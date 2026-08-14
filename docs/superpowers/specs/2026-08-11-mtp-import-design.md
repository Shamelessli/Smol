# 安卓设备（MTP）文件压缩支持

## 背景

用户通过数据线连接安卓设备（MTP 协议）后，无法压缩设备上的文件——文件**根本不进入队列**。

根因：MTP 是协议而非真实文件系统。Windows 对 MTP 设备呈现的是 Shell 命名空间虚拟路径（`::{GUID}\...`），`std::fs`（`get_path_info`、ffprobe、ffmpeg、Ghostscript、原生编码器）无法访问这种路径。当前 `get_path_info` 对 MTP 路径返回 `exists:false`，`useDragDrop.ts:19` 与 `Dropzone.tsx:45` 静默跳过文件，所以两种入口（拖拽/文件对话框）都看不到文件。

目标：让用户**像处理本地文件一样**处理安卓设备文件——拖入/选择设备文件 → 自动压缩 → 压缩结果**按输出模式写回设备**。

## 需求

1. 检测到非文件系统路径（`get_path_info` 返回不存在）时，用 Windows Shell API 把文件**自动导入**到本地工作区再压缩，交互上无需用户手动复制。
2. 压缩结果**写回设备**，写回位置按现有输出模式（outputMode）区分：same-folder / subfolder / custom / replace。
3. 压缩成功后清理本地副本；写回失败时保留本地结果并提示。
4. 导入失败（如设备断开）时给出清晰提示。
5. 导入前预检目标盘剩余空间，磁盘不足时**复用 `disk_full_hint` 友好提示**（与 2026-08-11-disk-full-hint 功能强协同，避免只覆盖编码不覆盖导入）。
6. 每个导入占用**唯一工作区子目录**，避免同名文件互相覆盖。
7. 启动时清理工作区残留（崩溃产生的中间文件）。

## 前置 spike：真机路径格式验证（必须先行）

**头号风险**：Tauri `onDragDropEvent` 与 rfd 对话框对 MTP 交付的路径字符串格式未经验证。若该字符串无法被 `SHParseDisplayName` 解析，整条导入链路失效——"失败引导提示"只是兜底，不是功能。

spike 内容（在实现主任务前完成）：
1. 用安卓真机 + 数据线连接。
2. 分别从**拖拽**和**文件对话框**添加一个设备文件，`console.log` Tauri 交付的原始路径字符串。
3. **对话框/目录选择器对 MTP 的可见性**（一并验证）：tauri-plugin-dialog（rfd，Windows IFileDialog）的 picker 里能否导航到"此电脑 → 安卓设备"并选中文件/目录？`open({ directory: true })` 能否选中 MTP 目录？（决定 custom 模式从源头是否可行）
4. 手工用 `SHParseDisplayName`（可在 Rust 加临时诊断命令或临时测试）验证能否解析出 `IShellItem`。
5. 结论写入 spec/plan：
   - 若可解析 → 按本设计实现。
   - 若不可解析但能拿到可复制信息 → 改用**替代方案**（见下文"方案 B"）。
   - 若完全不可行 → 冻结为"仅引导提示"。
   - **custom 模式三态结论**：若 `open({ directory: true })` 选不到 MTP 目录 → custom 模式对设备文件给 toast「无法选择设备目录」并按 **same-folder 行为处理**（或要求用户选本地目录）。spike 必须明确此回退，否则 custom 从源头不可用。可选另一方案：custom 模式对设备文件仅允许本地目录。

### 替代方案 B（spike 失败时的回退）

若 MTP 路径无法被 Shell API 解析，则无法自动导入。此时退化为：`get_path_info` 返回不存在的路径 → 统一 toast「无法访问该文件。若是安卓设备连接，请先将文件复制到本地后再拖入」。仍保证用户不会静默丢文件。

## 整体流程

```
MTP 原文件 → Shell API 导入本地工作区（唯一子目录） → 本地压缩 → 按输出模式写回设备 → 清理本地副本
```

## 输出模式 → 设备侧行为映射

| 输出模式 | 设备侧行为 |
|---|---|
| same-folder | 写回原文件同目录，新文件 `{name}_smol{ext}`（按文件名模式），设备原文件保留 |
| subfolder | 写回原目录下的 `smol/` 子目录（后端自动创建；创建失败回退原目录并提示） |
| custom | 写到用户选择的目录（`deliver_output` 内部自动区分本地/设备目录） |
| replace | 用压缩结果替换设备原文件。三步入队、一次 `PerformOperations`：`CopyItem` 临时名（`{stem}.{uuid8}.smol_tmp{ext}`，**uuid8 防上次失败残留同名触发自动改名**）→ `DeleteItem` 原文件 → `RenameItem` 临时名→原文件名 |

## 清理规则（压缩成功后）

- 本地导入副本（`工作区\{uuid}\{name}{ext}`）→ 删除
- 本地压缩输出（`工作区\{uuid}\{模式派生名}`）：
  - 写回成功 → 删除（结果已在设备）
  - 写回失败 → **保留**本地输出，toast「已压缩，但写回设备失败，结果保存在本地」
- 已最优（outputLarger）→ 设备原文件保留，删除本地导入副本（设备已有原文件）
- **replace 的命名冲突（关键）**：MTP 设备无回收站，不能像本地 `replace_original`（fs_bridge.rs:168）那样"先回收原文件"。但"直接 CopyItem 用原名到同目录"会触发 Windows **同名自动改名**——设备上已有 `IMG_123.jpg`，CopyItem 会把它写成 `IMG_123 (2).jpg` 或弹覆盖确认，随后删除原文件后用户得到的是错名文件。因此 replace 必须用**三步入队、一次 `PerformOperations`**：`CopyItem`（临时名 `{stem}.{uuid8}.smol_tmp{ext}`）→ `DeleteItem`（原文件）→ `RenameItem`（临时名 → 原文件名）。全程一个操作集，语义更原子。**临时名含 uuid8**：若上次 replace 中途失败在设备残留同名临时文件，本次 CopyItem 不会撞名（无残留顾虑）。
- **replace 部分失败中间态**：三步入队任一步失败 → 操作集整体回滚语义不保证（IFileOperation 部分成功可能残留临时文件或原文件），toast 说明实际状态（如「写回失败，临时文件与原件并存于设备，请检查」），**不丢数据**。
- **启动 GC**：App 启动时清理工作区**早于 N 分钟（默认 60）的子目录**（崩溃残留的中间文件）。不用"清空全部"——避免多实例冷启时清掉另一实例在途的 `{uuid}\` 导入副本。

## 失败处理

- 导入失败 → toast「无法访问该文件。若是安卓设备连接，请先将文件复制到本地」
- 导入时磁盘不足 → 复用 `disk_full_hint` 提示
- 写回失败 → 保留本地结果 + 提示

## 后端设计（新模块 `src-tauri/src/import.rs`）

### 新依赖

- `windows` crate，锁定 feature 集（避免拉入无关巨大体积）：
  ```toml
  windows = { version = "0.61", features = [
      "Win32_Foundation",
      "Win32_System_Com",
      "Win32_UI_Shell",
      "Win32_Storage_FileSystem",
  ] }
  ```
  （Cargo.lock 已含传递依赖 windows 0.61，直接声明只增加 feature，不引入新版本。读 `System.Size` 走 `IShellItem2::GetPropertyStore` → `IPropertyStore::GetValue`，子 feature 常被拆分——若编译报 `IPropertyStore`/`SHParseDisplayName` 缺失，追加 `Win32_UI_Shell_PropertiesSystem`。）

### COM 线程模型（关键，易踩坑）

Tauri 异步命令运行在 Tokio 工作线程上，**未初始化 COM**；直接调 `SHParseDisplayName`/`IFileOperation` 会得到 `CO_E_NOTINITIALIZED`。且 `IFileOperation::CopyItem`/`DeleteItem`/`RenameItem` 只是**入队**，必须再调 `PerformOperations` 才真正执行。

所有 COM 操作统一模式：
1. 每个命令内用 `tauri::async_runtime::spawn_blocking` 包住**全部** COM 调用（阻塞线程上执行）。
2. 阻塞闭包开头 `CoInitializeEx(null, COINIT_APARTMENTTHREADED)`（STA）；结尾 `CoUninitialize()`。
3. 入队（`CopyItem`/`NewItem`/`DeleteItem`/`RenameItem`）后必须调用 `PerformOperations()`，检查其 HRESULT 返回。

**`deliver_output` 的闭包边界**：整条交付（复制 + replace 的删除/改名）必须在**单个 `spawn_blocking` 闭包**内完成——CoInit/CoUninit 只做一次，且 replace 的三步（`CopyItem` 临时名 → `DeleteItem` 原文件 → `RenameItem`）入队到**同一个 `IFileOperation` 操作集**，一次 `PerformOperations` 执行，语义更原子、更快。

### 命令

公共命令（前端调用）：

1. `ensure_import_workspace() -> String` — 通过 `SHGetKnownFolderPath`（FOLDERID_Documents）取真实"文档"目录，创建并返回 `文档\Smol\imports`；同时**执行启动 GC**：清理 mtime 早于 60 分钟的子目录（多实例安全，见"清理规则"）。
2. `import_shell_item(display_path, workspace) -> PathInfo` — 导入到**唯一子目录** `{workspace}\{uuid}\`：
   - 磁盘预检：`GetDiskFreeSpaceW`（目标盘 = Documents 所在盘，已列入 feature）取 `free_bytes`；`IShellItem` 属性读取 `System.Size` 得 `needed_bytes`。
   - **大小拿不到的兜底**：`System.Size` 在某些 MTP 设备读不到（隐藏文件/权限）时，`needed_bytes` 取 0 → **跳过预检**（不阻塞导入），复制失败再由 `disk_full_hint` 兜底。已明确，不猜默认行为。
   - 不足 → 返回磁盘不足错误，前端复用 `disk_full_hint` 文案。
   - `SHParseDisplayName` 解析 MTP 路径 → `IFileOperation::CopyItem` 到 `{workspace}\{uuid}` → `PerformOperations` → 返回本地副本 `PathInfo`。
3. `deliver_output(local_path, mode, import_source_path, custom_output_dir, new_name, original_name) -> DeliverResult` — 压缩结果交付到设备，**单个 `spawn_blocking` 闭包内完成**。内部：
   1. **入口硬卡（对齐 fs_bridge.rs:158，仅 replace 模式）**：仅当 `mode == replace` 时要求 `local_path` 大小严格小于 `import_source_path` 原文件大小，否则拒绝。**非替换模式（same-folder/subfolder/custom）原文件保留不动，允许相等/略大的输出**（与现有非导入路径的 `outputLarger` 语义一致），硬卡不适用于它们。
   2. 调纯函数 `resolve_device_target(...)` 得目标（见下）。
   3. 低层复制（本地目录 → `std::fs::copy`；shell 目录 → `SHParseDisplayName` + `IFileOperation::CopyItem`；`dest_folder` 不存在自动创建，subfolder 建目录失败回退原目录）。
   4. `replace` 模式：三步入队到**同一 IFileOperation**——`CopyItem`（临时名 `{stem}.{salt}.smol_tmp{ext}`，salt=8 位短 uuid）→ `DeleteItem`（原文件）→ `RenameItem`（临时名→原文件名）→ 一次 `PerformOperations`。
   5. 返回 `DeliverResult { note: Option<String> }`（note 如「设备上无法创建 smol 子目录，已写入原目录」）。
4. `delete_local_file(path) -> ()` — 清理本地副本。

内部辅助（不暴露为 Tauri 命令）：`copy_to_shell(...)`、`delete_shell_item(...)`（含 COM 线程模型封装）。

### 可测纯逻辑（提取为纯函数 + `cargo test`）

- `resolve_device_target(mode, import_source_path, custom_output_dir, new_name, original_name, salt) -> DeviceTarget` — 模式 → 设备目标的纯映射（主要测试面）：
  ```rust
  struct DeviceTarget {
      dest_folder: String,    // 目标目录
      name: String,           // 写入文件名（replace 为临时名 {stem}.{salt}.smol_tmp{ext}）
      create_folder: bool,    // subfolder 需建 smol/，失败回退
      replace_original: bool, // replace 模式（驱动后续 DeleteItem+RenameItem 两步）
      rename_from: String,    // replace 时临时名 → 原文件名；否则空
  }
  ```
  - same-folder → dest=父目录，name=new_name，create=false，replace=false
  - subfolder → dest=父目录`\smol`，name=new_name，create=true，replace=false
  - custom → dest=custom_output_dir，name=new_name，create=false，replace=false
  - replace → dest=父目录，name=`{stem}.{salt}.smol_tmp{ext}`，create=false，replace=true，rename_from=original_name
  - `salt` 由 `deliver_output` 传入（8 位短 uuid），保证临时名唯一、无残留撞名。`resolve_device_target` 保持纯函数（salt 作为入参），仍可 `cargo test`。
- `compute_import_dir(workspace, uuid) -> String` — 唯一子目录拼接。
- `has_enough_space(free_bytes, needed_bytes) -> bool` — 磁盘预检判定（`needed_bytes == 0` → 返回 true，即跳过预检）。
- `disk_full_hint` — 复用已有（error.rs，已有测试）。

### 枚举 serde 约定（易漏）

`deliver_output(mode)` 收到的是前端 kebab-case 字符串（`"same-folder"`/`"subfolder"`/`"custom"`/`"replace"`）。仓库惯例是 `#[serde(rename_all = "camelCase")]`，但本枚举值是小写连字符——**必须显式 `#[serde(rename_all = "kebab-case")]`**，否则反序列化全部失败。

### 注册

在 `src-tauri/src/lib.rs` 的 `invoke_handler` 注册 4 个公共命令。

### 技术风险与缓解

- 路径字符串格式 + 对话框/目录选择器 MTP 可见性 → 前置 spike（见上文）。
- 大文件复制占用 → 导入前 free-space 预检（`GetDiskFreeSpaceW` + `System.Size`，大小拿不到跳过）+ 复用 `disk_full_hint`。
- 同名覆盖 → 唯一子目录 `{uuid}`。
- 崩溃残留 → 启动 GC（mtime 早于 60 分钟，多实例安全）。
- replace 命名冲突 → 三步入队 + 一次 `PerformOperations`（临时名复制 → 删原文件 → 改名）。
- replace/交付部分失败 → 中间态 toast（不丢数据）。
- COM 未初始化 / CopyItem 只入队 → `spawn_blocking` + `CoInitializeEx(STA)` + `PerformOperations`。
- `deliver_output` 缺少大小硬卡 → 入口校验 `local_path` 严格小于原文件。

## 前端设计

### 类型（`src/types/index.ts`）

Job 接口新增：

```ts
imported?: boolean;           // 是否从设备导入的本地副本
importSourcePath?: string;    // 设备上的原始路径，用于计算写回目标
```

### Tauri 包装器（`src/lib/tauri.ts`）

新增 `ensureImportWorkspace`、`importShellItem`、`deliverOutput`、`deleteLocalFile` 四个 invoke 包装器，与 Rust 公共命令一一对应。

### 导入 helper（新文件 `src/lib/imports.ts`）

`importPathToWorkspace(path: string): Promise<NewJobInput | null>`：
1. 调 `ensureImportWorkspace()` 拿工作区目录。
2. 调 `importShellItem(path, workspace)`。
3. 成功 → 返回 Job 数据（`inputPath`=本地副本路径、`name`、`kind`、`inputBytes`、`imported=true`、`importSourcePath=path`）。
4. 失败 → toast 引导提示，返回 `null`。

### 添加入口（`src/hooks/useDragDrop.ts` + `Dropzone.tsx`）

两处入口共用同一逻辑：`getPathInfo(path)` 返回存在 → 现有逻辑；不存在 → 调 `importPathToWorkspace(path)`，返回非 null 则 `addFiles([job])`。

### 压缩后处理（`src/hooks/useCompression.ts`，仅 `job.imported`）

**本地输出暂存**：imported Job 的压缩输出统一写到工作区子目录。**文件名派生**：
- 非 replace 模式：复用现有 `buildOutputPath` 的命名逻辑，仅取其派生出的**文件名**（`{pattern 展开}`，如 `IMG_123_smol.jpg`），目录替换为工作区子目录——即"模式派生名" = `buildOutputPath` 去掉目录部分。
- **replace 模式**：`buildOutputPath` 的类型不含 `"replace"`（签名是 `same-folder|subfolder|custom`），不能把 `outputMode="replace"` 传进去。改用 `buildReplaceIntermediatePath` 派生中间名（`{name}_smol{ext}`），目录替换为工作区 `{uuid}\`。

随后调 `deliverOutput(localOutput, mode, importSourcePath, customOutputDir, 模式派生名, 原文件名)`：

- 模式→设备目标映射、subfolder 建目录回退、replace 先复制后删原文件，**全部在 Rust `deliver_output` 内部完成**。
- 返回的 `note` 非空时 toast 提示（如「无法在设备创建 smol 子目录，已写入原目录」）。
- 成功 → `deleteLocalFile(工作区副本)` + `deleteLocalFile(工作区输出)`
- 失败 → 保留本地输出，toast「已压缩，但写回设备失败，结果保存在本地」

已最优（`outputLarger`）→ 仅 `deleteLocalFile(工作区副本)`（设备原文件不动）。

### 说明

- 模式映射在 Rust 侧纯函数实现并有测试，前端不重复该逻辑。
- 压缩命令本身零改动。

## 测试

- **spike 先行**（真机路径格式验证，见"前置 spike"）。
- `cargo test`：`resolve_device_target`（4 模式映射 + replace 临时名/改名 + 父目录/子目录派生）、`compute_import_dir`、`has_enough_space`（含 `needed_bytes==0` 跳过）、`disk_full_hint`（已有）。
- `cargo check`：编译干净、无 warning。
- 手动（需安卓真机 + 数据线）：
  - 拖拽/对话框选择设备文件 → 自动导入、压缩、写回设备同目录新文件。
  - replace 模式 → 设备原文件被替换为同名压缩文件（**验证无 ` (2)` 后缀**）；模拟中途失败 → 临时文件与原件并存 + toast，不丢数据。
  - subfolder 模式 → 设备 `smol/` 子目录创建并写回；创建失败 → 回退原目录 + toast。
  - custom 模式 → 选设备目录写回；选本地目录本地输出。
  - 写回失败（压缩后拔线）→ 本地结果保留 + 提示。
  - 已最优文件 → 设备原文件不动，本地副本清理。
  - 设备断开时导入 → 引导提示。
  - 两个同名文件导入 → 各自独立子目录，不互相覆盖。
  - 重启 App → mtime 超 60 分钟的子目录被清理；新启动的第二个实例不清第一个实例在途副本。
  - 对话框 picker 能否浏览/选择 MTP 文件与目录（spike 第 3 步结论）。

## 范围外

- 不做设备目录浏览/缩略图（对话框与资源管理器已能浏览 MTP）。
- 不做"写回设备时改名"的交互 UI（沿用文件名模式）。
- 不处理 MTP 文件夹删除（仅文件）。
- **导入中途不可取消**：IFileOperation 复制大文件（如 50GB 4K 视频）可能耗时数分钟，本设计无取消语义（需 `FOFX_NOCANCELUI` 反向取消 + Tauri job 取消钩子，属于后续增强）。导入期间用户需等待。
