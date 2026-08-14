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

## 整体流程

```
MTP 原文件 → Shell API 导入本地工作区 → 本地压缩 → 按输出模式写回设备 → 清理本地副本
```

## 输出模式 → 设备侧行为映射

| 输出模式 | 设备侧行为 |
|---|---|
| same-folder | 写回原文件同目录，新文件 `{name}_smol{ext}`（按文件名模式），设备原文件保留 |
| subfolder | 写回原目录下的 `smol/` 子目录（后端自动创建；创建失败回退原目录并提示） |
| custom | 写到用户选择的目录（`copyToShell` 自动区分本地/设备目录） |
| replace | 用压缩结果替换设备原文件。安全顺序：先复制新文件到设备成功 → 再删除设备原文件 |

## 清理规则（压缩成功后）

- 本地导入副本（`工作区\{name}{ext}`）→ 删除
- 本地压缩输出：
  - 写回成功 → 删除（结果已在设备）
  - 写回失败 → **保留**本地输出，toast「已压缩，但写回设备失败，结果保存在本地」
- 已最优（outputLarger）→ 设备原文件保留，删除本地导入副本（设备已有原文件）

## 失败处理

- 导入失败 → toast「无法访问该文件。若是安卓设备连接，请先将文件复制到本地」
- 写回失败 → 保留本地结果 + 提示

## 后端设计（新模块 `src-tauri/src/import.rs`）

### 新依赖

- `windows` crate（Shell COM 互操作；Cargo.lock 已含传递依赖 windows 0.61，直接声明所需 feature）。

### 命令

1. `ensure_import_workspace() -> String` — 通过 `SHGetKnownFolderPath`（FOLDERID_Documents）取真实"文档"目录，创建并返回 `文档\Smol\imports`。
2. `import_shell_item(display_path, dest_dir) -> PathInfo` — `SHParseDisplayName` 解析 MTP 路径 → `IFileOperation::CopyItem` 复制到 `dest_dir` → 返回本地副本的 `PathInfo`。
3. `copy_to_shell(local_path, dest_folder, new_name) -> ()` — 把压缩结果交付到目标目录，**同一 API 同时支持本地目录与 shell/设备目录**：目标存在为真实本地目录 → `std::fs::copy`；否则尝试 `SHParseDisplayName` 解析为 shell 目录 → `IFileOperation::CopyItem`。`dest_folder` 不存在时自动创建（本地用 `std::fs::create_dir_all`，shell 用 `IFileOperation::NewItem`）；创建失败返回错误，由前端回退原目录。
4. `delete_shell_item(display_path) -> ()` — replace 模式删除设备原文件。
5. `delete_local_file(path) -> ()` — 清理本地副本。

### 注册

在 `src-tauri/src/lib.rs` 的 `invoke_handler` 注册上述命令。

### 技术风险声明

拖拽/对话框传给前端的 MTP 路径字符串格式未在真机验证。若 `SHParseDisplayName` 无法解析该字符串，`import_shell_item` 失败 → 前端显示引导提示（正好满足"导入失败提示"）。设计不依赖某个具体路径格式。

## 前端设计

### 类型（`src/types/index.ts`）

Job 接口新增：

```ts
imported?: boolean;           // 是否从设备导入的本地副本
importSourcePath?: string;    // 设备上的原始路径，用于计算写回目标
```

### Tauri 包装器（`src/lib/tauri.ts`）

新增 `ensureImportWorkspace`、`importShellItem`、`copyToShell`、`deleteShellItem`、`deleteLocalFile` 五个 invoke 包装器，与 Rust 命令一一对应。

### 导入 helper（新文件 `src/lib/imports.ts`）

`importPathToWorkspace(path: string): Promise<NewJobInput | null>`：
1. 调 `ensureImportWorkspace()` 拿工作区目录。
2. 调 `importShellItem(path, workspace)`。
3. 成功 → 返回 Job 数据（`inputPath`=本地副本路径、`name`、`kind`、`inputBytes`、`imported=true`、`importSourcePath=path`）。
4. 失败 → toast 引导提示，返回 `null`。

### 添加入口（`src/hooks/useDragDrop.ts` + `Dropzone.tsx`）

两处入口共用同一逻辑：`getPathInfo(path)` 返回存在 → 现有逻辑；不存在 → 调 `importPathToWorkspace(path)`，返回非 null 则 `addFiles([job])`。

### 压缩后处理（`src/hooks/useCompression.ts`，仅 `job.imported`）

**本地输出暂存**：imported Job 的压缩输出统一写到工作区，文件名由文件名模式派生（如 `IMG_123_smol.jpg`），避免 `buildOutputPath` 把输出定向到设备/不可写目录。随后按 outputMode 用 `copyToShell` 交付：

| 模式 | 交付目标（dest_folder） | 新文件名 |
|---|---|---|
| same-folder | `importSourcePath` 父目录 | 模式派生名 |
| subfolder | 父目录`\smol`（建目录失败回退父目录） | 模式派生名 |
| custom | `customOutputDir`（`copyToShell` 内部自动区分本地/设备目录） | 模式派生名 |
| replace | `importSourcePath` 父目录 | 原文件名；`copyToShell` 成功后 `deleteShellItem(importSourcePath)` |

写回成功 → `deleteLocalFile(工作区副本)` + `deleteLocalFile(工作区输出)`；写回失败 → 保留本地输出，toast 提示。

已最优（`outputLarger`）→ 仅 `deleteLocalFile(工作区副本)`（设备原文件不动）。

### 说明

- `copyToShell` 统一处理本地与设备目标，前端无需判断 `customOutputDir` 是本地还是设备路径。
- 压缩命令本身零改动。

## 测试

- `cargo test` / `cargo check`（新模块无纯逻辑单元测试则至少编译干净）。
- 手动（需安卓真机 + 数据线）：
  - 拖拽/对话框选择设备文件 → 自动导入、压缩、写回设备同目录新文件。
  - replace 模式 → 设备原文件被替换，原文件删除。
  - subfolder 模式 → 设备 `smol/` 子目录创建并写回。
  - 写回失败（压缩后拔线）→ 本地结果保留 + 提示。
  - 已最优文件 → 设备原文件不动，本地副本清理。
  - 设备断开时导入 → 引导提示。

## 范围外

- 不做设备目录浏览/缩略图（对话框与资源管理器已能浏览 MTP）。
- 不做"写回设备时改名"的交互 UI（沿用文件名模式）。
- 不处理 MTP 文件夹删除（仅文件）。
