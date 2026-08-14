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
