# 安卓设备（MTP）文件压缩支持

## 背景

用户通过数据线连接安卓设备（MTP 协议）后，无法压缩设备上的文件——文件**根本不进入队列**。

根因链（经真机 spike 验证）：
1. MTP 是协议而非真实文件系统，Windows 呈现的是 Shell 命名空间虚拟路径，`std::fs` 无法访问。
2. **拖拽入口永久不可用**：WebView2 无法接收 MTP 拖放（禁止光标，`onDragDropEvent` 不触发）。
3. **rfd 对话框不可用**：rfd 用 `IFileDialog` 但强制 `SIGDN_FILESYSPATH`，MTP 项目无文件系统路径 → 选中后返回 null。
4. **路径字符串不可逆解析**：自定义 `IFileOpenDialog` 能显示/选中 MTP 并返回 `SIGDN_DESKTOPABSOLUTEPARSING` 解析名，但该解析名（含 `\\?\usb#...` 原始设备路径）**无法被 `SHParseDisplayName` 重新解析**（文件与父文件夹均失败，0x80070057）。
5. **可靠机制 = PIDL**：Shell 的规范二进制标识符（`ITEMIDLIST`）不经字符串解析，可跨线程用 `SHCreateItemFromIDList` 重建。

## 目标

让用户**像处理本地文件一样**处理安卓设备文件：自定义 shell 选择器选择设备文件 → 自动导入本地工作区 → 压缩 → 按输出模式写回设备。

## 需求

1. 用自定义 `IFileOpenDialog` 选择器（`pick_import`）替代 rfd，覆盖本地与 MTP 文件。
2. 拖拽入口仅支持本地文件（MTP 拖拽不可行，维持现状不新增）。
3. 压缩结果写回设备，位置按输出模式区分；**机制基于父文件夹 PIDL**（不依赖路径字符串解析）。
4. 压缩成功后清理本地副本；写回失败保留本地结果并提示。
5. 导入前预检磁盘空间，不足时复用 `disk_full_hint`。
6. 每导入唯一工作区子目录 `{uuid}\`；启动 GC 清 mtime>60min 子目录。

## 整体流程

```
自定义 picker 选中设备文件 → 会话内 CopyItem 导入工作区{uuid} + 捕获父文件夹 PIDL(base64)
→ 本地压缩 → deliver_output(父PIDL重建 → 按模式写回设备) → 清理本地副本
```

## 输出模式 → 设备侧行为映射

| 输出模式 | 行为 |
|---|---|
| same-folder | 写回父文件夹，新文件 `{name}_smol{ext}`（文件名模式），设备原文件保留 |
| subfolder | 写回父文件夹下 `smol/` 子目录（`NewItem` 创建；失败回退父文件夹并提示） |
| custom | **本地输出**（导入文件不入设备目录；设备目录本就无法通过目录选择器选中） |
| replace | 替换设备原文件：三步入队一次 `PerformOperations`：`CopyItem` 临时名（`{stem}.{salt}.smol_tmp{ext}`）→ `DeleteItem` 原文件 → `RenameItem` 临时名→原文件名（规避 Windows 同名自动改名） |

## 清理规则（压缩成功后）

- 本地导入副本（`工作区\{uuid}\{name}{ext}`）→ 删除
- 本地压缩输出（`工作区\{uuid}\{模式派生名}`）：写回成功删除；写回失败保留 + toast
- 已最优（outputLarger）→ 设备原文件保留，删除本地导入副本
- **replace 部分失败中间态**：临时名含 salt，残留不撞名；toast 说明实际状态，不丢数据
- **启动 GC**：mtime 早于 60 分钟的子目录（多实例安全）

## 失败处理

- 导入失败 → toast「无法访问该文件。若是安卓设备连接，请先将文件复制到本地」
- 导入时磁盘不足 → 复用 `disk_full_hint`
- 写回失败 → 保留本地结果 + 提示

## 后端设计（新模块 `src-tauri/src/import.rs`）

### 新依赖

```toml
windows = { version = "0.61", features = [
    "Win32_Foundation",
    "Win32_System_Com",
    "Win32_UI_Shell",
    "Win32_Storage_FileSystem",
] }
```
（读 `System.Size` 需要 `Win32_UI_Shell_PropertiesSystem` 时追加。）

### COM 线程模型（关键）

- Tauri 命令运行在 Tokio 线程上，未初始化 COM。
- 所有 COM 操作在命令内 `tauri::async_runtime::spawn_blocking` 闭包中执行：开头 `CoInitializeEx(null, COINIT_APARTMENTTHREADED)`，结尾 `CoUninitialize()`。
- `IFileOperation::CopyItem/DeleteItem/RenameItem` 只是入队，必须调 `PerformOperations()`。
- **PIDL 是纯字节数据**，可安全跨命令/跨线程保存与重建（这是本设计不依赖路径字符串的关键）。
- `deliver_output` 全程单个 `spawn_blocking` 闭包；replace 三步入队同一 `IFileOperation`。

### 命令

1. `ensure_import_workspace() -> String` — `SHGetKnownFolderPath`(FOLDERID_Documents) → `文档\Smol\imports`；创建并执行启动 GC（删 mtime>60min 子目录）。
2. `pick_import() -> Vec<PickResult>` — 打开 `IFileOpenDialog`（多选），对每个选中项：
   - `GetDisplayName(SIGDN_FILESYSPATH)` 成功 → 本地文件：返回 `{ is_local: true, path }`，前端按普通本地 job 处理。
   - 失败（MTP）→ 会话内 `CopyItem` 到 `{workspace}\{uuid}\`；`GetParent` 取父文件夹 → `SHGetIDListFromObject` 取 PIDL → 序列化字节 → base64；返回 `{ is_local: false, key: uuid, local_path, name, size, parent_id_list_b64 }`。
   - 磁盘预检：`GetDiskFreeSpaceExW` 取 `free_bytes`；`IShellItem2.GetProperty(System.Size)` 取 `needed_bytes`（失败取 0 跳过）；不足 → `disk_full_hint`。
   - `PickResult`：
     ```rust
     #[serde(rename_all = "camelCase")]
     pub struct PickResult {
         is_local: bool,
         path: Option<String>,        // 本地文件：文件系统路径
         key: Option<String>,         // MTP：uuid
         local_path: Option<String>,  // MTP：工作区副本路径
         name: Option<String>,
         size: Option<u64>,
         parent_id_list_b64: Option<String>,
     }
     ```
3. `deliver_output(local_path, key, mode, custom_output_dir, new_name, original_name) -> DeliverResult` — 压缩结果交付设备：
   1. `key` → 从工作区定位 uuid（也可由前端传 workspace 子目录名）；**父文件夹由 `parent_id_list_b64` 重建**：base64 解码 → `SHCreateItemFromIDList` → 父文件夹 `IShellItem`。
   2. 入口硬卡（**仅 replace 模式**）：`local_size < original_size`（`System.Size` 原文件；拿不到则拒绝）。
   3. `resolve_device_target(mode, parent_id_list_b64, custom_output_dir, new_name, original_name, salt)` 得目标。
   4. same-folder → `CopyItem(local, 父文件夹, name)`；subfolder → 父文件夹 `NewItem("smol")` 得子项 → `CopyItem`（失败回退父文件夹 + note）；replace → 三步入队一次 `PerformOperations`；custom → 本地 `std::fs::copy`。
   5. 返回 `DeliverResult { note: Option<String> }`。
4. `delete_local_file(path) -> ()` — 清理本地副本。

### 可测纯逻辑（`cargo test`）

- `resolve_device_target(mode, parent_available, custom_output_dir, new_name, original_name, salt) -> DeviceTarget` — 模式→目标映射：
  ```rust
  struct DeviceTarget {
      mode: Mode,               // SameFolder | Subfolder | Replace | CustomLocal
      name: String,             // 写入名（replace 为 {stem}.{salt}.smol_tmp{ext}）
      rename_from: String,      // replace 时临时名→原文件名
      create_subfolder: bool,   // subfolder
      note: Option<String>,     // 回退提示等
  }
  ```
  - same-folder → SameFolder，name=new_name
  - subfolder → Subfolder，name=new_name，create_subfolder=true
  - custom → CustomLocal，name=new_name
  - replace → Replace，name=`{stem}.{salt}.smol_tmp{ext}`，rename_from=original_name
- `compute_import_dir(workspace, uuid) -> String`
- `has_enough_space(free_bytes, needed_bytes) -> bool`（needed==0 → true）
- `pidl_to_base64(pidl) -> Vec<u8>` / `base64_to_pidl(bytes) -> ITEMIDLIST`（序列化往返，可测）
- `disk_full_hint`（复用已有）

### 注册

`lib.rs` 注册 `ensure_import_workspace`、`pick_import`、`deliver_output`、`delete_local_file`。

## 前端设计

### 类型（`src/types/index.ts`）

```ts
imported?: boolean;              // 是否从设备导入的本地副本
importParentIdListB64?: string;  // 父文件夹 PIDL base64（写回目标）
```

### Tauri 包装器（`src/lib/tauri.ts`）

`ensureImportWorkspace`、`pickImport(): Promise<PickResult[]>`、`deliverOutput(...)`、`deleteLocalFile(path)`。`PickResult` 接口镜像 Rust `camelCase`。

### 选择入口（`Dropzone.tsx` 的 Open files 改用 `pickImport`）

`handleOpenDialog` 改为：
1. 调 `pickImport()`。
2. 对每个结果：`is_local` → 普通本地 job（`getPathInfo` 已由 picker 保证为本地路径）；否则 → imported job（`inputPath=local_path`、`imported=true`、`importParentIdListB64=parent_id_list_b64`）。
3. `pickImport` 返回空/异常 → toast。

### 拖拽入口（`useDragDrop.ts`）

**不变**——仅本地文件。MTP 拖拽不可行，不做任何导入尝试。

### 压缩后处理（`useCompression.ts`，仅 `job.imported`）

- 本地输出暂存到工作区 `{uuid}\`，文件名按模式派生（`buildReplaceIntermediatePath`/模式名逻辑；imported+replace 用 `buildReplaceIntermediatePath` 派生名，目录替换为工作区 `{uuid}\`）。
- 已最优（outputLarger）→ 删本地副本。
- 否则调 `deliverOutput(staged, key, mode, customOutputDir, newName, originalName)`：
  - `note` → toast；成功 → 删工作区副本+输出；失败 → 保留本地输出 + toast。

### 说明

- 前端不触碰任何 shell 解析逻辑；写回目标由 Rust 用 PIDL 重建。
- 压缩命令零改动。

## 测试

- `cargo test`：`resolve_device_target`（4 模式 + replace 临时名 + subfolder 回退）、`compute_import_dir`、`has_enough_space`（needed==0 跳过）、`pidl_to_base64`/`base64_to_pidl` 往返、`disk_full_hint`。
- `cargo check` 干净。
- 手动（需安卓真机）：
  - `pickImport` 选设备文件 → 自动导入、压缩、写回同目录新文件；验证本地文件走普通路径。
  - replace → 设备原文件被同名替换（无 ` (2)` 后缀）。
  - subfolder → 设备 `smol/` 创建并写回；失败回退父文件夹 + toast。
  - custom → 本地输出。
  - 写回失败（压缩后拔线）→ 本地结果保留 + toast。
  - 已最优 → 设备不动，本地副本清理。
  - 两个同名文件 → 独立 `{uuid}` 子目录。
  - 重启 → mtime>60min 子目录清理；第二实例不清在途副本。

## 范围外

- 拖拽 MTP（WebView2 不可行）；设备目录浏览/缩略图；导入中途取消；MTP 文件夹删除。
