# 磁盘空间不足友好提示

## 背景

用户报告压缩视频失败，错误显示为 `FFmpeg: Conversion failed!`。经排查，根因是**磁盘空间不足**：ffmpeg 写输出时 ENOSPC 失败。

现有问题：`compress_video.rs` / `compress_audio.rs` / `compress_pdf.rs` 捕获 ffmpeg/ghostscript 的 stderr（仅最后 512 字节），失败时只把**最后一行**（通常是 `Conversion failed!`）作为错误消息展示，把真正的错误原因（`No space left on device`）隐藏了。`compress_image.rs` 用原生 Rust 编码直接写盘，磁盘满时产生 `IO error: ... No space left on device ...`。

目标：失败时识别"磁盘空间不足"，在任务行显示友好提示。

## 需求

1. 编码失败时，若真实原因是磁盘空间不足，任务行错误消息显示为友好提示：
   `Not enough disk space to write the output. Free up space and retry.`
2. 其他失败原因保持现有行为不变。

## 方案选型

- **方案 A（采用）**：Rust 侧识别。新增纯函数扫描错误文本，命中磁盘不足特征（英文 + 中文）则返回友好提示；应用到 4 个压缩命令的失败路径。可加单元测试。
- **方案 B（弃用）**：前端 `extractErrorMessage` 识别。仅对 image（IO 错误文本携带特征）有效；video/audio/pdf 的 stderr 被截断成最后一行，前端看不到磁盘错误特征，不完整。
- **方案 C（弃用）**：A+B 混合，过度设计。

## 后端设计

### 新增纯函数（`src-tauri/src/error.rs`）

```rust
/// Return a friendly hint when an error string looks like a disk-full failure.
pub fn disk_full_hint(err: &str) -> Option<&'static str> {
    const HINT: &str = "Not enough disk space to write the output. Free up space and retry.";
    const PATTERNS: &[&str] = &[
        "no space left on device",
        "not enough space",
        "insufficient space",
        "disk full",
        "enospc",
        "空间不足",
    ];
    let lower = err.to_lowercase();
    PATTERNS.iter().any(|p| lower.contains(p)).then_some(HINT)
}
```

### 应用到 4 个命令

- `compress_video.rs`：`!status.success()` 分支，取到 `stderr_output` 后先检查：
  ```rust
  if let Some(hint) = crate::error::disk_full_hint(&stderr_output) {
      let _ = std::fs::remove_file(&temp_path);
      return Err(AppError::Other(hint.into()));
  }
  ```
  再走现有通用消息逻辑。
- `compress_audio.rs`：同视频，`!status.success()` 分支。
- `compress_pdf.rs`：同视频/音频（ghostscript stderr）。
- `compress_image.rs`：`std::fs::write(&temp_path, &compressed)?` 处改为手动 map 错误：
  ```rust
  std::fs::write(&temp_path, &compressed).map_err(|e| {
      crate::error::disk_full_hint(&e.to_string())
          .map(|h| AppError::Other(h.into()))
          .unwrap_or_else(|| AppError::Io(e.to_string()))
  })?;
  ```

### 单元测试（`error.rs` 内 `#[cfg(test)]`）

- 英文特征命中：`"error writing output file: No space left on device"` → Some(HINT)
- 大小写不敏感：`"NO SPACE LEFT ON DEVICE"` → Some(HINT)
- 中文特征命中：`"磁盘空间不足"` → Some(HINT)
- 不相关错误：`"Invalid data found when processing input"` → None

## 前端

无改动。

## 错误处理

- 磁盘满时删除临时文件（`.part`），原文件保持不动。
- 其他失败原因行为不变。

## 测试

- `cargo test`：`disk_full_hint` 单元测试通过。
- 手动：制造磁盘满场景（如把输出目录放到已满的小分区）压缩视频/音频/PDF/图片，任务行显示友好提示。
- 手动：正常失败（如损坏文件）仍显示原错误。

## 范围外

- 不做编码前剩余空间预检查（用户已选择失败时识别）。
- 不改前端错误展示逻辑。
- 不改错误消息语言体系（提示用英文，与 App 现有 UI 一致）。
