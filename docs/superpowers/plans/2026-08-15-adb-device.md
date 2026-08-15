# ADB 设备压缩 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the abandoned MTP/COM approach with an ADB-based flow: an in-app ADB file browser selects device files, `adb pull` imports them to a local workspace, they compress, and the result is delivered per a device delivery mode (PC folder / Android folder / replace the Android file).

**Architecture:** A new Rust module `src-tauri/src/adb.rs` wraps the bundled `adb.exe` (`std::process::Command`, no COM). A frontend `DeviceBrowser` modal lists/enumerates device dirs and picks files; Dropzone gets an "Add from device" button (rfd `open()` is restored for local files). Device jobs carry `deviceRemotePath`/`deviceDeliveryMode`/`deviceRemoteDir`; `useCompression.ts` delivers after compression. The entire MTP implementation is removed.

**Tech Stack:** Rust (Tauri 2), React 19 + TS + Zustand, ADB platform-tools (bundled via a fetch script).

## Global Constraints

- Windows 10/11 64-bit only. Frontend uses `@/` alias.
- ADB is bundled: `scripts/fetch-adb.mjs` downloads platform-tools and extracts `adb.exe` to `src-tauri/binaries/adb.exe`; `tauri.conf.json` `bundle.resources` includes it; Rust resolves it next to the running exe.
- All `adb` invocations use `std::process::Command` with `CREATE_NO_WINDOW` on Windows; check `status.success()`.
- Device delivery modes (job-level, serialized as strings): `"pc-folder"`, `"android-folder"`, `"replace"`.
- `adb_ls` parsing is defensive: never error on odd output — return whatever parses (empty on failure). It is the one device-dependent risk; the parser is a pure, unit-tested function.
- Delivery failure copies the staged output to `{documents}\Smol\recovered\{name}` and the error message carries that path.
- Remove all MTP code: `src-tauri/src/import.rs`, `mod import;` + its command registrations, the `windows` direct dependency and `base64` (if otherwise unused), frontend `imported`/`importParentIdListB64` fields, `pickImport`/`deliverOutput`/`ensureImportWorkspace` wrappers, the custom picker in Dropzone, and the imported-job handling in `useCompression.ts`.

---

### Task 1: Remove the MTP implementation

**Files:**
- Delete: `src-tauri/src/import.rs`
- Modify: `src-tauri/src/lib.rs` (remove `mod import;` + 4 registrations)
- Modify: `src-tauri/Cargo.toml` (remove `windows` + `base64` direct deps; revert added features)
- Modify: `src/types/index.ts` (remove `imported`/`importParentIdListB64`)
- Modify: `src/lib/tauri.ts` (remove PickResult/DeliverResult/`pickImport`/`deliverOutput`/`ensureImportWorkspace`/`deleteLocalFile` if only used by MTP)
- Modify: `src/store/jobs.ts` (revert `NewJobInput` widening)
- Modify: `src/components/dropzone/Dropzone.tsx` (restore rfd `open()` local-file dialog; remove the `pickImport` flow and its now-unused imports)
- Modify: `src/hooks/useCompression.ts` (remove `handleImportedJob`, `stagingOutputPath`, the `job.imported` branch and its imports `deliverOutput`/`deleteLocalFile`/`toast` if unused; restore the pre-MTP result handling)
- Modify: `src/App.tsx` (remove the `ensureImportWorkspace` startup effect)

**Interfaces:**
- Produces: a clean tree with no MTP code. `pnpm build` and `cargo check`/`cargo test` green.

- [ ] **Step 1: Remove Rust MTP code**

Delete `src-tauri/src/import.rs`. In `src-tauri/src/lib.rs` remove `mod import;` and the `crate::import::*` entries from `invoke_handler` (keep `fs_bridge`, `probe`, `thumbs`, compress commands). In `Cargo.toml` remove the `windows` and `base64` direct deps (if `windows`/`base64` are no longer referenced anywhere in `src-tauri/src`).

- [ ] **Step 2: Remove frontend MTP code**

Remove the MTP Job fields, the `PickResult`/`DeliverResult` types and wrappers, the `NewJobInput` widening, the custom-picker `handleOpenDialog`, and the `handleImportedJob`/`stagingOutputPath` logic + the `job.imported` branch. Restore Dropzone's local-file dialog using `@tauri-apps/plugin-dialog` `open(...)` (the original `handleOpenDialog` with extension filters and `getPathInfo`). Restore `useCompression.ts` result handling to the pre-MTP form (replace-mode via `replaceOriginal`; else `setJobOutput`). Remove the App.tsx startup effect.

- [ ] **Step 3: Verify**

Run `pnpm build` (repo root) and `cargo check` + `cargo test` (src-tauri). All green; grep confirms no `import_`/`pick_import`/`deliver_output`/`PickResult`/`importParentIdListB64` references remain in `src/`/`src-tauri/src`.

- [ ] **Step 4: Commit**

```bash
git add -A src src-tauri
git commit -m "revert: remove MTP implementation (superseded by ADB)"
```

---

### Task 2: Backend — `adb.rs`, fetch script, bundling, device commands

**Files:**
- Create: `src-tauri/src/adb.rs` (pure `ls` parser + tests; command wrappers; 3 commands)
- Create: `scripts/fetch-adb.mjs`
- Modify: `src-tauri/src/lib.rs` (`mod adb;` + registrations)
- Modify: `src-tauri/Cargo.toml` (no new deps expected)
- Modify: `src-tauri/tauri.conf.json` (resources add adb.exe)
- Modify: `src-tauri/src/error.rs` (no change expected)

**Interfaces:**
- Produces:
  - Pure `pub fn parse_ls_line(line: &str) -> Option<DeviceEntry>` and `pub fn parse_ls_output(output: &str) -> Vec<DeviceEntry>` where `DeviceEntry { name, is_dir, size }` (serde camelCase). Tests: directory/file/`.`/`..`/unusual-permission/empty-line rows.
  - `fn adb_path() -> Result<PathBuf, AppError>` (exe-dir `adb.exe`, fallback `PATH`).
  - Commands:
    - `list_device_dir(path: String) -> Result<Vec<DeviceEntry>, AppError>` (runs `adb shell "ls -la <path>"`; on failure returns a guidance error re USB debugging)
    - `pull_device_files(items: Vec<String>, workspace: String) -> Result<Vec<PullResult>, AppError>` — `PullResult { key, local_path, name, size, remote_path }` camelCase; unique `{workspace}\{uuid}\` per item; `adb pull <remote> <local>`
    - `deliver_to_device(local_path, mode, remote_path, remote_dir, pc_dir, new_name) -> Result<DeliverResult, AppError>` — `DeliverResult { note: Option<String> }`; pc-folder → `std::fs::copy`; android-folder → `adb push <local> <remote_dir>/<new_name>`; replace → `adb push <local> <remote_path>`; failure → copy to `{documents}\Smol\recovered\{name}` and error message includes the recovered path
- Consumed by Task 3.

- [ ] **Step 1: Write the failing tests** (append `#[cfg(test)]` to `adb.rs`)

```rust
use super::*;

#[test]
fn parses_regular_file() {
    let e = parse_ls_line("-rw-rw---- 1 root sdcard_rw 24576 2024-01-01 10:00 IMG_1.jpg").unwrap();
    assert_eq!(e.name, "IMG_1.jpg");
    assert!(!e.is_dir);
    assert_eq!(e.size, 24576);
}

#[test]
fn parses_directory() {
    let e = parse_ls_line("drwxrwx--x 2 root sdcard_rw  4096 2024-01-01 10:00 DCIM").unwrap();
    assert!(e.is_dir);
    assert_eq!(e.name, "DCIM");
}

#[test]
fn skips_dot_entries() {
    let out = parse_ls_output("drwxrwx--x 1 root sdcard_rw 4096 2024-01-01 10:00 .\ndrwxrwx--x 1 root sdcard_rw 4096 2024-01-01 10:00 ..\n-rw-rw---- 1 root sdcard_rw 123 2024-01-01 10:00 a.txt");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].name, "a.txt");
}

#[test]
fn tolerates_odd_lines() {
    assert!(parse_ls_line("").is_none());
    assert!(parse_ls_line("total 128").is_none());
    assert!(parse_ls_output("garbage\n").is_empty());
}
```

- [ ] **Step 2: Run tests to verify they fail** — `cargo test adb::` in src-tauri → FAIL (undefined).

- [ ] **Step 3: Implement `adb.rs`**

```rust
use std::path::{Path, PathBuf};
use serde::Serialize;
use crate::error::AppError;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceEntry { pub name: String, pub is_dir: bool, pub size: u64 }

/// Parse one `ls -la` line. Never fails the caller — returns None on odd lines.
pub fn parse_ls_line(line: &str) -> Option<DeviceEntry> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() || line.starts_with("total") { return None; }
    let mut cols = line.split_whitespace();
    let perms = cols.next()?;
    let _links = cols.next()?;
    let _owner = cols.next()?;
    let _group = cols.next()?;
    let size: u64 = cols.next()?.parse().ok()?;
    let _date1 = cols.next()?;
    let _date2 = cols.next()?;
    let mut name_parts: Vec<&str> = cols.collect();
    // name may be last column only, but guard against extra columns
    let name = name_parts.pop()?.to_string();
    if name == "." || name == ".." { return None; }
    Some(DeviceEntry { name, is_dir: perms.starts_with('d'), size })
}

pub fn parse_ls_output(output: &str) -> Vec<DeviceEntry> {
    output.lines().filter_map(parse_ls_line).collect()
}

fn adb_cmd() -> Result<std::process::Command, AppError> {
    let adb = adb_path()?;
    let mut cmd = std::process::Command::new(&adb);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    Ok(cmd)
}

/// Bundled `adb.exe` next to the app exe, else on PATH.
pub fn adb_path() -> Result<PathBuf, AppError> {
    let self_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    if let Some(dir) = self_dir {
        let bundled = dir.join("adb.exe");
        if bundled.exists() { return Ok(builtin_or_bundled(bundled)); }
    }
    Err(AppError::Other("adb not found — run scripts/fetch-adb.mjs".into()))
}

fn builtin_or_bundled(p: PathBuf) -> PathBuf { p }
```

Note: keep `adb_path` simple; the fetch script is the source of `adb.exe`. Then the three commands:

```rust
#[tauri::command]
pub async fn list_device_dir(path: String) -> Result<Vec<DeviceEntry>, AppError> {
    let mut cmd = adb_cmd()?;
    cmd.args(["shell", &format!("ls -la {path}")]);
    let out = cmd.output()
        .map_err(|e| AppError::Other(format!("adb shell failed: {e}")))?;
    if !out.status.success() {
        return Err(AppError::Other(
            "无法列出设备目录 — 请确认设备已连接并启用 USB 调试".into(),
        ));
    }
    Ok(parse_ls_output(&String::from_utf8_lossy(&out.stdout)))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullResult { pub key: String, pub local_path: String, pub name: String, pub size: u64, pub remote_path: String }

#[tauri::command]
pub async fn pull_device_files(items: Vec<String>, workspace: String) -> Result<Vec<PullResult>, AppError> {
    let mut out = Vec::new();
    for remote in items {
        let uuid = uuid::Uuid::new_v4().simple().to_string();
        let dest = format!("{workspace}\\{uuid}");
        std::fs::create_dir_all(&dest)?;
        let name = remote.rsplit('/').next().unwrap_or("file").to_string();
        let local = Path::new(&dest).join(&name);
        let mut cmd = adb_cmd()?;
        cmd.args(["pull", &remote]).arg(&local);
        let st = cmd.status()
            .map_err(|e| AppError::Other(format!("adb pull failed: {e}")))?;
        if !st.success() {
            return Err(AppError::Other(format!("adb pull 失败: {remote} — 请确认设备已连接并启用 USB 调试")));
        }
        let size = std::fs::metadata(&local).map(|m| m.len()).unwrap_or(0);
        out.push(PullResult { key: uuid, local_path: local.to_string_lossy().into_owned(), name, size, remote_path: remote });
    }
    Ok(out)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliverResult { pub note: Option<String> }

fn recovered_dir() -> Result<PathBuf, AppError> {
    let docs = std::env::var("USERPROFILE")
        .map_err(|_| AppError::Other("USERPROFILE not set".into()))?;
    let dir = Path::new(&docs).join("Documents").join("Smol").join("recovered");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[tauri::command]
pub async fn deliver_to_device(
    local_path: String,
    mode: String,
    remote_path: String,
    remote_dir: Option<String>,
    pc_dir: Option<String>,
    new_name: String,
) -> Result<DeliverResult, AppError> {
    let run = || -> Result<DeliverResult, AppError> {
        match mode.as_str() {
            "pc-folder" => {
                let dir = pc_dir.ok_or_else(|| AppError::Other("未选择 PC 输出目录".into()))?;
                std::fs::create_dir_all(&dir)?;
                std::fs::copy(&local_path, Path::new(&dir).join(&new_name))?;
                Ok(DeliverResult { note: None })
            }
            "android-folder" => {
                let dir = remote_dir.ok_or_else(|| AppError::Other("未选择设备目标目录".into()))?;
                let target = format!("{}/{}", dir.trim_end_matches('/'), new_name);
                let mut cmd = adb_cmd()?;
                cmd.args(["push"]).arg(&local_path).arg(&target);
                let st = cmd.status().map_err(|e| AppError::Other(format!("adb push failed: {e}")))?;
                if !st.success() { return Err(AppError::Other("adb push 失败 — 请确认设备已连接并启用 USB 调试".into())); }
                Ok(DeliverResult { note: None })
            }
            _ => { // replace
                let mut cmd = adb_cmd()?;
                cmd.args(["push"]).arg(&local_path).arg(&remote_path);
                let st = cmd.status().map_err(|e| AppError::Other(format!("adb push failed: {e}")))?;
                if !st.success() { return Err(AppError::Other("adb push 失败 — 请确认设备已连接并启用 USB 调试".into())); }
                Ok(DeliverResult { note: None })
            }
        }
    };
    match run() {
        Ok(r) => Ok(r),
        Err(e) => {
            // durable recovery copy so the user never loses the result
            let recovered = recovered_dir()?.join(&new_name);
            let copied = std::fs::copy(&local_path, &recovered).is_ok();
            let base = e.to_string();
            let msg = if copied {
                format!("{base}；压缩结果已复制到 {}", recovered.display())
            } else { base };
            Err(AppError::Other(msg))
        }
    }
}
```

- [ ] **Step 4: Create `scripts/fetch-adb.mjs`**

Mirror `scripts/fetch-ffmpeg.mjs`'s download/unzip pattern (check how that script downloads + extracts; use `fetch` for the zip and `unzipper`/`extract-zip` or a Node unzip if available — otherwise reuse the same dependency the ffmpeg script uses). Download `https://dl.google.com/android/repository/platform-tools-latest-windows.zip`, extract `platform-tools/adb.exe` → `src-tauri/binaries/adb.exe`. Add it to the `postinstall` chain in `package.json` next to `fetch-ffmpeg.mjs` (prefix with `node scripts/fetch-adb.mjs &&`).

- [ ] **Step 5: Bundle adb**

In `src-tauri/tauri.conf.json` `bundle.resources`, add `"binaries/adb.exe": "adb.exe"`.

- [ ] **Step 6: Register commands + verify**

Add `mod adb;` and register `list_device_dir, pull_device_files, deliver_to_device` in `lib.rs`. Run `cargo test` (parser tests pass) and `cargo check`; run `node scripts/fetch-adb.mjs` once to produce `adb.exe` locally for manual testing.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/adb.rs src-tauri/src/lib.rs src-tauri/tauri.conf.json scripts/fetch-adb.mjs package.json
git commit -m "feat: adb backend (ls parser, pull/push, device commands, bundled adb)"
```

---

### Task 3: Frontend — DeviceBrowser, types, delivery

**Files:**
- Create: `src/components/device/DeviceBrowser.tsx`
- Modify: `src/types/index.ts` (device fields)
- Modify: `src/lib/tauri.ts` (wrappers)
- Modify: `src/store/jobs.ts` (`NewJobInput` widen with device fields; `updateJobOverrides` or new action for delivery mode)
- Modify: `src/components/dropzone/Dropzone.tsx` ("Add from device" button; keep rfd local dialog)
- Modify: `src/components/filelist/JobRow.tsx` (delivery-mode dropdown for device jobs)
- Modify: `src/hooks/useCompression.ts` (device job delivery after compress)

**Interfaces:**
- Consumes: Task 2 commands `list_device_dir`, `pull_device_files`, `deliver_to_device`; `delete_local_file` (add this wrapper if it was removed in Task 1 — keep the command in Rust or use a small fs remove; simplest: keep `delete_local_file` in `fs_bridge.rs` if present, else re-add the Rust command + wrapper).
- Produces:
  - `Job.deviceRemotePath?: string`, `Job.deviceDeliveryMode?: "pc-folder" | "android-folder" | "replace"`, `Job.deviceRemoteDir?: string`
  - Wrappers `listDeviceDir`, `pullDeviceFiles`, `deliverToDevice`, `deleteLocalFile`

- [ ] **Step 1: Types + wrappers**

Add the three Job fields; add wrappers in `tauri.ts` (camelCase args matching the Rust commands). Widen `NewJobInput`.

- [ ] **Step 2: DeviceBrowser component**

A modal (`src/components/device/DeviceBrowser.tsx`) with: current-dir label + up button, common-dir shortcut chips (DCIM, Pictures, Download, Movies, Music), a file/dir list (dirs navigate, files multi-select), and an "Add N files" confirm button. It calls `listDeviceDir` on mount and on navigation; on `adb` error shows the USB-debugging guidance text. Emits `selected: string[]` (remote paths) on confirm. Optionally supports a "pick directory" mode (for android-folder delivery) that emits one remote dir path.

- [ ] **Step 3: Dropzone "Add from device"**

Add a button (e.g. `Smartphone` lucide icon) next to "Open files…" that opens the DeviceBrowser; on selection calls `pullDeviceFiles(paths, workspace)` where `workspace = await ensureWorkspace()` (reuse the Rust `ensure_import_workspace` if kept — if Task 1 removed it, re-add a `get_import_workspace` command or reuse the same `Documents\Smol\imports` path logic; simplest: keep a `workspace_dir()` command in `adb.rs` or reuse the existing one). Adds jobs with `deviceRemotePath`, `deviceDeliveryMode: "replace"` default.

- [ ] **Step 4: JobRow delivery-mode dropdown**

For jobs with `deviceRemotePath`, render a small `<select>` with the 3 modes writing to `deviceDeliveryMode` via the store. For `android-folder`, an extra "choose device dir" affordance (opens DeviceBrowser in directory mode, writes `deviceRemoteDir`).

- [ ] **Step 5: useCompression delivery**

For jobs with `deviceRemotePath` (branch FIRST, before the local replace branch): compress to a workspace staging path (`stagingOutputPath`), then if `outputLarger` → `deleteLocalFile(inputPath)`; else `deliverToDevice(staged, deviceDeliveryMode, deviceRemotePath, deviceRemoteDir, pcDir, newName)` — `pcDir` from a per-job chosen PC folder (default: user's `Downloads\Smol\`) or `customOutputDir` if set; on success delete staged + input copy; on failure toast the real error (which includes the recovered path).

- [ ] **Step 6: Typecheck + lint**

`pnpm build` + eslint on changed files — clean.

- [ ] **Step 7: Commit**

```bash
git add src/types/index.ts src/lib/tauri.ts src/store/jobs.ts src/components/device/DeviceBrowser.tsx src/components/dropzone/Dropzone.tsx src/components/filelist/JobRow.tsx src/hooks/useCompression.ts
git commit -m "feat: ADB device browser, add-from-device flow, delivery modes"
```

---

### Task 4: Integration verification

- [ ] **Step 1: Build + tests** — `pnpm build`, `cargo test`, `cargo check` all green.
- [ ] **Step 2: Manual device checklist (user)** — with USB debugging on:
  1. Unauthorized state → guidance; authorize → browser works.
  2. Browse common dirs / navigate / multi-select.
  3. Pull → compress → each delivery mode (PC dir / device dir / replace) verified.
  4. Already-optimal → device untouched, local cleaned.
  5. Unplug during delivery → recovered copy exists + truthful toast.
  6. Two same-named files → distinct `{uuid}` dirs.
- [ ] **Step 3: Commit fixes if surfaced.**

---

## Self-Review Notes

- **Spec coverage:** MTP removal (T1), adb backend + fetch + bundling + tests (T2), DeviceBrowser/types/delivery (T3), verification (T4).
- **Placeholder scan:** parser/commands have near-complete code; `fetch-adb.mjs` defers to the ffmpeg script's download/unzip pattern (mirror it); "delete_local_file"/workspace commands are either kept or re-added minimally — flagged in T3 Step 1.
- **Type consistency:** `DeviceEntry`/`PullResult`/`DeliverResult` camelCase match Rust `rename_all`; `deliverToDevice` arg order matches the Rust command; device-job branch is FIRST in useCompression so device jobs never hit local replace.
- **Deferred:** MTP drag-drop, device dir delete/rename, adb transfer cancellation, USB-debugging setup wizard.
