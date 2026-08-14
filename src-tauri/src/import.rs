use std::path::Path;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use crate::error::{disk_full_hint, AppError};
use windows::core::{Interface, HSTRING, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Storage::EnhancedStorage::PKEY_Size;
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToUInt64};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    BHID_SFObject, FOF_NOCONFIRMATION, FOLDERID_Documents, FOS_ALLOWMULTISELECT, FileOpenDialog,
    FileOperation, IFileOpenDialog, IFileOperation, ILFree, ILGetSize, IShellFolder, IShellItem,
    IShellItem2, IShellItemArray, KNOWN_FOLDER_FLAG, SHCreateItemFromIDList, SHGetIDListFromObject,
    SHGetKnownFolderPath, SHParseDisplayName, SIGDN, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_FILESYSPATH,
};

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

/// Free-space pre-check. `needed_bytes == 0` means "unknown" — allow (skip).
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

/// Run `f` inside a dedicated COM-initialized (STA) blocking task.
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

/// Resolve a shell item's display name for the given SIGDN format, freeing the
/// CoTaskMemAlloc'd buffer. Returns `None` when the name cannot be resolved.
fn display_name(item: &IShellItem, sigdn: SIGDN) -> Option<String> {
    unsafe {
        let p = item.GetDisplayName(sigdn).ok()?;
        let s = p.to_string().ok();
        let _ = CoTaskMemFree(Some(p.as_ptr() as *const _));
        s
    }
}

/// Capture the raw bytes of an item's absolute PIDL (`ILGetSize` length).
fn item_pidl(item: &IShellItem) -> Result<Vec<u8>, AppError> {
    unsafe {
        let pidl = SHGetIDListFromObject(item)
            .map_err(|e| AppError::Other(format!("SHGetIDListFromObject: {e}")))?;
        let size = ILGetSize(Some(pidl));
        let bytes = std::slice::from_raw_parts(pidl as *const u8, size as usize).to_vec();
        ILFree(Some(pidl));
        Ok(bytes)
    }
}

fn new_file_operation() -> Result<IFileOperation, AppError> {
    unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL) }
        .map_err(|e| AppError::Other(format!("CoCreateInstance IFileOperation: {e}")))
}

fn perform(op: &IFileOperation) -> Result<(), AppError> {
    // 0.61 has no FOFX_NOCONFIRMATION; FOF_NOCONFIRMATION suffices.
    unsafe { op.SetOperationFlags(FOF_NOCONFIRMATION) }
        .map_err(|e| AppError::Other(format!("SetOperationFlags: {e}")))?;
    unsafe { op.PerformOperations() }
        .map_err(|e| AppError::Other(format!("PerformOperations: {e}")))
}

fn copy_item(op: &IFileOperation, src: &IShellItem, dest: &IShellItem, name: Option<&str>) -> Result<(), AppError> {
    let name_buf = name.map(|n| n.encode_utf16().collect::<Vec<u16>>());
    let pcw = name_buf.as_ref().map(|v| PCWSTR(v.as_ptr()));
    unsafe { op.CopyItem(src, Some(dest), pcw.as_ref(), None) }
        .map_err(|e| AppError::Other(format!("CopyItem: {e}")))
}

/// Documents directory (e.g. `C:\Users\<name>\Documents`).
fn documents_dir() -> Result<String, AppError> {
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Documents, KNOWN_FOLDER_FLAG(0), None)
            .map_err(|e| AppError::Other(format!("SHGetKnownFolderPath: {e}")))?;
        let path = p.to_string().unwrap_or_default();
        let _ = CoTaskMemFree(Some(p.as_ptr() as *const _));
        Ok(path)
    }
}

fn import_workspace_dir() -> Result<String, AppError> {
    Ok(format!(r"{}\Smol\imports", documents_dir()?))
}

/// Resolve a filesystem path (or any display name) to an `IShellItem`.
fn parse_shell_item(path: &str) -> Result<IShellItem, AppError> {
    unsafe {
        let name = HSTRING::from(path);
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        SHParseDisplayName(&name, None, &mut pidl, 0, None)
            .map_err(|e| AppError::Other(format!("SHParseDisplayName failed for {path}: {e}")))?;
        let item = SHCreateItemFromIDList::<IShellItem>(pidl)
            .map_err(|e| AppError::Other(format!("SHCreateItemFromIDList failed for {path}: {e}")))?;
        ILFree(Some(pidl));
        Ok(item)
    }
}

/// Size of a shell item via `IShellItem2::GetProperty(PKEY_Size)`.
fn shell_item_size(item: &IShellItem) -> Option<u64> {
    unsafe {
        let item2: IShellItem2 = item.cast().ok()?;
        let mut pv = item2.GetProperty(&PKEY_Size).ok()?;
        let v = PropVariantToUInt64(&pv).ok();
        let _ = PropVariantClear(&mut pv);
        v
    }
}

/// Create/obtain a child shell item (e.g. the `smol` folder) under `parent`.
fn create_child_folder(parent: &IShellItem, child_name: &str) -> Result<IShellItem, AppError> {
    unsafe {
        let folder: IShellFolder = parent
            .BindToHandler(None, &BHID_SFObject)
            .map_err(|e| AppError::Other(format!("BindToHandler IShellFolder: {e}")))?;
        let name = HSTRING::from(child_name);
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        folder
            .ParseDisplayName(HWND::default(), None, &name, None, &mut pidl, std::ptr::null_mut())
            .map_err(|e| AppError::Other(format!("ParseDisplayName({child_name}): {e}")))?;
        let child = SHCreateItemFromIDList::<IShellItem>(pidl)
            .map_err(|e| AppError::Other(format!("SHCreateItemFromIDList: {e}")))?;
        ILFree(Some(pidl));
        Ok(child)
    }
}

/// Resolve `original_name` inside the parent folder captured by `pidl_bytes`.
fn parse_shell_item_original(original_name: &str, pidl_bytes: &[u8]) -> Result<IShellItem, AppError> {
    unsafe {
        let parent = SHCreateItemFromIDList::<IShellItem>(pidl_bytes.as_ptr() as *const ITEMIDLIST)
            .map_err(|e| AppError::Other(format!("SHCreateItemFromIDList: {e}")))?;
        let folder: IShellFolder = parent
            .BindToHandler(None, &BHID_SFObject)
            .map_err(|e| AppError::Other(format!("BindToHandler IShellFolder: {e}")))?;
        let name = HSTRING::from(original_name);
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        folder
            .ParseDisplayName(HWND::default(), None, &name, None, &mut pidl, std::ptr::null_mut())
            .map_err(|e| AppError::Other(format!("ParseDisplayName({original_name}): {e}")))?;
        let item = SHCreateItemFromIDList::<IShellItem>(pidl)
            .map_err(|e| AppError::Other(format!("SHCreateItemFromIDList: {e}")))?;
        ILFree(Some(pidl));
        Ok(item)
    }
}

/// Create & return the import workspace; clean subdirs older than 60 min.
#[tauri::command]
pub async fn ensure_import_workspace() -> Result<String, AppError> {
    let ws = with_com(|| {
        let ws = import_workspace_dir()?;
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

        unsafe { dialog.SetOptions(FOS_ALLOWMULTISELECT) }
            .map_err(|e| AppError::Other(format!("SetOptions: {e}")))?;

        let hr = unsafe { dialog.Show(None) };
        if hr.is_err() { return Ok(Vec::new()); } // user cancelled

        let results: IShellItemArray = unsafe { dialog.GetResults() }
            .map_err(|e| AppError::Other(format!("GetResults: {e}")))?;

        let count = unsafe { results.GetCount() }.map_err(|e| AppError::Other(format!("GetCount: {e}")))?;
        let mut out: Vec<PickResult> = Vec::new();

        // free-space pre-check once
        let mut free: u64 = 0;
        let drive = "C:\\";
        unsafe {
            GetDiskFreeSpaceExW(&HSTRING::from(drive), Some(&mut free), None, None)
                .map_err(|e| AppError::Other(format!("GetDiskFreeSpaceExW: {e}")))?;
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

        // custom — local output only
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
            SHCreateItemFromIDList::<IShellItem>(pidl_bytes.as_ptr() as *const ITEMIDLIST)
        }
        .map_err(|e| AppError::Other(format!("SHCreateItemFromIDList: {e}")))?;

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
                unsafe { op.DeleteItem(&original_item, None) }
                    .map_err(|e| AppError::Other(format!("DeleteItem: {e}")))?;
                let rename = HSTRING::from(&target.rename_from);
                unsafe { op.RenameItem(&local_item, &rename, None) }
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

/// Remove a local file (e.g. a workspace temp import).
#[tauri::command]
pub fn delete_local_file(path: String) -> Result<(), AppError> {
    std::fs::remove_file(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
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
}
