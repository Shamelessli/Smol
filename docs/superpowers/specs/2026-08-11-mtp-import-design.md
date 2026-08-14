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
3. 手工用 `SHParseDisplayName`（可在 Rust 加临时诊断命令或临时测试）验证能否解析出 `IShellItem`。
4. 结论写入 spec/plan：
   - 若可解析 → 按本设计实现。
   - 若不可解析但能拿到可复制信息 → 改用**替代方案**（见下文"方案 B"）。
   - 若完全不可行 → 冻结为"仅引导提示"。

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
| replace | 用压缩结果替换设备原文件。安全顺序：先复制新文件到设备成功 → 再删除设备原文件 |

## 清理规则（压缩成功后）

- 本地导入副本（`工作区\{uuid}\{name}{ext}`）→ 删除
- 本地压缩输出（`工作区\{uuid}\{模式派生名}`）：
  - 写回成功 → 删除（结果已在设备）
  - 写回失败 → **保留**本地输出，toast「已压缩，但写回设备失败，结果保存在本地」
- 已最优（outputLarger）→ 设备原文件保留，删除本地导入副本（设备已有原文件）
- **replace 部分失败中间态**（设备无回收站，`delete_shell_item` 是硬删）：采用"先复制新文件成功 → 再删原文件"的安全顺序。若复制成功但删原文件失败 → 设备上新旧两份并存，**不丢数据**，toast 提示「压缩结果已写回设备，但原文件删除失败，请手动删除」。
- **启动 GC**：App 启动时清理工作区全部内容（崩溃残留的中间文件）。启动早于任何 job，安全。

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
  （Cargo.lock 已含传递依赖 windows 0.61，直接声明只增加 feature，不引入新版本。）

### COM 线程模型（关键，易踩坑）

Tauri 异步命令运行在 Tokio 工作线程上，**未初始化 COM**；直接调 `SHParseDisplayName`/`IFileOperation` 会得到 `CO_E_NOTINITIALIZED`。且 `IFileOperation::CopyItem` 只是**入队**，必须再调 `PerformOperations` 才真正执行。

所有 COM 操作统一模式：
1. 在命令内用 `tauri::async_runtime::spawn_blocking` 包住 COM 调用（阻塞线程上执行）。
2. 阻塞闭包开头 `CoInitializeEx(null, COINIT_APARTMENTTHREADED)`（STA）；结尾 `CoUninitialize()`。
3. `CopyItem`/`NewItem`/`DeleteItem` 之后必须调用 `PerformOperations()`，检查其 HRESULT 返回。

### 命令

公共命令（前端调用）：

1. `ensure_import_workspace() -> String` — 通过 `SHGetKnownFolderPath`（FOLDERID_Documents）取真实"文档"目录，创建并返回 `文档\Smol\imports`；同时**执行启动 GC**（清空工作区）。
2. `import_shell_item(display_path, workspace) -> PathInfo` — 导入到**唯一子目录** `{workspace}\{uuid}\`：
   - 先检查目标盘剩余空间 ≥ 所需（无直接 API 拿 MTP 文件大小，用 `IShellItem` 属性读取 `System.Size`；不足 → 返回磁盘不足错误，前端复用 `disk_full_hint` 文案）。
   - `SHParseDisplayName` 解析 MTP 路径 → `IFileOperation::CopyItem` 到 `{workspace}\{uuid}` → `PerformOperations` → 返回本地副本 `PathInfo`（`input_path`=唯一子目录下副本）。
3. `deliver_output(local_path, mode, import_source_path, custom_output_dir, new_name, original_name) -> DeliverResult` — 压缩结果交付到设备。内部：
   1. 调纯函数 `resolve_device_target(...)` 得目标（见下）。
   2. 低层复制（本地目录 → `std::fs::copy`；shell 目录 → `SHParseDisplayName` + `IFileOperation::CopyItem` + `PerformOperations`；`dest_folder` 不存在自动创建，subfolder 建目录失败回退原目录）。
   3. `replace` 模式在复制成功后 `delete_shell_item(import_source_path)`（`IFileOperation::DeleteItem` + `PerformOperations`）。
   4. 返回 `DeliverResult { note: Option<String> }`（note 如「设备上无法创建 smol 子目录，已写入原目录」）。
4. `delete_local_file(path) -> ()` — 清理本地副本。

内部辅助（不暴露为 Tauri 命令）：`copy_to_shell(...)`、`delete_shell_item(...)`（含 COM 线程模型封装）。

### 可测纯逻辑（提取为纯函数 + `cargo test`）

- `resolve_device_target(mode, import_source_path, custom_output_dir, new_name, original_name) -> DeviceTarget` — 模式 → 设备目标的纯映射（主要测试面）：
  ```rust
  struct DeviceTarget {
      dest_folder: String,   // 目标目录
      name: String,          // 写入文件名
      create_folder: bool,   // subfolder 需建 smol/，失败回退
      replace_original: bool,// replace 模式
  }
  ```
  - same-folder → dest=父目录，name=new_name，create=false，replace=false
  - subfolder → dest=父目录`\smol`，name=new_name，create=true，replace=false
  - custom → dest=custom_output_dir，name=new_name，create=false，replace=false
  - replace → dest=父目录，name=original_name，create=false，replace=true
- `compute_import_dir(workspace, uuid) -> String` — 唯一子目录拼接。
- `has_enough_space(free_bytes, needed_bytes) -> bool` — 磁盘预检判定。
- `disk_full_hint` — 复用已有（error.rs，已有测试）。

### 注册

在 `src-tauri/src/lib.rs` 的 `invoke_handler` 注册 4 个公共命令。

### 技术风险与缓解

- 路径字符串格式 → 前置 spike（见上文）。
- 大文件复制占用 → 导入前 free-space 预检 + 复用 `disk_full_hint`。
- 同名覆盖 → 唯一子目录 `{uuid}`。
- 崩溃残留 → 启动 GC。
- replace 部分失败 → "先复制后删除"+ 中间态 toast（不丢数据）。

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

**本地输出暂存**：imported Job 的压缩输出统一写到工作区子目录，文件名由文件名模式派生（如 `IMG_123_smol.jpg`），避免 `buildOutputPath` 把输出定向到设备/不可写目录。随后调 `deliverOutput(localOutput, mode, importSourcePath, customOutputDir, 模式派生名, 原文件名)`：

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
- `cargo test`：`resolve_device_target`（4 模式映射 + 父目录/子目录/shell 路径派生）、`compute_import_dir`、`has_enough_space`、`disk_full_hint`（已有）。
- `cargo check`：编译干净、无 warning。
- 手动（需安卓真机 + 数据线）：
  - 拖拽/对话框选择设备文件 → 自动导入、压缩、写回设备同目录新文件。
  - replace 模式 → 设备原文件被替换；模拟删原文件失败 → 新旧两份并存 + toast，不丢数据。
  - subfolder 模式 → 设备 `smol/` 子目录创建并写回；创建失败 → 回退原目录 + toast。
  - 写回失败（压缩后拔线）→ 本地结果保留 + 提示。
  - 已最优文件 → 设备原文件不动，本地副本清理。
  - 设备断开时导入 → 引导提示。
  - 两个同名文件导入 → 各自独立子目录，不互相覆盖。
  - 重启 App → 工作区残留被清理。

## 范围外

- 不做设备目录浏览/缩略图（对话框与资源管理器已能浏览 MTP）。
- 不做"写回设备时改名"的交互 UI（沿用文件名模式）。
- 不处理 MTP 文件夹删除（仅文件）。
