# Disk-Full Hint Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show a friendly "not enough disk space" error on a job row when compression failed because the disk ran out of space, instead of the opaque `FFmpeg: Conversion failed!`.

**Architecture:** A pure Rust classifier `disk_full_hint(&str) -> Option<&'static str>` in `src-tauri/src/error.rs` scans an error/stderr string for disk-full signatures (English + Chinese, case-insensitive). It is applied at the failure paths of the four compress commands, which currently surface only the last line of stderr and hide the real cause. Frontend is unchanged.

**Tech Stack:** Rust (Tauri 2), `thiserror` AppError.

## Global Constraints

- Windows 10/11 64-bit is the only target.
- Do NOT change behavior for non-disk-full failures — existing messages (`FFmpeg: …`, `Ghostscript: …`, `IO error: …`) stay exactly as they are.
- Friendly hint text (verbatim): `Not enough disk space to write the output. Free up space and retry.`
- Detection patterns (verbatim, all matched case-insensitively via `to_lowercase`): `no space left on device`, `not enough space`, `insufficient space`, `error writing file`, `error writing output`, `failed to write`, `disk full`, `enospc`, `空间不足`.
- Rust logic is covered by `cargo test` in `src-tauri`; the frontend has no test framework (no frontend changes in this plan).

---

### Task 1: `disk_full_hint` classifier + unit tests

**Files:**
- Modify: `src-tauri/src/error.rs` (add function + `#[cfg(test)]` module)

**Interfaces:**
- Produces: `pub fn disk_full_hint(err: &str) -> Option<&'static str>` in `crate::error`. Returns `Some(HINT)` when `err` matches a disk-full signature, else `None`. Consumed by Task 2.

- [ ] **Step 1: Write the failing tests**

Append to the end of `src-tauri/src/error.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::disk_full_hint;

    const HINT: &str = "Not enough disk space to write the output. Free up space and retry.";

    #[test]
    fn detects_english_no_space_left() {
        assert_eq!(
            disk_full_hint("error writing output file: No space left on device"),
            Some(HINT)
        );
    }

    #[test]
    fn detection_is_case_insensitive() {
        assert_eq!(disk_full_hint("NO SPACE LEFT ON DEVICE"), Some(HINT));
    }

    #[test]
    fn detects_chinese_disk_full() {
        assert_eq!(disk_full_hint("写入失败：磁盘空间不足"), Some(HINT));
    }

    #[test]
    fn unrelated_error_returns_none() {
        assert_eq!(disk_full_hint("Invalid data found when processing input"), None);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run (working directory `G:\document\smol\src-tauri`): `cargo test disk_full_hint`
Expected: FAIL — `disk_full_hint` is not defined (cannot find function `disk_full_hint` in this scope). This is the expected RED: the classifier does not exist yet.

- [ ] **Step 3: Write the implementation**

Add `disk_full_hint` to `src-tauri/src/error.rs` (place it after the `From<std::io::Error>` impl, before the `#[cfg(test)]` module):

```rust
/// Return a friendly hint when an error string looks like a disk-full failure.
pub fn disk_full_hint(err: &str) -> Option<&'static str> {
    const HINT: &str = "Not enough disk space to write the output. Free up space and retry.";
    const PATTERNS: &[&str] = &[
        "no space left on device",
        "not enough space",
        "insufficient space",
        "error writing file",
        "error writing output",
        "failed to write",
        "disk full",
        "enospc",
        "空间不足",
    ];
    let lower = err.to_lowercase();
    PATTERNS.iter().any(|p| lower.contains(p)).then_some(HINT)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run (working directory `G:\document\smol\src-tauri`): `cargo test disk_full_hint`
Expected: PASS — 4 tests pass, 0 failed, output pristine.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/error.rs
git commit -m "feat: add disk_full_hint error classifier with tests"
```

---

### Task 2: Apply the hint at the four compress failure paths

**Files:**
- Modify: `src-tauri/src/commands/compress_video.rs:219-229` (failure branch)
- Modify: `src-tauri/src/commands/compress_audio.rs:182-191` (failure branch)
- Modify: `src-tauri/src/commands/compress_pdf.rs:137-146` (failure branch)
- Modify: `src-tauri/src/commands/compress_image.rs:179` (`std::fs::write` error)

**Interfaces:**
- Consumes: `crate::error::disk_full_hint` from Task 1.
- Produces: nothing new; the four commands surface the friendly hint on disk-full failures.

- [ ] **Step 1: Wire the hint into `compress_video.rs`**

In `src-tauri/src/commands/compress_video.rs`, the failure branch currently reads:

```rust
    if !status.success() {
        let _ = std::fs::remove_file(&temp_path);
        let msg = if stderr_output.is_empty() {
            "Compression failed or was cancelled".into()
        } else {
            // Surface the last line of FFmpeg's stderr as the error message
            let last = stderr_output.lines().last().unwrap_or(&stderr_output);
            format!("FFmpeg: {last}")
        };
        return Err(AppError::Other(msg));
    }
```

Change it to insert the disk-full check right after `remove_file`:

```rust
    if !status.success() {
        let _ = std::fs::remove_file(&temp_path);
        if let Some(hint) = crate::error::disk_full_hint(&stderr_output) {
            return Err(AppError::Other(hint.into()));
        }
        let msg = if stderr_output.is_empty() {
            "Compression failed or was cancelled".into()
        } else {
            // Surface the last line of FFmpeg's stderr as the error message
            let last = stderr_output.lines().last().unwrap_or(&stderr_output);
            format!("FFmpeg: {last}")
        };
        return Err(AppError::Other(msg));
    }
```

- [ ] **Step 2: Wire the hint into `compress_audio.rs`**

In `src-tauri/src/commands/compress_audio.rs`, the failure branch currently reads:

```rust
    if !status.success() {
        let _ = std::fs::remove_file(&temp_path);
        let msg = if stderr_output.is_empty() {
            "Compression failed or was cancelled".into()
        } else {
            let last = stderr_output.lines().last().unwrap_or(&stderr_output);
            format!("FFmpeg: {last}")
        };
        return Err(AppError::Other(msg));
    }
```

Change it to:

```rust
    if !status.success() {
        let _ = std::fs::remove_file(&temp_path);
        if let Some(hint) = crate::error::disk_full_hint(&stderr_output) {
            return Err(AppError::Other(hint.into()));
        }
        let msg = if stderr_output.is_empty() {
            "Compression failed or was cancelled".into()
        } else {
            let last = stderr_output.lines().last().unwrap_or(&stderr_output);
            format!("FFmpeg: {last}")
        };
        return Err(AppError::Other(msg));
    }
```

- [ ] **Step 3: Wire the hint into `compress_pdf.rs`**

In `src-tauri/src/commands/compress_pdf.rs`, the failure branch currently reads:

```rust
    if !status.success() {
        let _ = std::fs::remove_file(&temp_path);
        let msg = if stderr_output.is_empty() {
            "PDF compression failed or was cancelled".into()
        } else {
            let last = stderr_output.lines().last().unwrap_or(&stderr_output);
            format!("Ghostscript: {last}")
        };
        return Err(AppError::Other(msg));
    }
```

Change it to:

```rust
    if !status.success() {
        let _ = std::fs::remove_file(&temp_path);
        if let Some(hint) = crate::error::disk_full_hint(&stderr_output) {
            return Err(AppError::Other(hint.into()));
        }
        let msg = if stderr_output.is_empty() {
            "PDF compression failed or was cancelled".into()
        } else {
            let last = stderr_output.lines().last().unwrap_or(&stderr_output);
            format!("Ghostscript: {last}")
        };
        return Err(AppError::Other(msg));
    }
```

- [ ] **Step 4: Wire the hint into `compress_image.rs`**

In `src-tauri/src/commands/compress_image.rs`, the write currently reads (line 179):

```rust
    std::fs::write(&temp_path, &compressed)?;
```

Change it to:

```rust
    std::fs::write(&temp_path, &compressed).map_err(|e| {
        crate::error::disk_full_hint(&e.to_string())
            .map(|h| AppError::Other(h.into()))
            .unwrap_or_else(|| AppError::Io(e.to_string()))
    })?;
```

- [ ] **Step 5: Verify the full Rust suite**

Run (working directory `G:\document\smol\src-tauri`): `cargo test`
Expected: PASS — the 4 `disk_full_hint` tests plus the 3 `compute_replace_target` tests pass, 0 failed.

Run (working directory `G:\document\smol\src-tauri`): `cargo check`
Expected: clean, no warnings or errors.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/commands/compress_video.rs src-tauri/src/commands/compress_audio.rs src-tauri/src/commands/compress_pdf.rs src-tauri/src/commands/compress_image.rs
git commit -m "feat: surface friendly disk-space hint on encode failures"
```

---

## Self-Review Notes

- **Spec coverage:** classifier + patterns + HINT text (Task 1); applied to video/audio/pdf (stderr branches) and image (`std::fs::write`) (Task 2); temp-file cleanup preserved (existing `remove_file` kept, hint inserted after it); non-disk-full behavior unchanged (hint check returns early only on match); unit tests for EN/CI/chinese/match+none cases (Task 1).
- **Placeholder scan:** all steps contain exact code, paths, and commands.
- **Type consistency:** `disk_full_hint(&str) -> Option<&'static str>` defined in Task 1 and used identically in all four call sites in Task 2.
