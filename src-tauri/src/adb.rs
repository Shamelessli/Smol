use std::path::{Path, PathBuf};
use serde::Serialize;
use tauri::Manager;
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

/// Parse one `ls -aF` line into an entry. `ls -F` appends `/` to directories
/// and `*` to executables, and prints one entry per line — so names with
/// spaces survive intact (no whitespace-column guessing). Returns None for
/// `.`/`..`/empty/odd lines. Size is not available in this mode (0).
pub fn parse_ls_line(line: &str) -> Option<DeviceEntry> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() {
        return None;
    }
    let (name, is_dir) = if let Some(n) = line.strip_suffix('/') {
        (n, true)
    } else {
        let n = line.strip_suffix('*').unwrap_or(line);
        (n, false)
    };
    if name == "." || name == ".." || name.is_empty() {
        return None;
    }
    Some(DeviceEntry {
        name: name.to_string(),
        is_dir,
        size: 0,
    })
}

pub fn parse_ls_output(output: &str) -> Vec<DeviceEntry> {
    output.lines().filter_map(parse_ls_line).collect()
}

/// Parse `adb devices` output into (serial, state) pairs. Skips the header
/// line and ignores blank/garbage rows.
pub fn parse_devices_output(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .skip(1) // "List of devices attached"
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            match (it.next(), it.next()) {
                (Some(serial), Some(state)) => {
                    Some((serial.to_string(), state.to_string()))
                }
                _ => None,
            }
        })
        .collect()
}

/// Run `adb devices` and return (serial, state) pairs.
fn adb_device_states() -> Result<Vec<(String, String)>, AppError> {
    let mut cmd = adb_cmd()?;
    cmd.args(["devices"]);
    let out = cmd
        .output()
        .map_err(|e| AppError::Other(format!("adb devices failed: {e}")))?;
    if !out.status.success() {
        return Err(AppError::Other("adb devices 失败".into()));
    }
    Ok(parse_devices_output(&String::from_utf8_lossy(&out.stdout)))
}

/// Ensure at least one device is ready (`device` state) before a command that
/// needs it. Recovers a stale adb server (kill-server, then the next adb call
/// auto-starts a fresh one) once before giving up. Returns a targeted error for
/// the `unauthorized` state so the UI can tell the user to accept the phone's
/// USB-debugging prompt.
fn ensure_device_ready() -> Result<(), AppError> {
    let any_device = || -> Result<bool, AppError> {
        Ok(adb_device_states()?.iter().any(|(_, s)| s == "device"))
    };
    if any_device()? {
        return Ok(());
    }
    if adb_device_states()?.iter().any(|(_, s)| s == "unauthorized") {
        return Err(AppError::Other(
            "设备未授权 — 请在手机上允许 USB 调试授权".into(),
        ));
    }
    // Stale server recovery: kill the server so the next invocation starts a
    // fresh one that re-enumerates the device.
    if let Ok(mut c) = adb_cmd() {
        let _ = c.args(["kill-server"]).status();
    }
    if any_device()? {
        return Ok(());
    }
    Err(AppError::Other(
        "未检测到设备 — 请确认设备已连接并启用 USB 调试，并在手机上允许授权".into(),
    ))
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

/// Build a useful adb error message. Connection problems (no device / offline /
/// unauthorized) get the USB-debugging guidance; otherwise the device-side
/// stderr tail (e.g. "Permission denied") is surfaced so the real cause shows.
fn adb_error(stderr: &[u8], context: &str) -> AppError {
    let tail = String::from_utf8_lossy(stderr)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .last()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let lower = tail.to_lowercase();
    let conn = lower.contains("no devices")
        || lower.contains("offline")
        || lower.contains("unauthorized");
    let msg = if conn || tail.is_empty() {
        format!("{context}：请确认设备已连接并启用 USB 调试")
    } else {
        format!("{context}：{tail}")
    };
    AppError::Other(msg)
}

#[tauri::command]
pub async fn list_device_dir(path: String) -> Result<Vec<DeviceEntry>, AppError> {
    let entries = tauri::async_runtime::spawn_blocking(move || -> Result<Vec<DeviceEntry>, AppError> {
        ensure_device_ready()?;
        let mut cmd = adb_cmd()?;
        let escaped = path.replace('\'', "'\\''");
        cmd.args(["shell", &format!("ls -aF '{escaped}'")]);
        let out = cmd
            .output()
            .map_err(|e| AppError::Other(format!("adb shell failed: {e}")))?;
        if !out.status.success() {
            return Err(adb_error(&out.stderr, &format!("无法列出目录 {path}")));
        }
        Ok(parse_ls_output(&String::from_utf8_lossy(&out.stdout)))
    })
    .await
    .map_err(|e| AppError::Other(format!("adb shell 任务失败: {e}")))?;
    Ok(entries?)
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
    let pulled = tauri::async_runtime::spawn_blocking(move || -> Result<Vec<PullResult>, AppError> {
        ensure_device_ready()?;
        let mut out = Vec::new();
        let mut last_err: Option<Vec<u8>> = None;
        for remote in items {
            let uuid = uuid::Uuid::new_v4().simple().to_string();
            let dest = format!("{workspace}\\{uuid}");
            std::fs::create_dir_all(&dest)?;
            let name = remote.rsplit('/').next().unwrap_or("file").to_string();
            let local = Path::new(&dest).join(&name);
            let mut cmd = adb_cmd()?;
            cmd.args(["pull", &remote]).arg(&local);
            let out_res = cmd
                .output()
                .map_err(|e| AppError::Other(format!("adb pull failed: {e}")))?;
            if !out_res.status.success() {
                // One bad item must not abort the whole batch: drop the empty
                // uuid dir this pull created and move on to the next item.
                let _ = std::fs::remove_dir_all(&dest);
                last_err = Some(out_res.stderr);
                continue;
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
        if out.is_empty() {
            let stderr = last_err.unwrap_or_default();
            return Err(adb_error(&stderr, "adb pull 失败"));
        }
        Ok(out)
    })
    .await
    .map_err(|e| AppError::Other(format!("adb pull 任务失败: {e}")))?;
    Ok(pulled?)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliverResult {
    pub note: Option<String>,
    pub path: Option<String>,
}

/// Return (creating it if needed) the local import workspace:
/// `Documents\Smol\imports`. Every `adb pull` stores each file in its own
/// `{uuid}` subdirectory here so staged outputs never collide.
#[tauri::command]
pub fn get_import_workspace(app: tauri::AppHandle) -> Result<String, AppError> {
    let docs = app
        .path()
        .document_dir()
        .map_err(|e| AppError::Other(format!("无法获取文档目录: {e}")))?;
    let dir = docs.join("Smol").join("imports");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.to_string_lossy().into_owned())
}

fn recovered_dir(app: &tauri::AppHandle) -> Result<PathBuf, AppError> {
    let docs = app
        .path()
        .document_dir()
        .map_err(|e| AppError::Other(format!("无法获取文档目录: {e}")))?;
    let dir = docs.join("Smol").join("recovered");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[tauri::command]
pub async fn deliver_to_device(
    app: tauri::AppHandle,
    local_path: String,
    mode: String,
    remote_path: String,
    remote_dir: Option<String>,
    pc_dir: Option<String>,
    new_name: String,
) -> Result<DeliverResult, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let run = || -> Result<DeliverResult, AppError> {
            ensure_device_ready()?;
            match mode.as_str() {
                "pc-folder" => {
                    let dir = pc_dir.ok_or_else(|| AppError::Other("未选择 PC 输出目录".into()))?;
                    std::fs::create_dir_all(&dir)?;
                    let target = Path::new(&dir).join(&new_name);
                    std::fs::copy(&local_path, &target)?;
                    Ok(DeliverResult {
                        note: None,
                        path: Some(target.to_string_lossy().into_owned()),
                    })
                }
                "android-folder" => {
                    let dir = remote_dir.ok_or_else(|| AppError::Other("未选择设备目标目录".into()))?;
                    let target = format!("{}/{}", dir.trim_end_matches('/'), new_name);
                    let mut cmd = adb_cmd()?;
                    cmd.args(["push"]).arg(&local_path).arg(&target);
                    let out = cmd
                        .output()
                        .map_err(|e| AppError::Other(format!("adb push failed: {e}")))?;
                    if !out.status.success() {
                        return Err(adb_error(&out.stderr, "adb push 失败"));
                    }
                    Ok(DeliverResult {
                        note: None,
                        path: Some(target),
                    })
                }
                "replace" => {
                    let mut cmd = adb_cmd()?;
                    cmd.args(["push"]).arg(&local_path).arg(&remote_path);
                    let out = cmd
                        .output()
                        .map_err(|e| AppError::Other(format!("adb push failed: {e}")))?;
                    if !out.status.success() {
                        return Err(adb_error(&out.stderr, "adb push 失败"));
                    }
                    Ok(DeliverResult {
                        note: None,
                        path: Some(remote_path),
                    })
                }
                _ => return Err(AppError::Other(format!("未知交付模式: {mode}"))),
            }
        };
        match run() {
            Ok(r) => Ok(r),
            Err(e) => {
                let base = e.to_string();
                let msg = match recovered_dir(&app) {
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
    })
    .await
    .map_err(|e| AppError::Other(format!("adb push 任务失败: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_regular_file() {
        let e = parse_ls_line("IMG_1.jpg").unwrap();
        assert_eq!(e.name, "IMG_1.jpg");
        assert!(!e.is_dir);
    }

    #[test]
    fn parses_directory_with_slash_indicator() {
        let e = parse_ls_line("DCIM/").unwrap();
        assert!(e.is_dir);
        assert_eq!(e.name, "DCIM");
    }

    #[test]
    fn preserves_names_with_spaces() {
        let e = parse_ls_line("My Files/").unwrap();
        assert!(e.is_dir);
        assert_eq!(e.name, "My Files");
        let f = parse_ls_line("happy vacation.mp4").unwrap();
        assert!(!f.is_dir);
        assert_eq!(f.name, "happy vacation.mp4");
    }

    #[test]
    fn parses_executable_indicator() {
        let e = parse_ls_line("script.sh*").unwrap();
        assert!(!e.is_dir);
        assert_eq!(e.name, "script.sh");
    }

    #[test]
    fn skips_dot_entries() {
        let out = parse_ls_output("./\n../\nDCIM/\na.txt");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "DCIM");
        assert_eq!(out[1].name, "a.txt");
    }

    #[test]
    fn tolerates_odd_lines() {
        assert!(parse_ls_line("").is_none());
        assert!(parse_ls_line("/").is_none()); // strip_suffix('/') → empty name
        // a non-indicator line is a plain file whose name may contain spaces
        let e = parse_ls_line("garbage that has no indicator").unwrap();
        assert_eq!(e.name, "garbage that has no indicator");
        assert!(!e.is_dir);
    }

    #[test]
    fn parses_device_states() {
        let out = parse_devices_output(
            "List of devices attached\nR58M123ABC\tdevice\n1234abcd\toffline\n",
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], ("R58M123ABC".to_string(), "device".to_string()));
        assert_eq!(out[1], ("1234abcd".to_string(), "offline".to_string()));
    }

    #[test]
    fn parses_no_devices_as_empty() {
        assert!(parse_devices_output("List of devices attached\n\n").is_empty());
    }
}
