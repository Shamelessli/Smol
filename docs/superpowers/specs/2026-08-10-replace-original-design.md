# Replace Original — 压缩后替换原文件

## 背景

Smol 目前所有压缩都是**非破坏性**的：输出写入 `{name}_smol{ext}`（或 subfolder/custom 目录），原文件从不被改动。用户希望新增一种「压缩后直接替换原文件」的模式，同时保持已有的 already-optimal 检测——如果压缩结果不小于原文件，则原文件保持不动。

## 需求

1. 在「输出位置」下拉框中新增第三个选项 **Replace original**。
2. 选中该模式后，压缩成功的文件直接替换原文件（原文件移入系统回收站作为安全网）。
3. already-optimal 的文件（压缩结果 ≥ 原文件）完全不动，UI 继续显示 "Already optimal"。
4. 扩展名变化的格式（如 WAV→MP3、BMP→JPG）：保留新扩展名。原 `foo.wav` 移入回收站，压缩结果 `foo.mp3` 留在同一目录（同 stem 换扩展名）。

## 方案选型

- **方案 A（采用）**：新增独立 Rust 命令 `replace_original`，4 个既有压缩命令完全不动。前端在 replace 模式下先压缩到同目录中间文件，成功后调用 `replace_original` 完成「回收原文件 + 改名到最终路径」。already-optimal 分支天然由既有 `output_larger` 逻辑覆盖。
- **方案 B（弃用）**：给 4 个压缩命令加 `replace: bool` 参数。改动面大、扩展名逻辑重复，隔离性差。
- **方案 C（弃用）**：PowerShell 调用回收站。无新依赖但慢且 hacky。

## 后端设计

### 新依赖

- `trash` crate（Windows 上通过 COM 调用系统回收站，跨平台、轻量、维护良好）。

### 新 Tauri 命令 `replace_original`

实现位置：`src-tauri/src/fs_bridge.rs`（现有文件操作命令集中地）。

签名：`replace_original(compressed_path: String, original_path: String) -> Result<String, AppError>`

逻辑：

1. 校验 `compressed_path` 存在，且其大小严格小于 `original_path` 的大小（双保险，防止误删原文件）。
2. 计算最终路径：
   - 扩展名相同 → `original_path` 本身。
   - 扩展名不同 → `original_path` 换上新扩展名（同目录、同 stem）。例如 `D:\music\foo.wav` + 压缩结果 `foo_smol.mp3` → 最终 `D:\music\foo.mp3`。
3. 将 `original_path` 移入回收站（`trash::delete`）。**失败则返回错误并中止**——原文件仍保留，压缩结果（中间文件）也保留，UI 走 `setJobError`。
4. 将 `compressed_path` 重命名/移动到最终路径（复用既有 retry-on-sharing-violation 模式，错误码 32）。
5. 返回最终路径字符串。

### 命令注册

在 `src-tauri/src/lib.rs` 的 `invoke_handler` 中注册 `replace_original`。

## 前端设计

### 类型与设置

- `src/types/index.ts`：`Settings.outputMode` 扩展为 `"same-folder" | "subfolder" | "custom" | "replace"`。
- `src/store/settings.ts`：默认值不变，无需改逻辑。

### 输出控制 UI（`src/components/settings/OutputControls.tsx`）

- 下拉框 `modes` 数组增加 `{ id: "replace", label: "Replace original" }`。
- `outputMode === "replace"` 时：
  - 文件名模式输入框置灰并禁用（replace 模式忽略 pattern）。
  - 显示警告文案「Original files are moved to Recycle Bin」。

### 压缩流程（`src/hooks/useCompression.ts`）

在 `startSqueeze` 中，`outputMode === "replace"` 时：

1. `outputPath` 不经过 `buildOutputPath`，改为同目录中间文件：
   `{dir}/{stem}_smol{ext}`（与输入路径必然不同，绕开既有 `input == output` 拦截）。
   中间文件名与用户 pattern 无关。
2. 调用既有 `compressVideo/compressAudio/compressImage/compressPdf`，参数与现在完全一致。
3. 处理结果：
   - `result.outputLarger === true`（已最优）→ `setJobOutput(inputPath, inputBytes)`，原文件不动。UI 现有 DoneCard 逻辑自动显示 "Already optimal"。
   - 否则调用 `replace_original(result.outputPath, job.inputPath)`：
     - 成功 → `setJobOutput(最终路径, result.outputBytes)`。
     - 失败 → `setJobError(id, message)`。原文件与压缩结果均保留，用户可手动处理。

### 结果卡（`src/components/filelist/DoneCard.tsx`）

- 图片 Before/After 预览按钮当前条件为 `job.kind === "image" && job.outputPath && !outputLarger`。替换模式下 `outputPath === inputPath`（同扩展名时），前后对比无意义。改为额外要求 `job.outputPath !== job.inputPath`。

## 错误处理

- 回收站移动失败：中止，返回错误，原文件与压缩结果都保留。
- 重命名失败（共享冲突等）：复用既有错误码 32 重试模式；最终失败则返回错误。
- 中间文件：替换成功后不再残留（被改名到最终路径）；失败时保留以便排查。

## 测试

- 手动：视频/音频/图片/PDF 各测一次 replace 模式，验证原文件进回收站、结果落到原路径。
- 手动：已最优文件（如已高压缩的图片）验证原文件不动、显示 "Already optimal"。
- 手动：WAV→MP3 验证新扩展名保留、文件名同 stem。
- 手动：取消替换（回收站失败路径）不破坏原文件。

## 范围外

- 不改变非 replace 模式的任何行为。
- 不做备份文件（用户已选择回收站作为安全网）。
- 不处理 per-file override 中的输出模式（override 目前不含 outputMode）。
