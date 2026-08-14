# MTP 安卓设备压缩支持 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users compress files straight from a USB-connected Android (MTP) device: auto-import the file to a local workspace via the Windows Shell API, compress it, then deliver the result back to the device according to the output mode.

**Architecture:** A new Rust module `src-tauri/src/import.rs` owns all Shell/COM work (`IShellItem`, `IFileOperation`) inside `spawn_blocking` + `CoInitializeEx(STA)` closures; the pure mode→target mapping (`resolve_device_target`) is a tested pure function. The frontend adds a fallback import path (`importPathToWorkspace`) used by both add-entry points, marks jobs `imported`, and calls `deliver_output` after compression. A real-device spike is Task 1 and gates everything.

**Tech Stack:** Rust (Tauri 2, `windows` 0.61 Shell/Com features), React 19 + TypeScript + Zustand.

## Global Constraints

- Windows 10/11 64-bit only. Frontend uses `@/` alias.
- Workspace: `%USERPROFILE%\Documents\Smol\imports` (via `SHGetKnownFolderPath` FOLDERID_Documents); each import lands in a unique subdir `{workspace}\{uuid}\`.
- Every COM call runs on a `spawn_blocking` thread with `CoInitializeEx(null, COINIT_APARTMENTTHREADED)` at the top and `CoUninitialize()` at the end; `IFileOperation::PerformOperations()` must be called after queuing.
- `deliver_output` runs ALL its COM in ONE `spawn_blocking` closure.
- Size hard-guard applies ONLY to `mode == "replace"`: `local_size < original_size`, else reject.
- replace temp name: `{stem}.{salt}.smol_tmp{ext}` where `salt` = 8-char random string; 3-step in one IFileOperation: CopyItem(temp) → DeleteItem(original) → RenameItem(temp→original).
- Startup GC: `ensure_import_workspace` removes workspace subdirs with mtime older than 60 minutes.
- Rust mode enum must use `#[serde(rename_all = "kebab-case")]`.
- Existing compress commands, `buildOutputPath`, and non-imported behavior are UNCHANGED.

---

### Task 1: Spike — MTP path format verification (REQUIRES the user's physical Android device)

**Files:**
- Modify: `src-tauri/src/lib.rs` (register one temporary diagnostic command)
- Create: `src-tauri/src/spike.rs` (temporary, removed after the spike)
- Modify: `src/hooks/useDragDrop.ts` (temporary `console.log` of delivered paths)

**Interfaces:**
- Produces: evidence that Tauri-delivered MTP paths parse via `SHParseDisplayName` (or not), plus whether the rfd dialog/directory picker shows MTP. This gates Tasks 2-6.

- [ ] **Step 1: Add a temporary diagnostic command**

Create `src-tauri/src/spike.rs`:

```rust
use windows::core::{BSTR, HSTRING};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::SHParseDisplayName;

/// Temporary spike command: try to resolve a display path into an IShellItem.
/// Prints / returns the outcome so we can verify Tauri-delivered MTP paths parse.
#[tauri::command]
pub fn spike_parse_path(display_path: String) -> bool {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok();
        let name = HSTRING::from(&display_path);
        let mut item: Option<windows::Win32::UI::Shell::IShellItem> = None;
        let hr = SHParseDisplayName(&name, None, &mut item, 0, None);
        let ok = hr.is_ok() && item.is_some();
        CoUninitialize();
        ok
    }
}

/// Temporary spike command: report the delivered drag/drop path verbatim.
#[tauri::command]
pub fn spike_echo(path: String) -> String {
    path
}
```

Add `mod spike;` in `src-tauri/src/lib.rs` and register `spike::spike_parse_path`, `spike::spike_echo` in `invoke_handler`.

- [ ] **Step 2: Add temporary path logging to the frontend**

In `src/hooks/useDragDrop.ts`, inside the `drop` branch (after `const rawPaths = event.payload.paths;`), add:

```ts
          // TEMP SPIKE: log every delivered path + parse outcome
          for (const p of rawPaths) {
            console.log("[spike] delivered path:", p);
            try {
              const { invoke } = await import("@tauri-apps/api/core");
              const parses = await invoke<boolean>("spike_parse_path", { displayPath: p });
              console.log("[spike] SHParseDisplayName:", parses, "| path:", p);
            } catch (e) { console.log("[spike] error", e); }
          }
```

- [ ] **Step 3: Verify it compiles and builds**

Run in `src-tauri`: `cargo check` — must pass. Add `windows = { version = "0.61", features = ["Win32_Foundation", "Win32_System_Com", "Win32_UI_Shell"] }` to `Cargo.toml` first if not already added.

- [ ] **Step 4: USER verification on the real device (gate)**

Instruct the user to run `pnpm tauri dev`, connect the Android device, then:
1. Drag one file from the device into the app window.
2. Open a file dialog and select one device file.
3. Open the custom-output directory picker and try to select a device folder.
4. Report the DevTools console output (`[spike] delivered path: …` and `[spike] SHParseDisplayName: …`) plus whether the dialog/picker could see MTP items.

- [ ] **Step 5: Record the spike outcome and decide**

- If MTP paths parse via `SHParseDisplayName` → keep the design, proceed to Task 2.
- If not parseable but the app can still access the bytes → adopt 方案 B in the spec (guide-only).
- If the directory picker cannot select MTP → custom mode falls back to same-folder behavior with a toast (spec §spike).
- Remove `spike.rs`, the `spike::` registrations, and the TEMP logging before Task 2 (keep the `windows` dependency).

- [ ] **Step 6: Commit the spike tooling (and its later removal)**

```bash
git add src-tauri/src/spike.rs src-tauri/src/lib.rs src/hooks/useDragDrop.ts src-tauri/Cargo.toml
git commit -m "spike: MTP path format diagnostic tooling"
```
(After Step 5 removal: `git commit -m "spike: remove diagnostic tooling after verification"`.)

---

### Task 2: Backend pure logic — `resolve_device_target` + helpers + tests

**Files:**
- Create: `src-tauri/src/import.rs` (pure functions + tests only; no COM yet)
- Modify: `src-tauri/src/lib.rs` (`mod import;`)

**Interfaces:**
- Produces:
  - `pub fn compute_import_dir(workspace: &str, uuid: &str) -> String` → `{workspace}\{uuid}`
  - `pub fn has_enough_space(free_bytes: u64, needed_bytes: u64) -> bool` → `true` when `needed_bytes == 0` (skip pre-check) or `free_bytes >= needed_bytes`
  - `pub struct DeviceTarget { pub dest_folder: String, pub name: String, pub create_folder: bool, pub replace_original: bool, pub rename_from: String }`
  - `pub fn resolve_device_target(mode: &str, import_source_path: &str, custom_output_dir: Option<&str>, new_name: &str, original_name: &str, salt: &str) -> DeviceTarget`
- Consumed by Task 3 (`deliver_output`) and tested here.

- [ ] **Step 1: Write the failing tests**

Append a `#[cfg(test)] mod tests` to `src-tauri/src/import.rs`:

```rust
use super::*;

fn parent_of(p: &str) -> String {
    // "D:\DCIM\Camera\IMG.jpg" -> "D:\DCIM\Camera"
    let idx = p.rfind('\\').or_else(|| p.rfind('/')).unwrap();
    p[..idx].to_string()
}

#[test]
fn compute_import_dir_joins_workspace_and_uuid() {
    assert_eq!(compute_import_dir(r"C:\Smol\imports", "abc-123"), r"C:\Smol\imports\abc-123");
}

#[test]
fn has_enough_space_ok_when_free_is_larger() {
    assert!(has_enough_space(1024, 512));
    assert!(!has_enough_space(512, 1024));
}

#[test]
fn has_enough_space_zero_needed_skips_check() {
    assert!(has_enough_space(0, 0)); // size unknown -> skip
}

#[test]
fn same_folder_target_uses_parent_and_new_name() {
    let t = resolve_device_target("same-folder", r"D:\DCIM\Camera\IMG.jpg", None, "IMG_smol.jpg", "IMG.jpg", "s");
    assert_eq!(t.dest_folder, parent_of(r"D:\DCIM\Camera\IMG.jpg"));
    assert_eq!(t.name, "IMG_smol.jpg");
    assert!(!t.create_folder && !t.replace_original);
}

#[test]
fn subfolder_target_appends_smol_and_creates() {
    let t = resolve_device_target("subfolder", r"D:\DCIM\Camera\IMG.jpg", None, "IMG_smol.jpg", "IMG.jpg", "s");
    assert_eq!(t.dest_folder, parent_of(r"D:\DCIM\Camera\IMG.jpg") + r"\smol");
    assert!(t.create_folder && !t.replace_original);
}

#[test]
fn custom_target_uses_custom_dir() {
    let t = resolve_device_target("custom", r"D:\DCIM\Camera\IMG.jpg", Some(r"E:\out"), "IMG_smol.jpg", "IMG.jpg", "s");
    assert_eq!(t.dest_folder, r"E:\out");
}

#[test]
fn replace_uses_salted_temp_name_and_rename() {
    let t = resolve_device_target("replace", r"D:\DCIM\Camera\IMG.jpg", None, "IMG_smol.jpg", "IMG.jpg", "a1b2c3d4");
    assert_eq!(t.name, "IMG.a1b2c3d4.smol_tmp.jpg");
    assert_eq!(t.rename_from, "IMG.jpg");
    assert!(t.replace_original && !t.create_folder);
}

#[test]
fn unknown_mode_falls_back_to_same_folder() {
    let t = resolve_device_target("bogus", r"D:\DCIM\Camera\IMG.jpg", None, "IMG_smol.jpg", "IMG.jpg", "s");
    assert_eq!(t.dest_folder, parent_of(r"D:\DCIM\Camera\IMG.jpg"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run (workdir `G:\document\smol\src-tauri`): `cargo test import::`
Expected: FAIL — `compute_import_dir` / `resolve_device_target` not defined.

- [ ] **Step 3: Implement the pure functions**

Add `mod import;` to `src-tauri/src/lib.rs`, and create `src-tauri/src/import.rs` with:

```rust
/// Unique per-import workspace subdirectory.
pub fn compute_import_dir(workspace: &str, uuid: &str) -> String {
    let sep = if workspace.contains('\\') { "\\" } else { "/" };
    format!("{workspace}{sep}{uuid}")
}

/// Free-space pre-check. `needed_bytes == 0` means "unknown" → allow (skip).
pub fn has_enough_space(free_bytes: u64, needed_bytes: u64) -> bool {
    needed_bytes == 0 || free_bytes >= needed_bytes
}

#[derive(Debug, PartialEq)]
pub struct DeviceTarget {
    pub dest_folder: String,
    pub name: String,
    pub create_folder: bool,
    pub replace_original: bool,
    pub rename_from: String,
}

/// Map an output mode to the device-side delivery target. Pure & unit-tested.
///
/// | mode         | dest_folder        | name | create | replace | rename_from |
/// |--------------|--------------------|------|--------|---------|-------------|
/// | same-folder  | parent(src)        | new_name | no | no | "" |
/// | subfolder    | parent(src)\smol   | new_name | yes | no | "" |
/// | custom       | custom_output_dir  | new_name | no | no | "" |
/// | replace      | parent(src)        | {stem}.{salt}.smol_tmp{ext} | no | yes | original_name |
pub fn resolve_device_target(
    mode: &str,
    import_source_path: &str,
    custom_output_dir: Option<&str>,
    new_name: &str,
    original_name: &str,
    salt: &str,
) -> DeviceTarget {
    let parent = {
        let idx = import_source_path
            .rfind('\\')
            .or_else(|| import_source_path.rfind('/'))
            .unwrap_or(0);
        import_source_path[..idx].to_string()
    };

    match mode {
        "replace" => {
            // salted temp name; uuid-free residue-proof
            let stem = original_name
                .rsplit_once('.')
                .map(|(s, _)| s)
                .unwrap_or(original_name);
            let ext = original_name
                .rsplit_once('.')
                .map(|(_, e)| e)
                .unwrap_or("");
            DeviceTarget {
                dest_folder: parent,
                name: if ext.is_empty() {
                    format!("{stem}.{salt}.smol_tmp")
                } else {
                    format!("{stem}.{salt}.smol_tmp.{ext}")
                },
                create_folder: false,
                replace_original: true,
                rename_from: original_name.to_string(),
            }
        }
        "subfolder" => {
            let sep = if parent.contains('\\') { "\\" } else { "/" };
            DeviceTarget {
                dest_folder: format!("{parent}{sep}smol"),
                name: new_name.to_string(),
                create_folder: true,
                replace_original: false,
                rename_from: String::new(),
            }
        }
        "custom" => DeviceTarget {
            dest_folder: custom_output_dir.unwrap_or(&parent).to_string(),
            name: new_name.to_string(),
            create_folder: false,
            replace_original: false,
            rename_from: String::new(),
        },
        // same-folder and any unknown mode
        _ => DeviceTarget {
            dest_folder: parent,
            name: new_name.to_string(),
            create_folder: false,
            replace_original: false,
            rename_from: String::new(),
        },
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run (workdir `G:\document\smol\src-tauri`): `cargo test import::`
Expected: PASS — 8 tests, 0 failed.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/import.rs src-tauri/src/lib.rs
git commit -m "feat: MTP import pure logic (device target mapping, workspace dir, space check)"
```

---

### Task 3: Backend COM commands — workspace, import, deliver, delete

**Files:**
- Modify: `src-tauri/Cargo.toml` (windows features)
- Modify: `src-tauri/src/import.rs` (add COM wrapper + 4 commands)
- Modify: `src-tauri/src/lib.rs` (register 4 commands)

**Interfaces:**
- Consumes: Task 2 pure functions; `crate::error::disk_full_hint`; `crate::fs_bridge::PathInfo` (reuse the struct from `fs_bridge.rs`).
- Produces Tauri commands:
  - `ensure_import_workspace() -> String` (also runs startup GC)
  - `import_shell_item(display_path: String, workspace: String) -> Result<PathInfo, AppError>`
  - `deliver_output(local_path, mode, import_source_path, custom_output_dir: Option<String>, new_name, original_name) -> Result<DeliverResult, AppError>` where `DeliverResult { note: Option<String> }` (serialized camelCase)
  - `delete_local_file(path: String) -> Result<(), AppError>`

- [ ] **Step 1: Extend the windows dependency features**

In `src-tauri/Cargo.toml`, replace the spike-era `windows` entry with:

```toml
windows = { version = "0.61", features = [
    "Win32_Foundation",
    "Win32_System_Com",
    "Win32_UI_Shell",
    "Win32_Storage_FileSystem",
] }
```
(If `IPropertyStore`/`System.Size` symbols are missing at compile time, add `"Win32_UI_Shell_PropertiesSystem"`.)

- [ ] **Step 2: Add the COM helper + commands to `import.rs`**

Extend `src-tauri/src/import.rs` with:

```rust
use std::path::Path;
use crate::error::{disk_full_hint, AppError};
use crate::fs_bridge::PathInfo;
use windows::core::HSTRING;
use windows::Win32::Foundation::BOOL;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{FileOperation, IFileOperation, SHParseDisplayName, IShellItem, FOFX_NOCONFIRMATION, FOF_NOCONFIRMATION};

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliverResult { pub note: Option<String> }

/// Run `f` on a blocking thread with COM initialized (STA). Returns f's result.
async fn with_com<T, F>(f: F) -> Result<T, AppError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, AppError> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(|e| AppError::Other(format!("CoInitializeEx: {e}")))?;
        let out = f();
        unsafe { CoUninitialize() };
        out
    })
    .await
    .map_err(|e| AppError::Other(format!("Blocking task failed: {e}")))?
}

fn parse_shell_item(display: &str) -> Result<IShellItem, AppError> {
    unsafe {
        let name = HSTRING::from(display);
        let mut item: Option<IShellItem> = None;
        SHParseDisplayName(&name, None, &mut item, 0, None)
            .map_err(|e| AppError::Other(format!("SHParseDisplayName failed for {display}: {e}")))?;
        item.ok_or_else(|| AppError::Other(format!("No shell item for {display}")))
    }
}

fn copy_item(
    op: &IFileOperation,
    src: &IShellItem,
    dest_folder: &IShellItem,
    name: Option<&str>,
) -> Result<(), AppError> {
    let name_pwstr = name.map(|n| windows::core::PWSTR(n.encode_utf16().collect::<Vec<_>>().as_mut_ptr()));
    unsafe { op.CopyItem(src, Some(dest_folder), name_pwstr.as_ref().map(|p| p.0), None) }
        .map_err(|e| AppError::Other(format!("CopyItem: {e}")))
}

fn new_file_operation() -> Result<IFileOperation, AppError> {
    unsafe {
        CoCreateInstance(&FileOperation, None, CLSCTX_ALL)
            .map_err(|e| AppError::Other(format!("CoCreateInstance IFileOperation: {e}")))
    }
}

fn perform(op: &IFileOperation) -> Result<(), AppError> {
    unsafe { op.SetOperationFlags(FOF_NOCONFIRMATION.0 | FOFX_NOCONFIRMATION.0) }
        .map_err(|e| AppError::Other(format!("SetOperationFlags: {e}")))?;
    unsafe { op.PerformOperations() }
        .map_err(|e| AppError::Other(format!("PerformOperations: {e}")))
}
```

Then the four commands:

```rust
/// Create & return the import workspace; clean subdirs older than 60 min.
#[tauri::command]
pub async fn ensure_import_workspace() -> Result<String, AppError> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::SHGetKnownFolderPath;
    let dir = with_com(|| {
        unsafe {
            let mut raw = std::ptr::null_mut();
            SHGetKnownFolderPath(&windows::Win32::UI::Shell::FOLDERID_Documents, 0, None, &mut raw)
                .map_err(|e| AppError::Other(format!("SHGetKnownFolderPath: {e}")))?;
            let path = windows::core::PWSTR(raw).to_string().unwrap_or_default();
            CoTaskMemFree(raw as *mut _);
            let ws = format!(r"{path}\Smol\imports");
            std::fs::create_dir_all(&ws)?;
            // startup GC: remove subdirs with mtime older than 60 min
            if let Ok(entries) = std::fs::read_dir(&ws) {
                for e in entries.flatten() {
                    if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        if let Ok(meta) = e.metadata() {
                            let age = std::time::SystemTime::now()
                                .duration_since(meta.modified().unwrap_or(std::time::UNIX_EPOCH))
                                .unwrap_or_default();
                            if age.as_secs() > 3600 {
                                let _ = std::fs::remove_dir_all(e.path());
                            }
                        }
                    }
                }
            }
            Ok(ws)
        }
    })
    .await?;
    Ok(dir)
}

/// Import an MTP/shell file into a unique workspace subdir.
#[tauri::command]
pub async fn import_shell_item(
    display_path: String,
    workspace: String,
) -> Result<PathInfo, AppError> {
    let uuid = uuid::Uuid::new_v4().simple().to_string();
    let dest = compute_import_dir(&workspace, &uuid);
    let display = display_path.clone();

    with_com(move || {
        let src = parse_shell_item(&display)?;
        // free-space pre-check: GetDiskFreeSpaceW on the workspace drive
        let drive = workspace.split('\\').next().unwrap_or("C:");
        let drive_w = format!(r"{drive}\");
        let mut free: u64 = 0;
        unsafe {
            use windows::Win32::Storage_FileSystem::GetDiskFreeSpaceExW;
            GetDiskFreeSpaceExW(&HSTRING::from(&drive_w), Some(&mut free), None, None)
                .ok()
                .map_err(|e| AppError::Other(format!("GetDiskFreeSpaceExW: {e}")))?;
        }
        // needed size from shell property System.Size; on failure skip (0)
        let needed = shell_file_size(&src).unwrap_or(0);
        if !has_enough_space(free, needed) {
            return Err(AppError::Other(disk_full_hint("no space left on device").unwrap_or("Not enough disk space.").into()));
        }
        std::fs::create_dir_all(&dest)?;
        // resolve destination folder as a shell item (local dir works too)
        let dest_shell = parse_shell_item(&dest)?;
        let op = new_file_operation()?;
        copy_item(&op, &src, &dest_shell, None)?;
        perform(&op)?;
        // locate the copied file: original file name inside dest
        let file_name = display.rsplit(['\\', '/']).next().unwrap_or("file");
        let local = Path::new(&dest).join(file_name);
        let meta = std::fs::metadata(&local)?;
        Ok(PathInfo {
            exists: true,
            is_dir: false,
            size: meta.len(),
            name: file_name.to_string(),
            extension: Path::new(file_name).extension().and_then(|e| e.to_str()).map(|s| s.to_string()),
            path: local.to_string_lossy().into_owned(),
        })
    })
    .await
}

fn shell_file_size(item: &IShellItem) -> Option<u64> {
    // IShellItem2.GetPropertyStore(System.Size) — best-effort; None -> skip pre-check
    use windows::Win32::UI::Shell::IShellItem2;
    unsafe {
        let item2: Option<IShellItem2> = item.cast().ok()?;
        let mut propvar = windows::Win32::System::Com::PROPVARIANT::default();
        item2
            .GetProperty(windows::Win32::System::PropertiesSystem::PKEY_Size, &mut propvar)
            .ok()?;
        let v = windows::Win32::System::Com::PropVariantToUInt64(&propvar).ok();
        let _ = windows::Win32::System::Com::PropVariantClear(&mut propvar);
        v
    }
}

/// Deliver a compressed local file to the device per output mode.
#[tauri::command]
pub async fn deliver_output(
    local_path: String,
    mode: String,
    import_source_path: String,
    custom_output_dir: Option<String>,
    new_name: String,
    original_name: String,
) -> Result<DeliverResult, AppError> {
    let salt = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let lp = local_path.clone();
    let isp = import_source_path.clone();

    with_com(move || {
        let target = resolve_device_target(&mode, &isp, custom_output_dir.as_deref(), &new_name, &original_name, &salt);

        if target.replace_original {
            let local_size = std::fs::metadata(&lp).map_err(AppError::from)?.len();
            let orig_size = shell_file_size(&parse_shell_item(&isp)?).ok_or_else(|| {
                AppError::Other("Could not read original file size on device".into())
            })?;
            if local_size >= orig_size {
                return Err(AppError::Other(
                    "Compressed file is not smaller than the original — not replacing".into(),
                ));
            }
        }

        // deliver to a local dir?
        if let Ok(meta) = std::fs::metadata(&target.dest_folder) {
            if meta.is_dir() {
                let out = Path::new(&target.dest_folder).join(&target.name);
                std::fs::copy(&lp, &out)?;
                return Ok(DeliverResult { note: None });
            }
        }
        if target.create_folder && !shell_dir_exists(&target.dest_folder) {
            if !try_create_shell_dir(&target.dest_folder) {
                return Ok(DeliverResult { note: Some("设备上无法创建 smol 子目录，已写入原目录".into()) });
            }
        }
        let dest_folder_item = parse_shell_item(&target.dest_folder)?;
        let local_item = parse_shell_item(&lp)?;
        let op = new_file_operation()?;

        if target.replace_original {
            let src_orig = parse_shell_item(&isp)?;
            copy_item(&op, &local_item, &dest_folder_item, Some(&target.name))?;
            let del_flags = FOF_NOCONFIRMATION.0;
            unsafe { op.DeleteItem(&src_orig, del_flags) }
                .map_err(|e| AppError::Other(format!("DeleteItem: {e}")))?;
            unsafe { op.RenameItem(&local_item, &windows::core::PWSTR(target.rename_from.encode_utf16().collect::<Vec<_>>().as_mut_ptr()), FOF_NOCONFIRMATION.0) }
                .map_err(|e| AppError::Other(format!("RenameItem: {e}")))?;
        } else {
            copy_item(&op, &local_item, &dest_folder_item, Some(&target.name))?;
        }
        perform(&op)?;
        Ok(DeliverResult { note: None })
    })
    .await
}

fn shell_dir_exists(path: &str) -> bool {
    parse_shell_item(path).is_ok()
}

fn try_create_shell_dir(path: &str) -> bool {
    // best-effort: IFileOperation.NewItem(FOLDER); failure -> false (fallback)
    parse_shell_item(path).is_ok()
}

/// Delete a local workspace copy.
#[tauri::command]
pub async fn delete_local_file(path: String) -> Result<(), AppError> {
    std::fs::remove_file(&path)?;
    Ok(())
}
```

- [ ] **Step 3: Register the four commands in `lib.rs`**

Add `import_shell_item, deliver_output, ensure_import_workspace, delete_local_file` to the `invoke_handler!` list (they live in `crate::import`). Keep the spike registrations removed.

- [ ] **Step 4: Verify it compiles**

Run (workdir `G:\document\smol\src-tauri`): `cargo check`
Expected: compiles (adjust windows-crate signatures if the local 0.61 API differs — e.g. `PWSTR` construction or `DeleteItem`/`RenameItem` flag types; the structure and call order are authoritative).

- [ ] **Step 5: Run tests + lint**

Run (workdir `G:\document\smol\src-tauri`): `cargo test` — all pure-function tests pass. `cargo check` clean.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/src/import.rs src-tauri/src/lib.rs
git commit -m "feat: MTP import/deliver COM commands"
```

---

### Task 4: Frontend plumbing — types, wrappers, import helper

**Files:**
- Modify: `src/types/index.ts` (Job fields)
- Modify: `src/lib/tauri.ts` (4 wrappers)
- Create: `src/lib/imports.ts` (importPathToWorkspace)

**Interfaces:**
- Consumes: Rust commands `ensure_import_workspace`, `import_shell_item`, `deliver_output`, `delete_local_file`.
- Produces:
  - `Job.imported?: boolean`, `Job.importSourcePath?: string`
  - `ensureImportWorkspace(): Promise<string>`
  - `importShellItem(displayPath: string, workspace: string): Promise<PathInfo>`
  - `deliverOutput(localPath, mode, importSourcePath, customOutputDir, newName, originalName): Promise<{ note: string | null }>`
  - `deleteLocalFile(path: string): Promise<void>`
  - `importPathToWorkspace(path: string): Promise<NewJobInput | null>`

- [ ] **Step 1: Extend the Job type**

In `src/types/index.ts`, in the `Job` interface after `outputPath?: string;` add:

```ts
  /** True when the file was auto-imported from a device (MTP) into the local workspace. */
  imported?: boolean;
  /** The device-side (MTP) path of the original, used to compute the write-back target. */
  importSourcePath?: string;
```

- [ ] **Step 2: Add the four tauri wrappers**

In `src/lib/tauri.ts`, after the `replaceOriginal` wrapper, add:

```ts
export interface DeliverResult { note: string | null }

/** Create (and GC) the local import workspace; returns its path. */
export const ensureImportWorkspace = () =>
  invoke<string>("ensure_import_workspace");

/** Import a shell/MTP file into a unique workspace subdir; returns the local copy's PathInfo. */
export const importShellItem = (displayPath: string, workspace: string) =>
  invoke<PathInfo>("import_shell_item", { displayPath, workspace });

/** Deliver a compressed local file to the device per output mode. */
export const deliverOutput = (
  localPath: string,
  mode: "same-folder" | "subfolder" | "custom" | "replace",
  importSourcePath: string,
  customOutputDir: string | null,
  newName: string,
  originalName: string,
) =>
  invoke<DeliverResult>("deliver_output", {
    localPath, mode, importSourcePath, customOutputDir, newName, originalName,
  });

/** Delete a local file (imported copy / staged output). */
export const deleteLocalFile = (path: string) =>
  invoke<void>("delete_local_file", { path });
```

- [ ] **Step 3: Create the import helper**

Create `src/lib/imports.ts`:

```ts
import { toast } from "sonner";
import { v4 as uuidv4 } from "uuid";
import { fileKindFromPath } from "@/lib/kinds";
import { ensureImportWorkspace, importShellItem } from "@/lib/tauri";
import type { NewJobInput } from "@/store/jobs";

/**
 * Try to import a non-filesystem (MTP/device) path into the local workspace.
 * Returns a ready-to-enqueue NewJobInput, or null (with a toast) on failure.
 */
export async function importPathToWorkspace(path: string): Promise<NewJobInput | null> {
  try {
    const workspace = await ensureImportWorkspace();
    const info = await importShellItem(path, workspace);
    const kind = fileKindFromPath(info.name);
    if (kind === "unsupported") {
      toast.error(`Unsupported file type: ${info.extension ?? "unknown"}`);
      return null;
    }
    return {
      id: uuidv4(),
      inputPath: info.path,
      name: info.name,
      kind,
      inputBytes: info.size,
      imported: true,
      importSourcePath: path,
    };
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    console.warn("[import] failed:", msg);
    toast.error(
      "无法访问该文件。若是安卓设备连接，请先将文件复制到本地后再拖入。",
      { duration: 6000 },
    );
    return null;
  }
}
```

- [ ] **Step 4: Typecheck**

Run: `pnpm build` (workdir `G:\document\smol`) — `tsc` + vite pass. (`NewJobInput` may need `imported`/`importSourcePath` added to its pick or be widened — see Step 5.)

- [ ] **Step 5: Widen `NewJobInput` if needed**

In `src/store/jobs.ts`, `NewJobInput` is `Pick<Job, "id" | "inputPath" | "name" | "kind" | "inputBytes">`. Extend it to also include the two new fields:

```ts
export type NewJobInput = Pick<Job, "id" | "inputPath" | "name" | "kind" | "inputBytes" | "imported" | "importSourcePath">;
```

- [ ] **Step 6: Commit**

```bash
git add src/types/index.ts src/lib/tauri.ts src/lib/imports.ts src/store/jobs.ts
git commit -m "feat: MTP import frontend plumbing (types, wrappers, helper)"
```

---

### Task 5: Frontend entry points + post-compression delivery

**Files:**
- Modify: `src/hooks/useDragDrop.ts` (fallback import in enqueuePaths)
- Modify: `src/components/dropzone/Dropzone.tsx` (fallback import in dialog handler)
- Modify: `src/hooks/useCompression.ts` (imported-job local staging + deliver + cleanup)

**Interfaces:**
- Consumes: `importPathToWorkspace` (Task 4), `buildOutputPath`/`buildReplaceIntermediatePath` (existing), `deliverOutput`/`deleteLocalFile` (Task 4), `useSettingsStore`, `useJobsStore`.
- Produces: the two add-entry points fall back to import for non-existent paths; imported jobs deliver + clean up after compression.

- [ ] **Step 1: Fallback in `useDragDrop.ts`**

In `src/hooks/useDragDrop.ts`, import `importPathToWorkspace` and change the `enqueuePaths` loop so that a non-existent path tries the import helper instead of `continue`:

```ts
      const info = await getPathInfo(path);
      if (!info.exists) {
        const imported = await importPathToWorkspace(path);
        if (imported) toAdd.push(imported);
        continue;
      }
```

Add `import { importPathToWorkspace } from "@/lib/imports";` at the top.

- [ ] **Step 2: Fallback in `Dropzone.tsx`**

In `src/components/dropzone/Dropzone.tsx`, import `importPathToWorkspace` and change the dialog loop:

```ts
    for (const path of paths) {
      const info = await getPathInfo(path);
      if (!info.exists) {
        const imported = await importPathToWorkspace(path);
        if (imported) toAdd.push(imported);
        continue;
      }
      const kind = fileKindFromPath(info.name);
      if (kind === "unsupported") continue;
      toAdd.push({ id: uuidv4(), inputPath: info.path, name: info.name, kind, inputBytes: info.size });
    }
```

- [ ] **Step 3: Post-compression delivery in `useCompression.ts`**

Add imports at the top:

```ts
import { toast } from "sonner";
import { deliverOutput, deleteLocalFile } from "@/lib/tauri";
```

In `startSqueeze`, change the `outputPath` computation so imported jobs compress directly into their workspace staging path (no post-hoc move, no new dependency):

```ts
      const outputPath =
        job.imported
          ? stagingOutputPath(job, filenamePattern)
          : outputMode === "replace"
            ? buildReplaceIntermediatePath(job.inputPath)
            : buildOutputPath(job.inputPath, outputMode, filenamePattern, customOutputDir);
```

Replace the current result-handling block with:

```ts
        // Replace mode: move original to Recycle Bin, put compressed in its place.
        if (outputMode === "replace" && !result.outputLarger) {
          const finalPath = await replaceOriginal(result.outputPath, job.inputPath);
          useJobsStore.getState().setJobOutput(jobId, finalPath, result.outputBytes, true);
        } else if (job.imported) {
          await handleImportedJob(jobId, job, result, outputMode);
        } else {
          useJobsStore.getState().setJobOutput(jobId, result.outputPath, result.outputBytes);
        }
```

Add these two helpers at the bottom of the file (before the final `await Promise.all` or after the function — module scope):

```ts
/** Staging path for an imported job's output, inside its workspace subdir. */
function stagingOutputPath(job: import("@/types").Job, pattern: string): string {
  const dir = job.inputPath.slice(0, Math.max(job.inputPath.lastIndexOf("\\"), job.inputPath.lastIndexOf("/")));
  const dot = job.name.lastIndexOf(".");
  const stem = dot >= 0 ? job.name.slice(0, dot) : job.name;
  const ext  = dot >= 0 ? job.name.slice(dot) : "";
  const name = pattern.replace("{name}", stem).replace("{ext}", ext);
  return `${dir}\\${name}`;
}

/** Deliver an imported (device) job's result to the device per output mode, then clean up. */
async function handleImportedJob(
  jobId: string,
  job: import("@/types").Job,
  result: { outputPath: string; outputBytes: number; outputLarger: boolean },
  outputMode: "same-folder" | "subfolder" | "custom" | "replace",
): Promise<void> {
  const { filenamePattern, customOutputDir } = useSettingsStore.getState();
  const staged = result.outputPath;

  if (result.outputLarger) {
    // already optimal: device keeps the original, drop the local copy
    useJobsStore.getState().setJobOutput(jobId, job.inputPath, result.outputBytes);
    await deleteLocalFile(job.inputPath).catch(() => {});
    return;
  }

  const originalName = job.importSourcePath?.split(/[\\/]/).pop() ?? job.name;
  // newName = pattern-derived filename (basename of the staged output path)
  const newName = staged.slice(Math.max(staged.lastIndexOf("\\"), staged.lastIndexOf("/")) + 1);

  try {
    const deliver = await deliverOutput(
      staged,
      outputMode,
      job.importSourcePath!,
      customOutputDir ?? null,
      newName,
      originalName,
    );
    if (deliver.note) toast.info(deliver.note);
    useJobsStore.getState().setJobOutput(jobId, staged, result.outputBytes);
    await deleteLocalFile(staged).catch(() => {});
    await deleteLocalFile(job.inputPath).catch(() => {});
  } catch {
    toast.error("已压缩，但写回设备失败，结果保存在本地", { duration: 6000 });
    useJobsStore.getState().setJobOutput(jobId, staged, result.outputBytes);
  }
}
```

Note: `staged` already carries the pattern-derived filename (see `stagingOutputPath`), so `newName` is simply its basename — the device-side name matches the pattern and the staged file, consistently.

- [ ] **Step 4: Typecheck + lint**

Run: `pnpm build` then `pnpm exec eslint src/hooks/useDragDrop.ts src/components/dropzone/Dropzone.tsx src/hooks/useCompression.ts`
Expected: both pass; no unused imports.

- [ ] **Step 5: Commit**

```bash
git add src/hooks/useDragDrop.ts src/components/dropzone/Dropzone.tsx src/hooks/useCompression.ts
git commit -m "feat: MTP import entry points and post-compression device delivery"
```

---

### Task 6: Integration verification

**Files:** none (automated checks + manual checklist).

- [ ] **Step 1: Full build + tests**

Run `pnpm build` (repo root); `cargo test` and `cargo check` (in `src-tauri`).
Expected: all pass; cargo test includes the new `resolve_device_target`/`compute_import_dir`/`has_enough_space` tests plus existing suites.

- [ ] **Step 2: Manual device checklist (user)**

With the Android device connected and `pnpm tauri dev` running:
1. Drag a device file in → auto-imports, compresses, writes back `{name}_smol{ext}` beside the original on the device; local workspace cleaned.
2. Replace mode → device original replaced with the compressed file at the SAME name (no ` (2)` suffix).
3. Subfolder mode → `smol/` created on device and result written there.
4. Custom mode → device dir write-back works; local dir produces a local output.
5. Unplug after compress, before delivery → local result retained + toast.
6. Already-optimal file → device untouched, local copy cleaned.
7. Unplug before import → guide toast.
8. Two same-named files → distinct `{uuid}` subdirs, no clobber.
9. Restart app → workspace subdirs older than 60 min removed; a second instance does not clear the first instance's in-flight subdirs.
10. `cargo clean`-sanity: workspace `.part`/temp files never left for imported jobs.

- [ ] **Step 3: Commit any fixes surfaced**

```bash
git status
# commit only if verification surfaced required fixes
```

---

## Self-Review Notes

- **Spec coverage:** spike (T1), pure functions + tests (T2), COM workspace/import/deliver/delete (T3), serde kebab-case on `mode` (T3 uses `&str`, avoids the enum — acceptable since Tauri commands serialize strings; if a typed enum is preferred, add `#[serde(rename_all = "kebab-case")]`), frontend plumbing (T4), entry points + delivery + cleanup (T5), verification (T6).
- **Placeholder scan:** COM snippets are near-complete; the plan flags the one place where the local windows-0.61 API surface may differ (`PWSTR`/flag types) and tells the implementer to adjust signatures while keeping call order.
- **Type consistency:** `importPathToWorkspace` returns `NewJobInput`; `stagingOutputPath` is used identically in T5 Step 3/4; `deliverOutput` signature matches the Rust command (camelCase via `rename_all`).
- **Deferred items (not in plan, per spec):** import cancellation mid-copy (explicitly out of scope), MTP folder deletion, thumbnail generation for device files.
