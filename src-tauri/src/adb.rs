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

/// Parse one `ls -la` line into an entry. The name is everything after the
/// size column and the date columns, REJOINED with spaces — so names with
/// spaces survive intact (the classic bug is taking only the last token).
/// The date is 2 tokens when the month is ISO (`2024-01-01 10:00` /
/// `2023-07-15 2023`) and 3 tokens when month-name format is used
/// (`Jan 1 10:00` / `Jan 1 2023`); we disambiguate by the first date token.
pub fn parse_ls_line(line: &str) -> Option<DeviceEntry> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() || line.starts_with("total") {
        return None;
    }
    let cols: Vec<&str> = line.split_whitespace().collect();
    // perms links owner group size + date + name (>= 8 columns)
    if cols.len() < 8 {
        return None;
    }
    let perms = cols[0];
    let size: u64 = cols[4].parse().ok()?;
    // ISO date token (digits/dashes) → date is 2 tokens, name starts at 7;
    // month-name date → 3 tokens, name starts at 8.
    let name_start = if cols[5].contains('-') || cols[5].parse::<u32>().is_ok() {
        7
    } else {
        8
    };
    if cols.len() <= name_start {
        return None;
    }
    let name = cols[name_start..].join(" ");
    if name == "." || name == ".." || name.is_empty() {
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

/// Tail of stderr, trimmed, last non-empty line.
fn stderr_tail(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .last()
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdbProgressEvent {
    /// "pull" | "push"
    pub operation: String,
    /// The file being transferred (device path for pull, target for push).
    pub file: String,
    /// 0-100
    pub percent: u8,
}

/// Find the next `[ NN%]` token in `text` starting at `from`. Returns the byte
/// index just past the token and the percent. Handles adb's in-place `\r`
/// redraws by scanning raw text, not lines.
pub fn next_percent(text: &str, from: usize) -> Option<(usize, u8)> {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            let mut j = i + 1;
            // adb right-aligns the percent with leading spaces: `[  9%]`.
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            let dstart = j;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > dstart && j < bytes.len() && bytes[j] == b'%' {
                let p = text[dstart..j].parse::<u8>().ok()?;
                return Some((j + 1, p));
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

/// Spawn `cmd`, stream stderr, scan for `[ NN%]` progress tokens (adb redraws
/// in place with `\r`, so we scan raw bytes rather than lines) and emit events
/// via `on_progress` (deduped per percent step). Returns the exit status plus
/// the trailing stderr text for error reporting.
fn spawn_adb_progress(
    cmd: &mut std::process::Command,
    operation: String,
    file: String,
    on_progress: &tauri::ipc::Channel<AdbProgressEvent>,
) -> Result<(std::process::ExitStatus, String), AppError> {
    use std::io::Read;

    let mut child = cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| AppError::Other(format!("spawn adb: {e}")))?;

    let mut stderr = child.stderr.take().expect("stderr piped");
    let mut raw: Vec<u8> = Vec::new();
    let mut last_pct: i8 = -1;
    let ch = on_progress.clone();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stderr
            .read(&mut chunk)
            .map_err(|e| AppError::Other(format!("adb stderr read: {e}")))?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..n]);
        if raw.len() > 8192 {
            raw.drain(..raw.len() - 8192);
        }
        let text = String::from_utf8_lossy(&raw);
        let mut from = 0;
        while let Some((next, p)) = next_percent(&text, from) {
            from = next;
            let p = p as i8;
            if p != last_pct {
                last_pct = p;
                let _ = ch.send(AdbProgressEvent {
                    operation: operation.clone(),
                    file: file.clone(),
                    percent: p as u8,
                });
            }
        }
    }

    let status = child
        .wait()
        .map_err(|e| AppError::Other(format!("adb wait: {e}")))?;
    Ok((status, String::from_utf8_lossy(&raw).trim().to_string()))
}

/// Build a useful adb error message. Connection problems (no device / offline /
/// unauthorized) get the USB-debugging guidance; otherwise the device-side
/// stderr tail (e.g. "Permission denied") is surfaced so the real cause shows.
fn adb_error(stderr: &[u8], context: &str) -> AppError {
    let tail = stderr_tail(stderr);
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
        cmd.args(["shell", &format!("ls -la '{escaped}'")]);
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
    /// False when this item failed (e.g. a large-file transfer that dropped).
    pub ok: bool,
    /// Actionable failure message, present when `ok` is false.
    pub error: Option<String>,
}

/// Actionable message for a failed pull. Large-file transfers that drop
/// mid-way are almost always a USB-level disconnect, so include the remedies.
fn pull_failure_message(remote: &str, stderr: &[u8]) -> String {
    let tail = stderr_tail(stderr);
    let base = if tail.is_empty() {
        format!("拉取 {remote} 失败")
    } else {
        format!("拉取 {remote} 失败：{tail}")
    };
    format!(
        "{base}（若传输中途断开，请检查数据线/USB 接口，关闭 Windows 的 USB 选择性暂停，并在传输期间保持手机唤醒后重试）"
    )
}

#[tauri::command]
pub async fn pull_device_files(
    items: Vec<String>,
    workspace: String,
    on_progress: tauri::ipc::Channel<AdbProgressEvent>,
) -> Result<Vec<PullResult>, AppError> {
    let pulled = tauri::async_runtime::spawn_blocking(move || -> Result<Vec<PullResult>, AppError> {
        ensure_device_ready()?;
        let mut out = Vec::new();
        let mut all_failed: Option<String> = None;
        for remote in items {
            let uuid = uuid::Uuid::new_v4().simple().to_string();
            let dest = format!("{workspace}\\{uuid}");
            let name = remote.rsplit('/').next().unwrap_or("file").to_string();
            let local = Path::new(&dest).join(&name);
            let mut cmd = adb_cmd()?;
            cmd.args(["pull", &remote]).arg(&local);
            let (status, err_text) =
                spawn_adb_progress(&mut cmd, "pull".into(), remote.clone(), &on_progress)?;
            if !status.success() {
                // A failed item must not abort the batch: drop the empty uuid
                // dir it created, record the failure, and continue.
                let _ = std::fs::remove_dir_all(&dest);
                let msg = pull_failure_message(&remote, err_text.as_bytes());
                all_failed.get_or_insert_with(|| msg.clone());
                out.push(PullResult {
                    key: uuid,
                    local_path: String::new(),
                    name,
                    size: 0,
                    remote_path: remote,
                    ok: false,
                    error: Some(msg),
                });
                continue;
            }
            let size = std::fs::metadata(&local).map(|m| m.len()).unwrap_or(0);
            out.push(PullResult {
                key: uuid,
                local_path: local.to_string_lossy().into_owned(),
                name,
                size,
                remote_path: remote,
                ok: true,
                error: None,
            });
        }
        if out.iter().all(|r| !r.ok) {
            // Everything failed: surface the last actionable message.
            let msg = all_failed.unwrap_or_else(|| "adb pull 失败".to_string());
            return Err(AppError::Other(msg));
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
    on_progress: tauri::ipc::Channel<AdbProgressEvent>,
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
                    let (status, err_text) = spawn_adb_progress(
                        &mut cmd,
                        "push".into(),
                        target.clone(),
                        &on_progress,
                    )?;
                    if !status.success() {
                        return Err(adb_error(err_text.as_bytes(), "adb push 失败"));
                    }
                    Ok(DeliverResult {
                        note: None,
                        path: Some(target),
                    })
                }
                "replace" => {
                    let mut cmd = adb_cmd()?;
                    cmd.args(["push"]).arg(&local_path).arg(&remote_path);
                    let (status, err_text) = spawn_adb_progress(
                        &mut cmd,
                        "push".into(),
                        remote_path.clone(),
                        &on_progress,
                    )?;
                    if !status.success() {
                        return Err(adb_error(err_text.as_bytes(), "adb push 失败"));
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
    fn preserves_names_with_spaces() {
        let e = parse_ls_line("drwxrwx--x 2 root sdcard_rw 4096 2024-01-01 10:00 My Files").unwrap();
        assert!(e.is_dir);
        assert_eq!(e.name, "My Files");
        let f = parse_ls_line("-rw-rw---- 1 root sdcard_rw 1234 2024-01-01 10:00 happy vacation.mp4").unwrap();
        assert!(!f.is_dir);
        assert_eq!(f.name, "happy vacation.mp4");
    }

    #[test]
    fn parses_month_name_date() {
        let e = parse_ls_line("-rw-rw---- 1 root sdcard_rw 99 Jan  1  2023 old.txt").unwrap();
        assert_eq!(e.name, "old.txt");
        assert_eq!(e.size, 99);
    }

    #[test]
    fn skips_dot_entries_and_total() {
        let out = parse_ls_output(
            "total 128\ndrwxrwx--x 1 root sdcard_rw 4096 2024-01-01 10:00 .\ndrwxrwx--x 1 root sdcard_rw 4096 2024-01-01 10:00 ..\n-rw-rw---- 1 root sdcard_rw 123 2024-01-01 10:00 a.txt",
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "a.txt");
    }

    #[test]
    fn tolerates_odd_lines() {
        assert!(parse_ls_line("").is_none());
        assert!(parse_ls_line("total 128").is_none());
        assert!(parse_ls_line("garbage").is_none());
        assert!(parse_ls_output("garbage\n").is_empty());
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

    #[test]
    fn scans_adb_progress_percents() {
        // adb redraws with \r in place; scanning must find every token.
        let text = "\r[  9%] /sdcard/a.mp4\r[ 10%] /sdcard/a.mp4\r[ 50%]  123/456\r[100%] 456/456\n";
        let mut percents = Vec::new();
        let mut from = 0;
        while let Some((next, p)) = next_percent(text, from) {
            from = next;
            percents.push(p);
        }
        assert_eq!(percents, vec![9, 10, 50, 100]);
    }

    #[test]
    fn next_percent_ignores_non_matches() {
        assert_eq!(next_percent("no brackets here", 0), None);
        assert_eq!(next_percent("[99] no percent sign", 0), None);
        assert_eq!(next_percent("[] empty", 0), None);
    }
}
