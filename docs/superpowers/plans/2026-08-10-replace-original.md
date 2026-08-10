# Replace Original — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a "Replace original" output mode so successfully compressed files replace their originals (originals go to the Recycle Bin) while already-optimal files are left untouched.

**Architecture:** A new standalone Rust Tauri command `replace_original` performs the destructive swap (Recycle Bin + rename). The four existing compress commands are unchanged. The frontend computes a transient intermediate path in replace mode, then calls `replace_original` after a successful, smaller compression; the already-optimal branch falls through to existing logic untouched.

**Tech Stack:** Rust (Tauri 2, `trash` crate), React 19 + TypeScript + Zustand.

## Global Constraints

- Windows 10/11 64-bit is the only target; tests run on Windows.
- Do NOT change behavior of the existing output modes (`same-folder` / `subfolder` / `custom`).
- `trash` crate version `5`.
- No frontend test framework exists — frontend verification is `pnpm lint` + `pnpm build` (tsc) + manual checklist; Rust logic is covered by `cargo test` in `src-tauri`.
- New code follows the existing file conventions (comment style of the file being edited, `@/` alias for frontend imports, `AppError` for Rust errors).

---

### Task 1: Rust `replace_original` command + unit tests

**Files:**
- Modify: `src-tauri/Cargo.toml` (add `trash` dependency)
- Modify: `src-tauri/src/fs_bridge.rs` (add `compute_replace_target` + `replace_original` + `#[cfg(test)]` module)
- Modify: `src-tauri/src/lib.rs` (register the command)

**Interfaces:**
- Produces: Tauri command `replace_original(compressed_path: String, original_path: String) -> Result<String, AppError>` returning the final path string. Helper `fn compute_replace_target(original_path: &str, compressed_path: &str) -> PathBuf` (pure, unit-tested).

- [ ] **Step 1: Add `trash` dependency**

In `src-tauri/Cargo.toml`, add to `[dependencies]` (alphabetical order near `thiserror`):

```toml
trash = "5"
```

- [ ] **Step 2: Add the command + tests to `fs_bridge.rs`**

Append to the end of `src-tauri/src/fs_bridge.rs`. Current top imports are `use serde::Serialize;` `use std::path::Path;` `use crate::error::AppError;` — extend the `use` block at the top of the file to:

```rust
use std::path::{Path, PathBuf};
```

Then append the following code at the end of the file:

```rust
/// Decide where the compressed file ends up after replacing the original.
/// Same extension (case-insensitive) → the original path itself;
/// different extension (e.g. WAV → mp3) → original stem + new extension.
fn compute_replace_target(original_path: &str, compressed_path: &str) -> PathBuf {
    let orig = Path::new(original_path);
    let comp = Path::new(compressed_path);
    let comp_ext = comp.extension().and_then(|e| e.to_str()).unwrap_or("");
    let orig_ext = orig.extension().and_then(|e| e.to_str()).unwrap_or("");
    if comp_ext.eq_ignore_ascii_case(orig_ext) {
        orig.to_path_buf()
    } else {
        orig.with_extension(comp_ext)
    }
}

/// Replace the original file with a compressed one.
/// Moves the original to the Recycle Bin (aborting on failure), then renames
/// `compressed_path` onto the original's path (same stem + new extension when
/// the container format changed). Returns the final output path.
/// Refuses when `compressed_path` is missing or not strictly smaller.
#[tauri::command]
pub async fn replace_original(
    compressed_path: String,
    original_path: String,
) -> Result<String, AppError> {
    let comp = Path::new(&compressed_path);
    let orig = Path::new(&original_path);

    let comp_size = std::fs::metadata(comp)
        .map_err(|_| AppError::PathDoesNotExist(compressed_path.clone()))?
        .len();
    let orig_size = std::fs::metadata(orig)
        .map_err(|_| AppError::PathDoesNotExist(original_path.clone()))?
        .len();

    if comp_size >= orig_size {
        return Err(AppError::Other(
            "Compressed file is not smaller than the original — refusing to replace".into(),
        ));
    }

    let target = compute_replace_target(&original_path, &compressed_path);

    // Safety net: the original always goes to the Recycle Bin, never a hard delete.
    // On failure we abort — nothing has been destroyed yet.
    trash::delete(orig)
        .map_err(|e| AppError::Other(format!("Failed to move original to Recycle Bin: {e}")))?;

    // Rename compressed → target. The target must not exist (we just recycled the
    // original for the same-ext case); remove any stale file just in case.
    if target.exists() {
        std::fs::remove_file(&target)?;
    }
    let mut retries = 5;
    loop {
        match std::fs::rename(comp, &target) {
            Ok(_) => break,
            Err(e) if e.raw_os_error() == Some(32) && retries > 0 => {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                retries -= 1;
            }
            Err(e) => return Err(AppError::Other(format!("Failed to move compressed file: {e}"))),
        }
    }

    Ok(target.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_replace_target_same_extension_returns_original() {
        let target = compute_replace_target(r"D:\music\song.mp4", r"D:\music\song_smol.mp4");
        assert_eq!(target.to_string_lossy(), r"D:\music\song.mp4");
    }

    #[test]
    fn compute_replace_target_different_extension_reuses_stem() {
        let target = compute_replace_target(r"D:\music\song.wav", r"D:\music\song_smol.mp3");
        assert_eq!(target.to_string_lossy(), r"D:\music\song.mp3");
    }

    #[test]
    fn compute_replace_target_extension_match_is_case_insensitive() {
        let target = compute_replace_target(r"D:\music\song.MP4", r"D:\music\song_smol.mp4");
        assert_eq!(target.to_string_lossy(), r"D:\music\song.MP4");
    }
}
```

- [ ] **Step 3: Register the command in `lib.rs`**

In `src-tauri/src/lib.rs`, add `fs_bridge::replace_original,` to the `invoke_handler!` list (place it after `fs_bridge::get_path_info,` or anywhere in that list):

```rust
            fs_bridge::replace_original,
```

- [ ] **Step 4: Run Rust tests to verify**

Run: `cargo test` in `src-tauri`
Expected: 3 new tests pass (`compute_replace_target_same_extension_returns_original`, `compute_replace_target_different_extension_reuses_stem`, `compute_replace_target_extension_match_is_case_insensitive`).

- [ ] **Step 5: Verify the crate compiles**

Run: `cargo check` in `src-tauri`
Expected: no errors (the `trash` crate and command signature compile).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/fs_bridge.rs src-tauri/src/lib.rs
git commit -m "feat: add replace_original command that swaps file via Recycle Bin"
```

---

### Task 2: Frontend plumbing — type, tauri wrapper, intermediate-path helper

**Files:**
- Modify: `src/types/index.ts` (extend `outputMode`)
- Modify: `src/lib/tauri.ts` (add `replaceOriginal` wrapper)
- Modify: `src/lib/outputPath.ts` (add `buildReplaceIntermediatePath`)

**Interfaces:**
- Consumes: the `replace_original` command from Task 1.
- Produces: `replaceOriginal(compressedPath: string, originalPath: string) => Promise<string>`; `buildReplaceIntermediatePath(inputPath: string) => string`; `Settings.outputMode` including `"replace"`.

- [ ] **Step 1: Extend the `outputMode` type**

In `src/types/index.ts`, change line 73 from:

```ts
  outputMode: "same-folder" | "subfolder" | "custom";
```

to:

```ts
  outputMode: "same-folder" | "subfolder" | "custom" | "replace";
```

- [ ] **Step 2: Add the `replaceOriginal` wrapper**

In `src/lib/tauri.ts`, after the `compressPdf` function (around line 163), add:

```ts
/**
 * Replace the original file with the compressed one: moves the original to the
 * Recycle Bin, then renames `compressedPath` onto the original's path (same
 * stem + new extension when the format changed). Returns the final path.
 * Throws if compressed is missing / not smaller, or the Recycle Bin move fails.
 */
export const replaceOriginal = (compressedPath: string, originalPath: string) =>
  invoke<string>("replace_original", { compressedPath, originalPath });
```

- [ ] **Step 3: Add the intermediate-path helper**

In `src/lib/outputPath.ts`, after `buildOutputPath` (end of file), add:

```ts
/**
 * Compute the transient intermediate output path for "replace original" mode.
 * Always sits in the input's directory with a distinct name (`{name}_smol{ext}`)
 * so the compression commands never see input == output.
 */
export function buildReplaceIntermediatePath(inputPath: string): string {
  const sep = inputPath.includes("\\") ? "\\" : "/";
  const lastSep = Math.max(inputPath.lastIndexOf("\\"), inputPath.lastIndexOf("/"));
  const dir = inputPath.slice(0, lastSep);
  const filename = inputPath.slice(lastSep + 1);
  const dotIdx = filename.lastIndexOf(".");
  const name = dotIdx >= 0 ? filename.slice(0, dotIdx) : filename;
  const ext = dotIdx >= 0 ? filename.slice(dotIdx) : "";
  return `${dir}${sep}${name}_smol${ext}`;
}
```

- [ ] **Step 4: Typecheck**

Run: `pnpm build` in the repo root
Expected: `tsc` passes and the Vite build completes without errors.

- [ ] **Step 5: Commit**

```bash
git add src/types/index.ts src/lib/tauri.ts src/lib/outputPath.ts
git commit -m "feat: add replace-mode types, tauri wrapper, and intermediate path helper"
```

---

### Task 3: Replace-mode compression flow

**Files:**
- Modify: `src/hooks/useCompression.ts`

**Interfaces:**
- Consumes: `buildReplaceIntermediatePath` and `replaceOriginal` from Task 2.
- Produces: the replace-mode branch inside `startSqueeze`; no new exports.

- [ ] **Step 1: Import the new helpers**

In `src/hooks/useCompression.ts`, update the imports from `@/lib/outputPath` and `@/lib/tauri`:

```ts
import { compressAudio, compressImage, compressPdf, compressVideo, replaceOriginal } from "@/lib/tauri";
import { buildOutputPath, buildReplaceIntermediatePath } from "@/lib/outputPath";
```

- [ ] **Step 2: Compute the intermediate path in replace mode**

In `startSqueeze`, replace the `buildOutputPath(...)` call (currently lines 68-73) with:

```ts
      const outputPath =
        outputMode === "replace"
          ? buildReplaceIntermediatePath(job.inputPath)
          : buildOutputPath(
              job.inputPath,
              outputMode,
              filenamePattern,
              customOutputDir,
            );
```

- [ ] **Step 3: Perform the swap after a successful smaller compression**

Replace the `// result.outputLarger: compressed ≥ original — original was kept` block (currently lines 118-123) with:

```ts
        // Replace mode: move original to Recycle Bin, put compressed in its place.
        // outputLarger (compressed ≥ original) → original kept, nothing replaced.
        if (outputMode === "replace" && !result.outputLarger) {
          const finalPath = await replaceOriginal(result.outputPath, job.inputPath);
          useJobsStore.getState().setJobOutput(jobId, finalPath, result.outputBytes);
        } else {
          useJobsStore.getState().setJobOutput(jobId, result.outputPath, result.outputBytes);
        }
```

- [ ] **Step 4: Typecheck + lint**

Run: `pnpm build` then `pnpm lint`
Expected: both pass with no errors (and no unused-variable warnings for `filenamePattern`/`customOutputDir` — they are still used in the non-replace branch).

- [ ] **Step 5: Commit**

```bash
git add src/hooks/useCompression.ts
git commit -m "feat: wire replace mode into the squeeze flow"
```

---

### Task 4: Settings UI — Replace original option

**Files:**
- Modify: `src/components/settings/OutputControls.tsx`

**Interfaces:**
- Consumes: `Settings.outputMode` (`"replace"`) from Task 2.
- Produces: the new dropdown option, disabled pattern input, and warning label.

- [ ] **Step 1: Widen the local `OutputMode` type and add the option**

In `src/components/settings/OutputControls.tsx`:
- Add to imports: `import type { Settings } from "@/types";`
- Replace the local type (line 7):

```tsx
type OutputMode = "same-folder" | "subfolder" | "custom";
```

with:

```tsx
type OutputMode = Settings["outputMode"];
```

- In the `modes` array (lines 20-29), add after the `custom` entry:

```tsx
    { id: "replace" as const, label: "Replace original" },
```

- [ ] **Step 2: Disable the filename pattern input and show the warning in replace mode**

Replace the filename-pattern `<input>` (lines 74-82) with:

```tsx
      {/* Filename pattern — disabled in replace mode (pattern is ignored) */}
      <input
        type="text"
        value={filenamePattern}
        disabled={outputMode === "replace"}
        onChange={(e) =>
          useSettingsStore.getState().patch({ filenamePattern: e.target.value })
        }
        className={`flex-1 bg-zinc-900 border border-zinc-800 rounded-lg px-2 py-1.5 text-xs ${
          outputMode === "replace"
            ? "text-zinc-600 cursor-not-allowed"
            : "text-zinc-200 placeholder-zinc-600 focus:outline-none focus:border-indigo-500"
        }`}
        placeholder="{name}_smol{ext}"
      />

      {outputMode === "replace" && (
        <span className="text-[10px] text-amber-400/80 shrink-0">
          Originals are moved to Recycle Bin
        </span>
      )}
```

- [ ] **Step 3: Hide the image before/after preview for replaced jobs**

In `src/components/filelist/DoneCard.tsx`, change the preview button condition (line 147) from:

```tsx
        {job.kind === "image" && job.outputPath && !outputLarger && (
```

to:

```tsx
        {job.kind === "image" && job.outputPath && !outputLarger && job.outputPath !== job.inputPath && (
```

- [ ] **Step 4: Typecheck + lint**

Run: `pnpm build` then `pnpm lint`
Expected: both pass.

- [ ] **Step 5: Commit**

```bash
git add src/components/settings/OutputControls.tsx src/components/filelist/DoneCard.tsx
git commit -m "feat: add Replace original option to output controls"
```

---

### Task 5: Integration verification

**Files:**
- None (manual verification).

- [ ] **Step 1: Full build + tests**

Run in repo root: `pnpm build`
Run in `src-tauri`: `cargo test` then `cargo check`
Expected: all pass.

- [ ] **Step 2: Manual checklist (requires `pnpm tauri dev` with the pinned sidecars)**

Verify each on Windows:

1. Video `D:\vids\clip.mp4` in replace mode → after squeeze: `clip.mp4` is the compressed file, original is in the Recycle Bin, DoneCard shows input→output sizes + saved %.
2. Audio `D:\music\track.wav` in replace mode → `track.wav` gone (Recycle Bin), `track.mp3` present, same directory/stem, new extension kept.
3. Image already heavily compressed (e.g. an optimized PNG) in replace mode → shows "Already optimal", original file byte-for-byte untouched, nothing in the Recycle Bin.
4. PDF in replace mode → original recycled, compressed PDF at the original path.
5. Non-replace modes (`same-folder` / `subfolder` / `custom`) behave exactly as before; pattern input re-enables when switching back.
6. Recycle Bin failure path: (simulate by making the Recycle Bin move fail, e.g. via a denied/read-only original) → job shows error, both original and intermediate file remain on disk.

- [ ] **Step 3: Commit any leftover changes from verification**

```bash
git status
# commit only if verification surfaced required fixes
```

---

## Self-Review Notes

- **Spec coverage:** backend command + registration (Task 1), type/wrapper/helper (Task 2), flow integration (Task 3), dropdown/disabled-input/warning (Task 4 Step 1-2), preview gating (Task 4 Step 3), already-optimal untouched (falls through existing `outputLarger` path, Task 3), extension-change handling (Rust `compute_replace_target`, Task 1), error handling (Task 1 recycle-bin abort + Task 3 `setJobError`).
- **Placeholder scan:** all steps contain exact code/paths/commands.
- **Type consistency:** `replaceOriginal`, `buildReplaceIntermediatePath`, `Settings["outputMode"]` used identically across Tasks 2-4; Rust command name `replace_original` matches the `invoke` string in `replaceOriginal`.
