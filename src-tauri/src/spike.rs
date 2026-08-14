// TEMP SPIKE: MTP path format verification. Removed after the spike (Task 1, Step 5).
use windows::core::HSTRING;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{ILFree, SHParseDisplayName};

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
