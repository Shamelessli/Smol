# MTP 安卓设备压缩支持 Implementation Plan (PIDL pivot)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users compress files from a USB-connected Android (MTP) device: a custom shell picker selects the device file, imports it to a local workspace, it compresses, and the result is delivered back to the device per output mode.

**Architecture:** One custom `IFileOpenDialog` command `pick_import` does the pick + in-session import to a unique workspace subdir and captures the device parent folder's **PIDL (base64 bytes)**. `deliver_output` rebuilds the parent folder shell item from that PIDL via `SHCreateItemFromIDList` (no path-string parsing — spike proved MTP parsing names don't round-trip). All COM runs in `spawn_blocking` + `CoInitializeEx(STA)` closures. Drag-drop stays local-only (WebView2 rejects MTP drags — spike-verified). rfd dialog is abandoned for the add-files flow.

**Tech Stack:** Rust (Tauri 2, `windows` 0.61 Shell/Com), React 19 + TS + Zustand.

## Global Constraints

- Windows 10/11 64-bit only. Frontend uses `@/` alias.
- Workspace `%USERPROFILE%\Documents\Smol\imports` (SHGetKnownFolderPath FOLDERID_Documents); per-import unique subdir `{workspace}\{uuid}\`; startup GC removes subdirs with mtime older than 60 minutes.
- All COM inside `spawn_blocking` with `CoInitializeEx(None, COINIT_APARTMENTTHREADED)` / `CoUninitialize()`; `IFileOperation::PerformOperations()` after queuing.
- `deliver_output` runs ALL COM in ONE `spawn_blocking` closure; replace = 3 steps in one IFileOperation: CopyItem(temp) → DeleteItem(original) → RenameItem(temp→original), where temp = `{stem}.{salt}.smol_tmp{ext}`, salt = 8-char random.
- Size hard-guard applies ONLY to `mode == "replace"`: `local_size < original_size`.
- Write-back destination is resolved from `parent_id_list_b64` (PIDL), NEVER from path strings.
- custom mode for imported jobs = local output only.
- Frontend `@tauri-apps/plugin-dialog` is NOT used for the add-files flow (rfd returns null for MTP). Drag-drop path is UNCHANGED (local-only).
- Existing compress commands and `buildOutputPath` are UNCHANGED.

---

### Task 1: Remove spike tooling (spike findings recorded)

**Files:**
- Delete: `src-tauri/src/spike.rs`
- Modify: `src-tauri/src/lib.rs` (remove `mod spike;` + spike registrations)
- Modify: `src/hooks/useDragDrop.ts` (remove TEMP SPIKE console.log block)
- Modify: `src/components/dropzone/Dropzone.tsx` (remove SPIKE pick button + handler)
- Modify: `src-tauri/Cargo.toml` (windows features — keep the base set from the spike)

**Interfaces:**
- Produces: a clean tree with no diagnostic code. Keeps the `windows` dependency with features `["Win32_Foundation", "Win32_System_Com", "Win32_UI_Shell", "Win32_Storage_FileSystem"]`.

- [ ] **Step 1: Remove `spike.rs` and registrations**

Delete `src-tauri/src/spike.rs`. In `src-tauri/src/lib.rs`, remove `mod spike;` and the `spike::` entries from `invoke_handler`.

- [ ] **Step 2: Remove frontend spike logging**

In `src/hooks/useDragDrop.ts`, delete the TEMP SPIKE block inside the drop handler (the `// TEMP SPIKE` for-loop with `[spike] delivered path` / `spike_parse_path`) and restore the callback to a plain (non-async) arrow if it no longer needs `await`. In `src/components/dropzone/Dropzone.tsx`, remove the `SPIKE pick` button and the `handleSpikePick` function.

- [ ] **Step 3: Verify**

Run (workdir `G:\document\smol\src-tauri`): `cargo check` — clean, no warnings. Run (workdir `G:\document\smol`): `pnpm build` — clean.

- [ ] **Step 4: Commit**

```bash
git add -A src-tauri/src src/hooks/useDragDrop.ts src/components/dropzone/Dropzone.tsx
git commit -m "spike: remove MTP diagnostic tooling after verification"
```

---

### Task 2: Backend pure logic — `resolve_device_target`, pidl helpers, space check + tests

**Files:**
- Create: `src-tauri/src/import.rs` (pure functions + `#[cfg(test)]` only)
- Modify: `src-tauri/src/lib.rs` (`mod import;`)

**Interfaces:**
- Produces (all `pub`):
  - `pub enum Mode { SameFolder, Subfolder, Replace, CustomLocal }` with `impl From<&str>` parsing kebab-case mode strings (`"same-folder"`, `"subfolder"`, `"replace"`, `"custom"`); unknown → `SameFolder`.
  - `pub struct DeviceTarget { pub mode: Mode, pub name: String, pub rename_from: String, pub create_subfolder: bool, pub note: Option<String> }`
  - `pub fn resolve_device_target(mode: &str, new_name: &str, original_name: &str, salt: &str) -> DeviceTarget`
  - `pub fn compute_import_dir(workspace: &str, uuid: &str) -> String`
  - `pub fn has_enough_space(free_bytes: u64, needed_bytes: u64) -> bool`
  - `pub fn pidl_bytes_to_base64(bytes: &[u8]) -> String` and `pub fn base64_to_pidl_bytes(b64: &str) -> Option<Vec<u8>>`
- Consumed by Task 3 (`pick_import`/`deliver_output`).

- [ ] **Step 1: Write the failing tests**

Append a `#[cfg(test)] mod tests` to `src-tauri/src/import.rs`:

```rust
use super::*;

#[test]
fn mode_from_str_parses_kebab_case() {
    assert!(matches!(Mode::from("same-folder"), Mode::SameFolder));
    assert!(matches!(Mode::from("subfolder"), Mode::Subfolder));
    assert!(matches!(Mode::from("replace"), Mode::Replace));
    assert!(matches!(Mode::from("custom"), Mode::CustomLocal));
    assert!(matches!(Mode::from("bogus"), Mode::SameFolder));
}

#[test]
fn resolve_same_folder_uses_new_name() {
    let t = resolve_device_target("same-folder", "IMG_smol.jpg", "IMG.jpg", "s");
    assert!(matches!(t.mode, Mode::SameFolder));
    assert_eq!(t.name, "IMG_smol.jpg");
    assert!(t.rename_from.is_empty());
}

#[test]
fn resolve_subfolder_sets_create_flag() {
    let t = resolve_device_target("subfolder", "IMG_smol.jpg", "IMG.jpg", "s");
    assert!(matches!(t.mode, Mode::Subfolder));
    assert!(t.create_subfolder);
}

#[test]
fn resolve_replace_uses_salted_temp_and_rename() {
    let t = resolve_device_target("replace", "IMG_smol.jpg", "IMG.jpg", "a1b2c3d4");
    assert!(matches!(t.mode, Mode::Replace));
    assert_eq!(t.name, "IMG.a1b2c3d4.smol_tmp.jpg");
    assert_eq!(t.rename_from, "IMG.jpg");
}

#[test]
fn resolve_custom_is_local() {
    let t = resolve_device_target("custom", "IMG_smol.jpg", "IMG.jpg", "s");
    assert!(matches!(t.mode, Mode::CustomLocal));
}

#[test]
fn compute_import_dir_joins_workspace_and_uuid() {
    assert_eq!(compute_import_dir(r"C:\Smol\imports", "abc-123"), r"C:\Smol\imports\abc-123");
}

#[test]
fn has_enough_space_rules() {
    assert!(has_enough_space(1024, 512));
    assert!(!has_enough_space(512, 1024));
    assert!(has_enough_space(0, 0)); // unknown size -> skip
}

#[test]
fn pidl_base64_round_trips() {
    let bytes = vec![0x14u8, 0x00, 0x1f, 0x50, 0x00, 0x00];
    let b64 = pidl_bytes_to_base64(&bytes);
    assert_eq!(base64_to_pidl_bytes(&b64).unwrap(), bytes);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run (workdir `G:\document\smol\src-tauri`): `cargo test import::`
Expected: FAIL — functions not defined.

- [ ] **Step 3: Implement the pure functions**

Add `mod import;` to `src-tauri/src/lib.rs`; create `src-tauri/src/import.rs`:

```rust
use base64::{engine::general_purpose::STANDARD, Engine as _};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode { SameFolder, Subfolder, Replace, CustomLocal }

impl From<&str> for Mode {
    fn from(s: &str) -> Self {
        match s {
            "subfolder" => Mode::Subfolder,
            "replace"   => Mode::Replace,
            "custom"    => Mode::CustomLocal,
            _           => Mode::SameFolder,
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct DeviceTarget {
    pub mode: Mode,
    pub name: String,
    pub rename_from: String,
    pub create_subfolder: bool,
    pub note: Option<String>,
}

/// Map an output mode to the device-side delivery plan (pure, unit-tested).
pub fn resolve_device_target(mode: &str, new_name: &str, original_name: &str, salt: &str) -> DeviceTarget {
    match Mode::from(mode) {
        Mode::Replace => {
            let stem = original_name.rsplit_once('.').map(|(s, _)| s).unwrap_or(original_name);
            let ext  = original_name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
            DeviceTarget {
                mode: Mode::Replace,
                name: if ext.is_empty() { format!("{stem}.{salt}.smol_tmp") } else { format!("{stem}.{salt}.smol_tmp.{ext}") },
                rename_from: original_name.to_string(),
                create_subfolder: false,
                note: None,
            }
        }
        Mode::Subfolder => DeviceTarget {
            mode: Mode::Subfolder,
            name: new_name.to_string(),
            rename_from: String::new(),
            create_subfolder: true,
            note: None,
        },
        Mode::CustomLocal => DeviceTarget {
            mode: Mode::CustomLocal,
            name: new_name.to_string(),
            rename_from: String::new(),
            create_subfolder: false,
            note: None,
        },
        Mode::SameFolder => DeviceTarget {
            mode: Mode::SameFolder,
            name: new_name.to_string(),
            rename_from: String::new(),
            create_subfolder: false,
            note: None,
        },
    }
}

/// Unique per-import workspace subdirectory.
pub fn compute_import_dir(workspace: &str, uuid: &str) -> String {
    let sep = if workspace.contains('\\') { "\\" } else { "/" };
    format!("{workspace}{sep}{uuid}")
}

/// Free-space pre-check. `needed_bytes == 0` means "unknown" → allow (skip).
pub fn has_enough_space(free_bytes: u64, needed_bytes: u64) -> bool {
    needed_bytes == 0 || free_bytes >= needed_bytes
}

/// Serialize an ITEMIDLIST's raw bytes to base64 (PIDLs are plain data).
pub fn pidl_bytes_to_base64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// Decode base64 back into PIDL raw bytes.
pub fn base64_to_pidl_bytes(b64: &str) -> Option<Vec<u8>> {
    STANDARD.decode(b64).ok()
}
```

Note: `base64` crate — if it is NOT already a dependency, add `base64 = "0.22"` to `src-tauri/Cargo.toml` (or implement a tiny hand-rolled encoder/decoder if you prefer zero deps; the crate is standard).

- [ ] **Step 4: Run tests to verify they pass**

Run (workdir `G:\document\smol\src-tauri`): `cargo test import::`
Expected: PASS — 8 tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/import.rs src-tauri/src/lib.rs src-tauri/Cargo.toml
git commit -m "feat: MTP import pure logic (mode mapping, pidl codec, workspace, space check)"
```

---

### Task 3: Backend COM commands — pick_import, deliver_output, workspace, delete

**Files:**
- Modify: `src-tauri/src/import.rs` (add COM helpers + 4 commands)
- Modify: `src-tauri/Cargo.toml` (ensure `Win32_UI_Shell` + `Win32_Storage_FileSystem` features; add `base64` if not present)
- Modify: `src-tauri/src/lib.rs` (register 4 commands)

**Interfaces:**
- Consumes: Task 2 pure functions; `crate::error::disk_full_hint`; `crate::fs_bridge::PathInfo`-shaped struct (define a local `PickResult`).
- Produces Tauri commands:
  - `ensure_import_workspace() -> Result<String, AppError>`
  - `pick_import() -> Result<Vec<PickResult>, AppError>`
  - `deliver_output(local_path: String, key: String, mode: String, custom_output_dir: Option<String>, new_name: String, original_name: String, parent_id_list_b64: String) -> Result<DeliverResult, AppError>`
  - `delete_local_file(path: String) -> Result<(), AppError>`

- [ ] **Step 1: Ensure dependencies**

In `src-tauri/Cargo.toml`, the `windows` entry should be:
```toml
windows = { version = "0.61", features = [
    "Win32_Foundation",
    "Win32_System_Com",
    "Win32_UI_Shell",
    "Win32_Storage_FileSystem",
] }
```
Add `base64 = "0.22"` if not present. (If `IShellItem2::GetProperty`/`PKEY_Size` symbols are missing, add `"Win32_UI_Shell_PropertiesSystem"`.)

- [ ] **Step 2: Add COM helpers + commands to `import.rs`**

Append to `src-tauri/src/import.rs`:

```rust
use std::path::Path;
use crate::error::{disk_full_hint, AppError};
use windows::core::HSTRING;
use windows::Win32::Foundation::BOOL;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{
    FileOpenDialog, FOLDERID_Documents, IFileOpenDialog, IFileOperation, IShellItem,
    SHCreateItemFromIDList, SHGetIDListFromObject, SHGetKnownFolderPath, SHParseDisplayName,
    SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_FILESYSPATH, FOF_NOCONFIRMATION, FOFX_NOCONFIRMATION,
};

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PickResult {
    pub is_local: bool,
    pub path: Option<String>,
    pub key: Option<String>,
    pub local_path: Option<String>,
    pub name: Option<String>,
    pub size: Option<u64>,
    pub parent_id_list_b64: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliverResult { pub note: Option<String> }

async fn with_com<T, F>(f: F) -> Result<T, AppError>
where T: Send + 'static, F: FnOnce() -> Result<T, AppError> + Send + 'static {
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

fn display_name(item: &IShellItem, sigdn: windows::Win32::UI::Shell::SIGDN) -> Option<String> {
    unsafe {
        let mut p: windows::core::PWSTR = windows::core::PWSTR::null();
        item.GetDisplayName(sigdn, &mut p).ok()?;
        let s = p.to_string().ok();
        let _ = windows::Win32::System::Com::CoTaskMemFree(p.as_ptr() as _);
        s
    }
}

fn item_pidl(item: &IShellItem) -> Result<Vec<u8>, AppError> {
    unsafe {
        let mut pidl: *const windows::Win32::UI::Shell::Common::ITEMIDLIST = std::ptr::null();
        SHGetIDListFromObject(item, &mut pidl)
            .map_err(|e| AppError::Other(format!("SHGetIDListFromObject: {e}")))?;
        let size = windows::Win32::UI::Shell::ILGetSize(pidl);
        let bytes = std::slice::from_raw_parts(pidl as *const u8, size).to_vec();
        let _ = windows::Win32::UI::Shell::ILFree(Some(pidl));
        Ok(bytes)
    }
}

fn new_file_operation() -> Result<IFileOperation, AppError> {
    unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL) }
        .map_err(|e| AppError::Other(format!("CoCreateInstance IFileOperation: {e}")))
}

fn perform(op: &IFileOperation) -> Result<(), AppError> {
    unsafe { op.SetOperationFlags(FOF_NOCONFIRMATION.0 | FOFX_NOCONFIRMATION.0) }
        .map_err(|e| AppError::Other(format!("SetOperationFlags: {e}")))?;
    unsafe { op.PerformOperations() }
        .map_err(|e| AppError::Other(format!("PerformOperations: {e}")))
}

fn copy_item(op: &IFileOperation, src: &IShellItem, dest: &IShellItem, name: Option<&str>) -> Result<(), AppError> {
    let name_ptr = name.map(|n| {
        let v: Vec<u16> = n.encode_utf16().collect();
        v
    });
    let pw = name_ptr.as_ref().map(|v| windows::core::PWSTR(v.as_ptr() as *mut _));
    unsafe { op.CopyItem(src, Some(dest), pw.as_ref().map(|p| p.0), None) }
        .map_err(|e| AppError::Other(format!("CopyItem: {e}")))
}
```

Then the four commands:

```rust
/// Create & return the import workspace; clean subdirs older than 60 min.
#[tauri::command]
pub async fn ensure_import_workspace() -> Result<String, AppError> {
    let ws = with_com(|| {
        unsafe {
            let mut raw = std::ptr::null_mut();
            SHGetKnownFolderPath(&FOLDERID_Documents, 0, None, &mut raw)
                .map_err(|e| AppError::Other(format!("SHGetKnownFolderPath: {e}")))?;
            let path = windows::core::PWSTR(raw).to_string().unwrap_or_default();
            windows::Win32::System::Com::CoTaskMemFree(raw as _);
            let ws = format!(r"{path}\Smol\imports");
            std::fs::create_dir_all(&ws)?;
            if let Ok(entries) = std::fs::read_dir(&ws) {
                for e in entries.flatten() {
                    if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        if let Ok(meta) = e.metadata() {
                            let age = std::time::SystemTime::now()
                                .duration_since(meta.modified().unwrap_or(std::time::UNIX_EPOCH))
                                .unwrap_or_default();
                            if age.as_secs() > 3600 { let _ = std::fs::remove_dir_all(e.path()); }
                        }
                    }
                }
            }
            Ok(ws)
        }
    }).await?;
    Ok(ws)
}

/// Open the shell picker; import MTP selections to the workspace, capture parent PIDL.
#[tauri::command]
pub async fn pick_import() -> Result<Vec<PickResult>, AppError> {
    with_com(|| {
        let dialog: IFileOpenDialog = unsafe {
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_ALL)
        }.map_err(|e| AppError::Other(format!("CoCreateInstance FileOpenDialog: {e}")))?;

        unsafe { dialog.SetOptions(windows::Win32::UI::Shell::Common::FOS_ALLOWMULTISELECT.0) }
            .map_err(|e| AppError::Other(format!("SetOptions: {e}")))?;

        let hr = unsafe { dialog.Show(None) };
        if hr.is_err() { return Ok(Vec::new()); } // user cancelled

        let results: windows::Win32::UI::Shell::IShellItemArray = unsafe { dialog.GetResults() }
            .map_err(|e| AppError::Other(format!("GetResults: {e}")))?;

        let count = unsafe { results.GetCount() }.map_err(|e| AppError::Other(format!("GetCount: {e}")))?;
        let mut out: Vec<PickResult> = Vec::new();

        // free-space pre-check once
        let mut free: u64 = 0;
        let drive = "C:\\";
        unsafe {
            use windows::Win32::Storage_FileSystem::GetDiskFreeSpaceExW;
            GetDiskFreeSpaceExW(&HSTRING::from(drive), Some(&mut free), None, None)
                .ok().map_err(|e| AppError::Other(format!("GetDiskFreeSpaceExW: {e}")))?;
        }

        for i in 0..count {
            let item: IShellItem = unsafe { results.GetItemAt(i) }
                .map_err(|e| AppError::Other(format!("GetItemAt: {e}")))?;

            // local file?
            if let Some(fspath) = display_name(&item, SIGDN_FILESYSPATH) {
                out.push(PickResult {
                    is_local: true,
                    path: Some(fspath), key: None, local_path: None, name: None, size: None, parent_id_list_b64: None,
                });
                continue;
            }

            // MTP item: import into a unique workspace subdir
            let name = display_name(&item, SIGDN_DESKTOPABSOLUTEPARSING)
                .and_then(|p| p.rsplit(['\\', '/']).next().map(|s| s.to_string()))
                .unwrap_or_else(|| format!("file-{i}"));
            let needed = shell_item_size(&item).unwrap_or(0);
            if !has_enough_space(free, needed) {
                return Err(AppError::Other(
                    disk_full_hint("no space left on device").unwrap_or("Not enough disk space.").into(),
                ));
            }

            let workspace = import_workspace_dir()?;
            let uuid = uuid::Uuid::new_v4().simple().to_string();
            let dest = compute_import_dir(&workspace, &uuid);
            std::fs::create_dir_all(&dest)?;
            let dest_item = parse_shell_item(&dest)?;
            let op = new_file_operation()?;
            copy_item(&op, &item, &dest_item, Some(&name))?;
            perform(&op)?;

            let local_path = Path::new(&dest).join(&name);
            let size = std::fs::metadata(&local_path).map(|m| m.len()).unwrap_or(0);

            // parent folder PIDL (write-back destination)
            let parent_item: IShellItem = unsafe { item.GetParent() }
                .map_err(|e| AppError::Other(format!("GetParent: {e}")))?;
            let pidl = item_pidl(&parent_item)?;
            let b64 = pidl_bytes_to_base64(&pidl);

            out.push(PickResult {
                is_local: false,
                path: None,
                key: Some(uuid),
                local_path: Some(local_path.to_string_lossy().into_owned()),
                name: Some(name),
                size: Some(size),
                parent_id_list_b64: Some(b64),
            });
        }
        Ok(out)
    }).await
}
```

Implement the helpers referenced above (`import_workspace_dir` returns the same Documents path as `ensure_import_workspace` — factor the SHGetKnownFolderPath block into a shared `fn documents_dir() -> Result<String, AppError>` used by both; `shell_item_size` uses `IShellItem2::GetProperty(PKEY_Size)` → `PropVariantToUInt64`; `parse_shell_item` uses `SHParseDisplayName`).

```rust
fn documents_dir() -> Result<String, AppError> {
    unsafe {
        let mut raw = std::ptr::null_mut();
        SHGetKnownFolderPath(&FOLDERID_Documents, 0, None, &mut raw)
            .map_err(|e| AppError::Other(format!("SHGetKnownFolderPath: {e}")))?;
        let path = windows::core::PWSTR(raw).to_string().unwrap_or_default();
        windows::Win32::System::Com::CoTaskMemFree(raw as _);
        Ok(path)
    }
}
fn import_workspace_dir() -> Result<String, AppError> {
    Ok(format!(r"{}\Smol\imports", documents_dir()?))
}
fn parse_shell_item(path: &str) -> Result<IShellItem, AppError> {
    unsafe {
        let name = HSTRING::from(path);
        let mut item: Option<IShellItem> = None;
        SHParseDisplayName(&name, None, &mut item, 0, None)
            .map_err(|e| AppError::Other(format!("SHParseDisplayName failed for {path}: {e}")))?;
        item.ok_or_else(|| AppError::Other(format!("No shell item for {path}")))
    }
}
fn shell_item_size(item: &IShellItem) -> Option<u64> {
    use windows::Win32::UI::Shell::IShellItem2;
    unsafe {
        let item2: Option<IShellItem2> = item.cast().ok()?;
        let mut propvar = windows::Win32::System::Com::PROPVARIANT::default();
        item2.GetProperty(windows::Win32::System::PropertiesSystem::PKEY_Size, &mut propvar).ok()?;
        let v = windows::Win32::System::Com::PropVariantToUInt64(&propvar).ok();
        let _ = windows::Win32::System::Com::PropVariantClear(&mut propvar);
        v
    }
}
```

And the deliver command:

```rust
/// Deliver a compressed local file to the device using the captured parent PIDL.
#[tauri::command]
pub async fn deliver_output(
    local_path: String,
    key: String,                       // workspace uuid (informational)
    mode: String,
    custom_output_dir: Option<String>,
    new_name: String,
    original_name: String,
    parent_id_list_b64: String,
) -> Result<DeliverResult, AppError> {
    let salt = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let _ = key;
    let lp = local_path.clone();

    with_com(move || {
        let target = resolve_device_target(&mode, &new_name, &original_name, &salt);

        // custom → local output only
        if target.mode == Mode::CustomLocal {
            let out_dir = custom_output_dir.ok_or_else(|| AppError::Other("custom output dir is required".into()))?;
            std::fs::create_dir_all(&out_dir)?;
            std::fs::copy(&lp, Path::new(&out_dir).join(&target.name))?;
            return Ok(DeliverResult { note: None });
        }

        // rebuild parent folder from PIDL
        let pidl_bytes = base64_to_pidl_bytes(&parent_id_list_b64)
            .ok_or_else(|| AppError::Other("invalid parent id list".into()))?;
        let parent: IShellItem = unsafe {
            SHCreateItemFromIDList(pidl_bytes.as_ptr() as *const windows::Win32::UI::Shell::Common::ITEMIDLIST, &windows::core::GUID::from_u128(0))
                .map_err(|e| AppError::Other(format!("SHCreateItemFromIDList: {e}")))
        }?;
        // NOTE: use the correct interface GUID — use windows::Win32::UI::Shell::IShellItem's IID;
        // simplest correct call is via `SHCreateItemFromIDList::<IShellItem>(pidl)` generic form if available,
        // else pass `&IShellItem::IID`. Adjust to the 0.61 signature.

        if target.mode == Mode::Replace {
            let local_size = std::fs::metadata(&lp).map_err(AppError::from)?.len();
            let orig_size = shell_item_size(&parse_shell_item_original(&original_name, &pidl_bytes)?)
                .ok_or_else(|| AppError::Other("Could not read original size on device".into()))?;
            if local_size >= orig_size {
                return Err(AppError::Other("Compressed file is not smaller than the original — not replacing".into()));
            }
        }

        // subfolder: create smol/ under parent
        let mut dest = parent.clone();
        if target.mode == Mode::Subfolder {
            match create_child_folder(&parent, "smol") {
                Ok(child) => dest = child,
                Err(_) => return Ok(DeliverResult { note: Some("设备上无法创建 smol 子目录，已写入原目录".into()) }),
            }
        }

        let local_item = parse_shell_item(&lp)?;
        let op = new_file_operation()?;

        match target.mode {
            Mode::Replace => {
                let original_item = parse_shell_item_original(&original_name, &pidl_bytes)?;
                copy_item(&op, &local_item, &dest, Some(&target.name))?;
                unsafe { op.DeleteItem(&original_item, FOF_NOCONFIRMATION.0) }
                    .map_err(|e| AppError::Other(format!("DeleteItem: {e}")))?;
                unsafe { op.RenameItem(&local_item, &windows::core::PWSTR(target.rename_from.encode_utf16().collect::<Vec<_>>().as_mut_ptr()), FOF_NOCONFIRMATION.0) }
                    .map_err(|e| AppError::Other(format!("RenameItem: {e}")))?;
            }
            _ => {
                copy_item(&op, &local_item, &dest, Some(&target.name))?;
            }
        }
        perform(&op)?;
        Ok(DeliverResult { note: None })
    }).await
}
```

Implement the helpers referenced: `create_child_folder(parent: &IShellItem, child_name: &str) -> Result<IShellItem, AppError>` (bind parent to `IShellFolder`, `ParseDisplayName` the child name → PIDL → `SHCreateItemFromIDList`); `parse_shell_item_original(original_name, pidl_bytes)` — the original file lives IN the parent folder, so resolve it via the parent folder's `IShellFolder::ParseDisplayName(original_name)`.

- [ ] **Step 3: Register the four commands in `lib.rs`**

Add `ensure_import_workspace, pick_import, deliver_output, delete_local_file` (module `crate::import`) to `invoke_handler`.

- [ ] **Step 4: Verify it compiles**

Run (workdir `G:\document\smol\src-tauri`): `cargo check`
Expected: compiles. Adjust windows-0.61 signatures where the local API differs (e.g. `SHCreateItemFromIDList` generic vs PIDL+riid form, `FOS_ALLOWMULTISELECT` location, `SIGDN`/`PWSTR` types) — keep the call structure and ordering authoritative.

- [ ] **Step 5: Run tests + full check**

Run (workdir `G:\document\smol\src-tauri`): `cargo test` (Task 2 pure tests + existing) and `cargo check` — clean.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/src/import.rs src-tauri/src/lib.rs
git commit -m "feat: MTP pick_import/deliver_output COM commands (PIDL-based write-back)"
```

---

### Task 4: Frontend plumbing — types, wrappers

**Files:**
- Modify: `src/types/index.ts` (Job fields)
- Modify: `src/lib/tauri.ts` (wrappers + `PickResult` type)
- Modify: `src/store/jobs.ts` (`NewJobInput` pick widen)

**Interfaces:**
- Consumes: Rust commands `ensure_import_workspace`, `pick_import`, `deliver_output`, `delete_local_file`.
- Produces:
  - `Job.imported?: boolean`, `Job.importParentIdListB64?: string`
  - `PickResult` interface; `pickImport(): Promise<PickResult[]>`; `ensureImportWorkspace(): Promise<string>`; `deliverOutput(...)`; `deleteLocalFile(path)`
  - `NewJobInput` widened to include `imported`/`importParentIdListB64`

- [ ] **Step 1: Extend the Job type**

In `src/types/index.ts`, in the `Job` interface after `outputPath?: string;` add:

```ts
  /** True when the file was auto-imported from a device (MTP) into the local workspace. */
  imported?: boolean;
  /** Base64 of the device parent folder's shell PIDL — the write-back destination. */
  importParentIdListB64?: string;
```

- [ ] **Step 2: Add wrappers**

In `src/lib/tauri.ts`, after `replaceOriginal`, add:

```ts
export interface PickResult {
  isLocal: boolean;
  path: string | null;
  key: string | null;
  localPath: string | null;
  name: string | null;
  size: number | null;
  parentIdListB64: string | null;
}

export interface DeliverResult { note: string | null }

/** Create (and GC) the local import workspace; returns its path. */
export const ensureImportWorkspace = () =>
  invoke<string>("ensure_import_workspace");

/** Open the shell picker and import device files to the workspace. */
export const pickImport = () =>
  invoke<PickResult[]>("pick_import");

/** Deliver a compressed local file to the device (PIDL-based write-back). */
export const deliverOutput = (
  localPath: string,
  key: string,
  mode: "same-folder" | "subfolder" | "custom" | "replace",
  customOutputDir: string | null,
  newName: string,
  originalName: string,
  parentIdListB64: string,
) =>
  invoke<DeliverResult>("deliver_output", {
    localPath, key, mode, customOutputDir, newName, originalName, parentIdListB64,
  });

/** Delete a local file (imported copy / staged output). */
export const deleteLocalFile = (path: string) =>
  invoke<void>("delete_local_file", { path });
```

- [ ] **Step 3: Widen `NewJobInput`**

In `src/store/jobs.ts`:

```ts
export type NewJobInput = Pick<Job, "id" | "inputPath" | "name" | "kind" | "inputBytes" | "imported" | "importParentIdListB64">;
```

- [ ] **Step 4: Typecheck**

Run: `pnpm build` (workdir `G:\document\smol`) — tsc + vite pass.

- [ ] **Step 5: Commit**

```bash
git add src/types/index.ts src/lib/tauri.ts src/store/jobs.ts
git commit -m "feat: MTP frontend plumbing (types, pick_import/deliver wrappers)"
```

---

### Task 5: Frontend entry point + post-compression delivery

**Files:**
- Modify: `src/components/dropzone/Dropzone.tsx` (`handleOpenDialog` → use `pickImport`)
- Modify: `src/hooks/useDragDrop.ts` (NO change — local-only; verify nothing references the removed spike block)
- Modify: `src/hooks/useCompression.ts` (imported-job staging + deliver + cleanup)

**Interfaces:**
- Consumes: `pickImport`, `deliverOutput`, `deleteLocalFile` (Task 4), `stagingOutputPath` helper, `useSettingsStore`, `useJobsStore`.
- Produces: the Open-files dialog uses the custom picker; imported jobs deliver + clean up after compression.

- [ ] **Step 1: Rewrite `handleOpenDialog` in `Dropzone.tsx`**

Replace the `open({...})` (plugin-dialog) call with `pickImport`. Remove the `open` import from `@tauri-apps/plugin-dialog` if it becomes unused (check other usages first — `OutputControls.tsx` uses `open` for the directory picker; the Dropzone import can be dropped):

```tsx
  async function handleOpenDialog() {
    const results = await pickImport().catch(() => null);
    if (!results) return;

    const toAdd: NewJobInput[] = [];

    for (const r of results) {
      if (r.isLocal) {
        const kind = fileKindFromPath(r.path ?? "");
        if (kind === "unsupported" || !r.path) continue;
        const info = await getPathInfo(r.path);
        if (!info.exists) continue;
        toAdd.push({ id: uuidv4(), inputPath: r.path, name: info.name, kind, inputBytes: info.size });
      } else {
        const kind = fileKindFromPath(r.name ?? "");
        if (kind === "unsupported" || !r.localPath || !r.parentIdListB64) continue;
        toAdd.push({
          id: uuidv4(),
          inputPath: r.localPath,
          name: r.name!,
          kind,
          inputBytes: r.size ?? 0,
          imported: true,
          importParentIdListB64: r.parentIdListB64,
        });
      }
    }

    if (toAdd.length > 0) {
      useJobsStore.getState().addFiles(toAdd);
    }
  }
```

Add `import { pickImport } from "@/lib/tauri";` and `import type { NewJobInput } from "@/store/jobs";` (already imported). Remove the `open` import from `@tauri-apps/plugin-dialog` only if unused.

- [ ] **Step 2: Verify drag-drop is local-only**

In `src/hooks/useDragDrop.ts`, confirm the TEMP SPIKE block was removed (Task 1) and the handler is back to the original behavior. No changes needed here.

- [ ] **Step 3: Post-compression delivery in `useCompression.ts`**

Add imports:

```ts
import { toast } from "sonner";
import { deliverOutput, deleteLocalFile } from "@/lib/tauri";
```

Change the `outputPath` computation so imported jobs compress directly into their workspace staging path:

```ts
      const outputPath =
        job.imported
          ? stagingOutputPath(job, filenamePattern)
          : outputMode === "replace"
            ? buildReplaceIntermediatePath(job.inputPath)
            : buildOutputPath(job.inputPath, outputMode, filenamePattern, customOutputDir);
```

Replace the result-handling block with:

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

Add the helpers at module scope (bottom of file):

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

/** Deliver an imported (device) job's result to the device, then clean up. */
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

  const key = job.inputPath.slice(0, Math.max(job.inputPath.lastIndexOf("\\"), job.inputPath.lastIndexOf("/")))
    .split(/[\\/]/).pop() ?? "";
  const originalName = job.name;
  const newName = staged.slice(Math.max(staged.lastIndexOf("\\"), staged.lastIndexOf("/")) + 1);

  try {
    const deliver = await deliverOutput(
      staged,
      key,
      outputMode,
      customOutputDir ?? null,
      newName,
      originalName,
      job.importParentIdListB64!,
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

Note: `key` = the workspace uuid (basename of the workspace subdir, which is the last path segment of `job.inputPath`'s parent). `originalName` = `job.name` (the imported file's name equals the device file's name, since import preserves it). `newName` = basename of the staged output (pattern-derived).

- [ ] **Step 4: Typecheck + lint**

Run: `pnpm build` then `pnpm exec eslint src/components/dropzone/Dropzone.tsx src/hooks/useCompression.ts src/hooks/useDragDrop.ts`
Expected: both pass; no unused imports.

- [ ] **Step 5: Commit**

```bash
git add src/components/dropzone/Dropzone.tsx src/hooks/useCompression.ts
git commit -m "feat: MTP custom picker entry point and post-compression device delivery"
```

---

### Task 6: Integration verification

**Files:** none (automated checks + manual checklist).

- [ ] **Step 1: Full build + tests**

Run `pnpm build` (repo root); `cargo test` and `cargo check` (in `src-tauri`).
Expected: all pass.

- [ ] **Step 2: Manual device checklist (user)**

With the Android device connected and `pnpm tauri dev` running:
1. Open files via the custom picker → pick a device file → imports, compresses, writes back `{name}_smol{ext}` beside the original on the device; local workspace cleaned.
2. Pick a LOCAL file → behaves exactly as before (no workspace copy).
3. Replace mode → device original replaced at the SAME name (no ` (2)` suffix).
4. Subfolder mode → `smol/` created on device, result written there; if creation fails → fallback to same folder + toast.
5. Custom mode → result written to the local custom dir.
6. Unplug after compress, before delivery → local result retained + toast.
7. Already-optimal file → device untouched, local copy cleaned.
8. Two same-named device files → distinct `{uuid}` subdirs, no clobber.
9. Restart app → subdirs older than 60 min removed; second instance does not clear the first's in-flight subdirs.

- [ ] **Step 3: Commit any fixes surfaced**

```bash
git status
# commit only if verification surfaced required fixes
```

---

## Self-Review Notes

- **Spec coverage:** spike removal (T1), pure logic + pidl codec + tests (T2), COM pick_import/deliver/workspace/delete (T3), serde camelCase on PickResult/DeliverResult, frontend plumbing (T4), picker entry + delivery + cleanup (T5), verification (T6).
- **Placeholder scan:** COM snippets are near-complete with flagged API-surface notes (windows 0.61 `SHCreateItemFromIDList` form, `FOS_ALLOWMULTISELECT` location, `IShellItem::IID`). Implementers adjust signatures while keeping call structure.
- **Type consistency:** `PickResult`/`DeliverResult` camelCase matches Rust `rename_all`; `deliverOutput` arg order matches the Rust command; `stagingOutputPath` used consistently; `key` derivation documented.
- **Deferred (per spec):** drag-drop MTP (impossible), import cancellation mid-copy, MTP folder deletion, device-directory custom mode (local-only).
