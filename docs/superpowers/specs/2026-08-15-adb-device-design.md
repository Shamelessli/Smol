# 安卓设备压缩支持（ADB 方案）

## 背景与废弃

此前的 MTP/Shell COM 方案经真机验证不可行：WebView2 无法接收 MTP 拖放、rfd 对 MTP 返回 null、MTP 解析名无法被 `SHParseDisplayName` 往返解析。该方案（`src-tauri/src/import.rs` 的 COM 命令、`pick_import`/`deliver_output`、PIDL 写回、自定义 `IFileOpenDialog` 选择器、`windows` 直接依赖）全部废弃，本文档重新设计为 **ADB（Android Debug Bridge）** 方案。

ADB 经 USB 调试通道工作，`adb pull`/`adb push` 按文件操作，稳定可靠，不依赖 Shell 命名空间。

## 需求

1. 设备需启用 **USB 调试** 并授权；未就绪时给出引导提示。
2. App 内置 **ADB 文件浏览器**（`adb shell` 枚举），支持常用目录快捷键（DCIM/Pictures/Download/Movies/Music）+ 目录导航 + 多选。
3. `adb pull` 将选中的设备文件拉取到本地工作区，走现有压缩流程。
4. 压缩结果按 **设备交付模式** 交付：
   - **PC 指定文件夹**：输出保存到用户选的本地目录。
   - **安卓指定文件夹**：`adb push` 到用户选的设备目录。
   - **替换安卓文件**：`adb push` 覆盖设备原文件路径。
5. 压缩成功后清理本地副本；交付失败保留本地结果并如实提示。
6. 随应用打包 `adb.exe`（fetch 脚本下载 platform-tools）。

## 整体流程

```
启用 USB 调试 → ADB 浏览器选设备文件 → adb pull 到工作区{uuid} → 本地压缩
→ 按交付模式交付（PC 目录 / adb push 到设备目录 / adb push 覆盖原文件）→ 清理本地副本
```

## 交付模式

| 模式 | 行为 |
|---|---|
| pc-folder | 输出复制到用户选的本地目录（交付成功即删工作区副本） |
| android-folder | `adb push` 到用户选的设备目录（`/sdcard/...` 路径，由浏览器选目录） |
| replace | `adb push` 覆盖设备原文件路径 |

- 设备 job 的交付模式通过**独立下拉框**选择（默认 `replace`），与本地文件的 outputMode 互不影响。
- 已最优（outputLarger）→ 设备原文件不动，删本地副本。

## 失败处理

- 无设备 / 未授权 / adb 缺失 → 浏览器与导入入口提示对应引导。
- `adb pull` 失败 → toast「拉取失败，请确认设备已连接并启用 USB 调试」。
- 交付失败 → 保留本地输出，toast 显示真实本地位置（不做 MTP 那种会过期的工作区残留，交付失败即复制到持久恢复目录 `文档\Smol\recovered\`）。

## 后端设计（新模块 `src-tauri/src/adb.rs`）

### adb 定位与打包

- `scripts/fetch-adb.mjs`（仿 `scripts/fetch-ffmpeg.mjs`）：下载 platform-tools zip，解压 `adb.exe` 到 `src-tauri/binaries/adb.exe`。
- `tauri.conf.json` `bundle.resources` 增加 `"binaries/adb.exe": "adb.exe"`。
- Rust 定位：优先环境变量/当前目录，其次 exe 同目录 `adb.exe`；找不到返回错误。

### 命令封装（`std::process::Command`，非 COM；Windows 上 `CREATE_NO_WINDOW`）

1. `adb_devices() -> Result<Vec<DeviceState>, AppError>` — 解析 `adb devices`：每行 `serial\tstate`（state ∈ device/offline/unauthorized）。无 device → 引导提示。
2. `adb_ls(dir: &str) -> Result<Vec<DirEntry>, AppError>` — `adb shell "ls -la <dir>"` 解析每行：权限位首字符 `d` → is_dir；最后一列文件名（去掉 `.`/`..`）；大小列解析为 size。**风险**：不同设备 `ls` 输出列数/对齐差异 → 容错解析（按空白切分，取最后段为文件名，倒数第二段为大小，首字符判断目录），解析失败返回空列表而非错误。空目录返回空。
3. `adb_pull(remote: &str, local: &Path) -> Result<(), AppError>` — `adb pull <remote> <local>`；成功条件 status.success。
4. `adb_push(local: &Path, remote: &str) -> Result<(), AppError>` — `adb push <local> <remote>`。

### 命令

1. `list_device_dir(path: String) -> Result<Vec<DeviceEntry>, AppError>` — 调 `adb_ls`；`DeviceEntry { name, is_dir, size }`（serde camelCase）。
2. `pull_device_files(items: Vec<String>, workspace: String) -> Result<Vec<PullResult>, AppError>` — 对每个远程路径：
   - 生成 `{workspace}\{uuid}\`，`adb pull` 到该目录。
   - 文件名取远程路径最后段。
   - 返回 `PullResult { key, local_path, name, size, remote_path }`（serde camelCase）。
3. `deliver_to_device(local_path, mode, remote_path, remote_dir, pc_dir, new_name) -> Result<DeliverResult, AppError>` — 按模式：
   - pc-folder → `std::fs::copy` 到 `pc_dir`。
   - android-folder → `adb_push(local, <remote_dir>/<new_name>)`（`remote_dir` 以 `/` 结尾则拼接）。
   - replace → `adb_push(local, remote_path)`（覆盖原文件）。
   - 失败 → 复制 local 到 `文档\Smol\recovered\{name}`，错误消息含恢复路径。
4. `delete_local_file(path)` — 清理（复用现有，若保留）。

### 移除项

- 删除 `src-tauri/src/import.rs`、`mod import;` 与 4 个命令注册。
- 移除 `Cargo.toml` 的 `windows` 直接依赖与 `base64`（如无其他使用）。
- `tauri.conf.json` 移除无 MTP 相关项（无新增项，未引入）。

## 前端设计

### 类型（`src/types/index.ts`）

```ts
deviceRemotePath?: string;           // 设备上的原始路径（/sdcard/...）
deviceDeliveryMode?: "pc-folder" | "android-folder" | "replace";  // 设备交付模式（job 级）
deviceRemoteDir?: string;            // android-folder 模式的目标设备目录
```

移除 MTP 字段 `imported`/`importParentIdListB64`（若无其他使用）。

### Tauri 包装器（`src/lib/tauri.ts`）

`listDeviceDir(path)`、`pullDeviceFiles(items, workspace)`、`deliverToDevice(...)`、`deleteLocalFile(path)`。移除 `pickImport`/`deliverOutput`/`ensureImportWorkspace`。

### ADB 文件浏览器（新组件 `src/components/device/DeviceBrowser.tsx`）

- 模态框：地址栏（当前设备目录，可编辑）+ 常用目录快捷键 + 文件/目录列表（目录可进入，文件多选）+ 底部「添加 N 个文件」。
- 调用 `listDeviceDir`；`adb` 错误 → 显示引导（USB 调试）。
- 入口：Dropzone 增加 **"Add from device"** 按钮；本地文件恢复 rfd `open()`。

### 添加流程（`useDragDrop` 不涉及；Dropzone 内新增）

`pullDeviceFiles(选中远程路径, workspace)` → 每个结果添加 job：`inputPath=local_path`、`deviceRemotePath=remote_path`、`deviceDeliveryMode=默认 replace`（用户可在队列行改）。

### 压缩后处理（`useCompression.ts`，仅 `job.deviceRemotePath`）

- 本地输出暂存到工作区 `{uuid}\`（模式派生名，复用 `stagingOutputPath` 思路）。
- 已最优 → 删本地副本。
- 否则按 `job.deviceDeliveryMode` 调 `deliverToDevice(staged, mode, remotePath, remoteDir, pcDir, newName)`；成功 → 删工作区副本+输出；失败 → toast 显示恢复路径。

### 交付模式 UI

- 队列行（JobRow）对设备 job 显示交付模式下拉框（3 项，写入 `deviceDeliveryMode`）。
- android-folder 模式需选设备目录 → 复用 DeviceBrowser 的目录选择。

## 打包与脚本

- `scripts/fetch-adb.mjs`：下载 `https://dl.google.com/android/repository/platform-tools-latest-windows.zip` → 解压 `platform-tools/adb.exe` → `src-tauri/binaries/adb.exe`。postinstall 追加调用（与 fetch-ffmpeg 并列）。
- `tauri.conf.json` resources 增加 `adb.exe`。

## 测试

- `cargo test`：`adb_ls` 行解析纯函数（多行样本含目录/文件/`.`/`..`/权限差异）可单元测试。
- `cargo check` / `pnpm build` 干净。
- 手动（真机 + USB 调试）：
  - 未授权 → 引导提示；授权后 → 浏览器可用。
  - 浏览器导航/常用目录/多选。
  - pull → 压缩 → 三种交付模式各测一遍（PC 目录 / 设备目录 / 替换原文件）。
  - 已最优 → 设备不动，本地清理。
  - 拔线交付失败 → 恢复路径存在 + toast 如实。
  - 两个同名文件 → 独立 uuid 目录。

## 范围外

- MTP/拖拽设备文件（不可行）。
- 设备目录删除/重命名。
- 断点续传/取消 adb 传输。
