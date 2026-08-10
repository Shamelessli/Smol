use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error, Serialize)]
#[serde(tag = "kind", content = "message")]
pub enum AppError {
    #[error("IO error: {0}")]
    Io(String),

    #[error("Path does not exist: {0}")]
    PathDoesNotExist(String),

    #[allow(dead_code)] // Phase 4
    #[error("{0}")]
    Other(String),
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::Io(e.to_string())
    }
}

/// Return a friendly hint when an error string looks like a disk-full failure.
pub fn disk_full_hint(err: &str) -> Option<&'static str> {
    const HINT: &str = "Not enough disk space to write the output. Free up space and retry.";
    const PATTERNS: &[&str] = &[
        "no space left on device",
        "not enough space",
        "insufficient space",
        "error writing file",
        "error writing output",
        "failed to write",
        "disk full",
        "enospc",
        "空间不足",
    ];
    let lower = err.to_lowercase();
    PATTERNS.iter().any(|p| lower.contains(p)).then_some(HINT)
}

#[cfg(test)]
mod tests {
    use super::disk_full_hint;

    const HINT: &str = "Not enough disk space to write the output. Free up space and retry.";

    #[test]
    fn detects_english_no_space_left() {
        assert_eq!(
            disk_full_hint("error writing output file: No space left on device"),
            Some(HINT)
        );
    }

    #[test]
    fn detection_is_case_insensitive() {
        assert_eq!(disk_full_hint("NO SPACE LEFT ON DEVICE"), Some(HINT));
    }

    #[test]
    fn detects_chinese_disk_full() {
        assert_eq!(disk_full_hint("写入失败：磁盘空间不足"), Some(HINT));
    }

    #[test]
    fn unrelated_error_returns_none() {
        assert_eq!(disk_full_hint("Invalid data found when processing input"), None);
    }
}
