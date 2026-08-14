// TEMP SPIKE: MTP path format verification. Removed after the spike (Task 1, Step 5).
use windows::core::HSTRING;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, IShellItem, SIGDN, SIGDN_DESKTOPABSOLUTEPARSING,
    SIGDN_FILESYSPATH, ILFree, SHParseDisplayName,
};

/// Temporary spike command: try to resolve a display path via SHParseDisplayName.
/// Returns whether the Shell API could parse the path (so we can verify
/// Tauri-delivered MTP paths parse).
#[tauri::command]
pub fn spike_parse_path(display_path: String) -> bool {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok();
        let name = HSTRING::from(&display_path);
        // windows 0.61: SHParseDisplayName yields a raw PIDL (ITEMIDLIST),
        // not an IShellItem as in later crate versions.
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        let hr = SHParseDisplayName(&name, None, &mut pidl, 0, None);
        let ok = hr.is_ok() && !pidl.is_null();
        if !pidl.is_null() {
            ILFree(Some(pidl as *const ITEMIDLIST));
        }
        CoUninitialize();
        ok
    }
}

/// Temporary spike command: report the delivered drag/drop path verbatim.
#[tauri::command]
pub fn spike_echo(path: String) -> String {
    path
}

/// TEMP SPIKE: open a native `IFileOpenDialog` (single select, any file) and,
/// for the picked item, report the shell parsing name (`SIGDN_DESKTOPABSOLUTEPARSING`),
/// the filesystem path (`SIGDN_FILESYSPATH`, expected to fail for MTP), and whether
/// `SHParseDisplayName` can re-parse the parsing name. Verifies a custom shell
/// picker can hand MTP (Android device) selections back to the frontend.
#[tauri::command]
pub async fn spike_pick() -> Vec<String> {
    tauri::async_runtime::spawn_blocking(|| {
        unsafe {
            // Matches the established spike COM pattern: initialize STA COM for
            // the duration of the dialog interaction.
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok();
            let lines = pick_via_shell_dialog();
            CoUninitialize();
            lines
        }
    })
    .await
    .unwrap_or_else(|_| vec![String::from("spawn=ERR")])
}

/// TEMP SPIKE: run the file-open dialog and format the probe results.
unsafe fn pick_via_shell_dialog() -> Vec<String> {
    unsafe {
        let dialog: IFileOpenDialog =
            match CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) {
                Ok(d) => d,
                Err(e) => return vec![format!("create={e}")],
            };
        // NULL owner hwnd is fine for a spike. Cancel (ERROR_CANCELLED) is reported.
        if let Err(e) = dialog.Show(None) {
            return vec![format!("show={e}")];
        }
        let item: IShellItem = match dialog.GetResult() {
            Ok(i) => i,
            Err(e) => return vec![format!("getresult={e}")],
        };
        vec![format_display_names(&item)]
    }
}

/// TEMP SPIKE: read both display names, re-parse the parsing name, and format
/// one probe record: `parsing=...\nfilesys=...\nparse=<true|false>`. Also probe
/// the parent folder item: whether its parsing name (`SIGDN_DESKTOPABSOLUTEPARSING`)
/// round-trips through `SHParseDisplayName` (this decides if the MTP write-back
/// step can resolve the destination FOLDER by string), appended as
/// `parentparsing=...\nparentparse=<true|false>` (both `ERR` if `GetParent` fails).
unsafe fn format_display_names(item: &IShellItem) -> String {
    unsafe {
        let parsing = read_display_name(item, SIGDN_DESKTOPABSOLUTEPARSING);
        let filesys = read_display_name(item, SIGDN_FILESYSPATH);
        // Verify the shell parsing name round-trips through SHParseDisplayName
        // (windows 0.61 yields a raw PIDL, as in spike_parse_path).
        let parse_ok = parse_round_trips(&parsing);
        let (parentparsing, parentparse) = match item.GetParent() {
            Ok(parent) => {
                let p = read_display_name(&parent, SIGDN_DESKTOPABSOLUTEPARSING);
                let p_ok = parse_round_trips(&p);
                (p, format!("{p_ok}"))
            }
            Err(e) => (format!("ERR({e})"), String::from("ERR")),
        };
        format!(
            "parsing={parsing}\nfilesys={filesys}\nparse={parse_ok}\nparentparsing={parentparsing}\nparentparse={parentparse}"
        )
    }
}

/// TEMP SPIKE: check whether `SHParseDisplayName` can re-parse a parsing name
/// string. windows 0.61 yields a raw PIDL (ITEMIDLIST), as in spike_parse_path.
unsafe fn parse_round_trips(parsing: &str) -> bool {
    unsafe {
        let name = HSTRING::from(parsing);
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        let hr = SHParseDisplayName(&name, None, &mut pidl, 0, None);
        let ok = hr.is_ok() && !pidl.is_null();
        if !pidl.is_null() {
            ILFree(Some(pidl as *const ITEMIDLIST));
        }
        ok
    }
}

/// TEMP SPIKE: call `IShellItem::GetDisplayName` and convert the returned
/// `PWSTR` to a Rust `String`, freeing the COM-allocated buffer. Failures
/// (e.g. `SIGDN_FILESYSPATH` on MTP items) are reported as `ERR(<hresult>)`.
unsafe fn read_display_name(item: &IShellItem, sigdn: SIGDN) -> String {
    unsafe {
        match item.GetDisplayName(sigdn) {
            Ok(pwstr) => {
                let text = match pwstr.to_string() {
                    Ok(s) => s,
                    Err(e) => format!("ERR({e})"),
                };
                // GetDisplayName allocates the string with CoTaskMemAlloc.
                CoTaskMemFree(Some(pwstr.as_ptr().cast::<core::ffi::c_void>()));
                text
            }
            Err(e) => format!("ERR({e})"),
        }
    }
}
