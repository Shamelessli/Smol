use std::path::{Path, PathBuf};
use serde::Serialize;
use crate::error::AppError;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

/// Parse one `ls -la` line. Never fails the caller — returns None on odd lines.
pub fn parse_ls_line(line: &str) -> Option<DeviceEntry> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() || line.starts_with("total") {
        return None;
    }
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
    if name == "." || name == ".." {
        return None;
    }
    Some(DeviceEntry {
        name,
        is_dir: perms.starts_with('d'),
        size,
    })
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
        if bundled.exists() {
            return Ok(bundled);
        }
    }
    // Fallback: adb installed on PATH (e.g. Android Studio platform-tools).
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("adb.exe");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    Err(AppError::Other(
        "adb not found — run scripts/fetch-adb.mjs".into(),
    ))
}

#[tauri::command]
pub async fn list_device_dir(path: String) -> Result<Vec<DeviceEntry>, AppError> {
    let mut cmd = adb_cmd()?;
    let escaped = path.replace('\'', "'\\''");
    cmd.args(["shell", &format!("ls -la '{escaped}'")]);
    let out = cmd
        .output()
        .map_err(|e| AppError::Other(format!("adb shell failed: {e}")))?;
    if !out.status.success() {
        return Err(AppError::Other(
            "无法列出设备目录：请确认设备已连接并启用 USB 调试".into(),
        ));
    }
    Ok(parse_ls_output(&String::from_utf8_lossy(&out.stdout)))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullResult {
    pub key: String,
    pub local_path: String,
    pub name: String,
    pub size: u64,
    pub remote_path: String,
}

#[tauri::command]
pub async fn pull_device_files(
    items: Vec<String>,
    workspace: String,
) -> Result<Vec<PullResult>, AppError> {
    let mut out = Vec::new();
    for remote in items {
        let uuid = uuid::Uuid::new_v4().simple().to_string();
        let dest = format!("{workspace}\\{uuid}");
        std::fs::create_dir_all(&dest)?;
        let name = remote.rsplit('/').next().unwrap_or("file").to_string();
        let local = Path::new(&dest).join(&name);
        let mut cmd = adb_cmd()?;
        cmd.args(["pull", &remote]).arg(&local);
        let st = cmd
            .status()
            .map_err(|e| AppError::Other(format!("adb pull failed: {e}")))?;
        if !st.success() {
            return Err(AppError::Other(format!(
                "adb pull 失败：{remote}，请确认设备已连接并启用 USB 调试"
            )));
        }
        let size = std::fs::metadata(&local).map(|m| m.len()).unwrap_or(0);
        out.push(PullResult {
            key: uuid,
            local_path: local.to_string_lossy().into_owned(),
            name,
            size,
            remote_path: remote,
        });
    }
    Ok(out)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliverResult {
    pub note: Option<String>,
}

/// Return (creating it if needed) the local import workspace:
/// `Documents\Smol\imports`. Every `adb pull` stores each file in its own
/// `{uuid}` subdirectory here so staged outputs never collide.
#[tauri::command]
pub fn get_import_workspace() -> Result<String, AppError> {
    let docs = std::env::var("USERPROFILE")
        .map_err(|_| AppError::Other("USERPROFILE not set".into()))?;
    let dir = Path::new(&docs)
        .join("Documents")
        .join("Smol")
        .join("imports");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.to_string_lossy().into_owned())
}

fn recovered_dir() -> Result<PathBuf, AppError> {
    let docs = std::env::var("USERPROFILE")
        .map_err(|_| AppError::Other("USERPROFILE not set".into()))?;
    let dir = Path::new(&docs)
        .join("Documents")
        .join("Smol")
        .join("recovered");
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
                let st = cmd
                    .status()
                    .map_err(|e| AppError::Other(format!("adb push failed: {e}")))?;
                if !st.success() {
                    return Err(AppError::Other(
                        "adb push 失败：请确认设备已连接并启用 USB 调试".into(),
                    ));
                }
                Ok(DeliverResult { note: None })
            }
            "replace" => {
                let mut cmd = adb_cmd()?;
                cmd.args(["push"]).arg(&local_path).arg(&remote_path);
                let st = cmd
                    .status()
                    .map_err(|e| AppError::Other(format!("adb push failed: {e}")))?;
                if !st.success() {
                    return Err(AppError::Other(
                        "adb push 失败：请确认设备已连接并启用 USB 调试".into(),
                    ));
                }
                Ok(DeliverResult { note: None })
            }
            _ => return Err(AppError::Other(format!("未知交付模式: {mode}"))),
        }
    };
    match run() {
        Ok(r) => Ok(r),
        Err(e) => {
            let base = e.to_string();
            let msg = match recovered_dir() {
                Ok(dir) => {
                    let recovered = dir.join(&new_name);
                    if std::fs::copy(&local_path, &recovered).is_ok() {
                        format!("{base}；压缩结果已复制到 {}", recovered.display())
                    } else {
                        base
                    }
                }
                Err(_) => base,
            };
            Err(AppError::Other(msg))
        }
    }
}

#[cfg(test)]
mod tests {
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
}
