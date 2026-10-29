//! Read-only Desktop discovery, separate from CC Switch CLI version detection.
//! Installation/update transactions will be migrated from Manager independently.

use serde::Serialize;
#[cfg(any(target_os = "macos", test))]
use std::path::PathBuf;

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DetectionStatus {
    Detected,
    NotFound,
    InspectionFailed,
    Unsupported,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopInstallation {
    pub path: String,
    pub version: Option<String>,
    pub build: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopDetection {
    pub status: DetectionStatus,
    pub comparison_complete: bool,
    pub installations: Vec<DesktopInstallation>,
    pub message: String,
    pub recovery_targets: Vec<String>,
}

// Manager identifies the Codex lineage by bundle ID, including its renamed
// ChatGPT.app. The unrelated ChatGPT Classic bundle must not be accepted.
#[cfg(any(target_os = "macos", test))]
fn from_metadata(path: String, metadata: &serde_json::Value) -> Option<DesktopInstallation> {
    if metadata.get("CFBundleIdentifier")?.as_str()? != "com.openai.codex" {
        return None;
    }
    let text = |key| {
        metadata.get(key).and_then(|value| match value {
            serde_json::Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
    };
    Some(DesktopInstallation {
        path,
        version: text("CFBundleShortVersionString"),
        build: text("CFBundleVersion"),
    })
}

// Injectable transport keeps fixture tests away from real installations.
#[cfg(any(target_os = "macos", test))]
fn detect_in_roots(
    roots: &[PathBuf],
    mut read_metadata: impl FnMut(&std::path::Path) -> Result<serde_json::Value, ()>,
) -> DesktopDetection {
    let mut installations = Vec::new();
    let mut inspection_failed = false;
    let mut seen = std::collections::HashSet::new();
    for root in roots {
        for name in ["Codex.app", "ChatGPT.app"] {
            let path = root.join(name);
            let plist = path.join("Contents/Info.plist");
            match path.try_exists() {
                Ok(false) => continue,
                Err(_) => {
                    inspection_failed = true;
                    continue;
                }
                Ok(true) => {}
            }
            let canonical = match path.canonicalize() {
                Ok(path) => path,
                Err(_) => {
                    inspection_failed = true;
                    continue;
                }
            };
            if !seen.insert(canonical) {
                continue;
            }
            // Existing bundles with missing/invalid metadata are not "absent".
            match read_metadata(&plist) {
                Ok(metadata)
                    if metadata
                        .get("CFBundleIdentifier")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|id| !id.trim().is_empty()) =>
                {
                    if let Some(installation) =
                        from_metadata(path.to_string_lossy().into(), &metadata)
                    {
                        installations.push(installation);
                    }
                }
                _ => inspection_failed = true,
            }
        }
    }
    let (status, message) = if !installations.is_empty() {
        (
            DetectionStatus::Detected,
            if inspection_failed {
                "已检测到 Desktop，但部分候选安装无法读取；此检测仅检查本地元数据，未验证签名。"
            } else {
                "已读取本机 Desktop 元数据；此检测未验证签名。"
            },
        )
    } else if inspection_failed {
        (
            DetectionStatus::InspectionFailed,
            "部分安装目录无法读取，暂不能确定 Desktop 安装状态。",
        )
    } else {
        (
            DetectionStatus::NotFound,
            "标准应用目录中未发现 Codex Desktop；CLI 版本在下方单独检测。",
        )
    };
    DesktopDetection {
        status,
        comparison_complete: !inspection_failed,
        installations,
        message: message.into(),
        recovery_targets: Vec::new(),
    }
}

#[cfg(target_os = "macos")]
pub fn detect() -> DesktopDetection {
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = std::env::var_os("HOME") {
        let applications = PathBuf::from(home).join("Applications");
        roots.push(applications.join("OSIR Codex"));
        roots.push(applications);
    }
    detect_in_roots(&roots, |plist| {
        let metadata = std::fs::symlink_metadata(plist).map_err(|_| ())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 64 * 1024 {
            return Err(());
        }
        let mut command = std::process::Command::new("/usr/bin/plutil");
        command
            .args(["-convert", "json", "-o", "-", "--"])
            .arg(plist)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
        let output =
            bounded_metadata_output(&mut command, std::time::Duration::from_secs(1), 512 * 1024)?;
        serde_json::from_slice(&output).map_err(|_| ())
    })
}

#[cfg(any(target_os = "macos", test))]
fn bounded_metadata_output(
    command: &mut std::process::Command,
    timeout: std::time::Duration,
    limit: u64,
) -> Result<Vec<u8>, ()> {
    use std::{io::Read, process::Stdio, time::Instant};
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ())?;
    let stdout = child.stdout.take().ok_or(())?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ())?;
        if bytes.len() as u64 > limit {
            return Err(());
        }
        Ok(bytes)
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(());
            }
        }
    };
    let bytes = reader.join().map_err(|_| ())??;
    status?.success().then_some(bytes).ok_or(())
}

#[cfg(not(target_os = "macos"))]
pub fn detect() -> DesktopDetection {
    DesktopDetection {
        status: DetectionStatus::Unsupported,
        comparison_complete: false,
        installations: vec![],
        message: "当前平台的 Desktop 检测服务尚未接入；CLI 检测可单独使用。".into(),
        recovery_targets: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(unix)]
    #[test]
    fn metadata_transport_bounds_output_and_terminates_timed_out_processes() {
        use std::{
            process::Command,
            time::{Duration, Instant},
        };
        assert_eq!(
            bounded_metadata_output(
                Command::new("/usr/bin/printf").arg("metadata"),
                Duration::from_secs(1),
                64
            )
            .unwrap(),
            b"metadata"
        );
        assert!(bounded_metadata_output(
            Command::new("/usr/bin/printf").arg("too much metadata"),
            Duration::from_secs(1),
            4
        )
        .is_err());
        let started = Instant::now();
        assert!(bounded_metadata_output(
            Command::new("/bin/sh").args(["-c", "exec /bin/sleep 2"]),
            Duration::from_millis(40),
            64
        )
        .is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn missing_candidates_do_not_read_metadata() {
        let home = tempfile::tempdir().unwrap();
        let result = detect_in_roots(&[home.path().to_owned()], |_| {
            panic!("absent bundles must not invoke metadata transport")
        });
        assert_eq!(result.status, DetectionStatus::NotFound);
        assert!(result.comparison_complete);
        assert!(result.installations.is_empty());
    }

    #[test]
    fn incomplete_or_unreadable_bundle_is_not_reported_absent() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("Codex.app")).unwrap();
        for metadata in [Err(()), Ok(json!({})), Ok(json!({"CFBundleIdentifier":42}))] {
            let result = detect_in_roots(&[home.path().to_owned()], |_| metadata.clone());
            assert_eq!(result.status, DetectionStatus::InspectionFailed);
            assert!(!result.comparison_complete);
            assert!(result.installations.is_empty());
        }
    }

    #[test]
    fn duplicate_roots_are_inspected_once_and_classic_is_excluded() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("Codex.app")).unwrap();
        std::fs::create_dir(home.path().join("ChatGPT.app")).unwrap();
        let mut calls = 0;
        let result = detect_in_roots(&[home.path().to_owned(), home.path().to_owned()], |path| {
            calls += 1;
            Ok(if path.starts_with(home.path().join("Codex.app")) {
                json!({"CFBundleIdentifier":"com.openai.codex","CFBundleShortVersionString":" 1.2.3 "})
            } else {
                json!({"CFBundleIdentifier":"com.openai.chat"})
            })
        });
        assert_eq!(calls, 2);
        assert_eq!(result.status, DetectionStatus::Detected);
        assert!(result.comparison_complete);
        assert_eq!(result.installations.len(), 1);
        assert_eq!(result.installations[0].version.as_deref(), Some("1.2.3"));
        assert!(result.installations[0].build.is_none());
    }

    #[test]
    fn partial_failure_preserves_detected_installation_and_warning() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("Codex.app")).unwrap();
        std::fs::create_dir(home.path().join("ChatGPT.app")).unwrap();
        let result = detect_in_roots(&[home.path().to_owned()], |path| {
            if path.starts_with(home.path().join("Codex.app")) {
                Ok(json!({"CFBundleIdentifier":"com.openai.codex"}))
            } else {
                Err(())
            }
        });
        assert_eq!(result.status, DetectionStatus::Detected);
        assert!(!result.comparison_complete);
        assert_eq!(result.installations.len(), 1);
        assert!(result.message.contains("部分候选安装无法读取"));
    }

    #[test]
    fn renamed_codex_matches_identity_not_display_name() {
        let result = from_metadata(
            "/Applications/ChatGPT.app".into(),
            &json!({
                "CFBundleIdentifier": "com.openai.codex",
                "CFBundleShortVersionString": "1.2.3",
                "CFBundleVersion": 123
            }),
        )
        .unwrap();
        assert_eq!(result.version.as_deref(), Some("1.2.3"));
        assert_eq!(result.build.as_deref(), Some("123"));
        assert!(from_metadata(
            "/Applications/ChatGPT.app".into(),
            &json!({"CFBundleIdentifier": "com.openai.chat"})
        )
        .is_none());
    }

    #[test]
    fn absent_versions_remain_unknown() {
        let result = from_metadata(
            "/Applications/Codex.app".into(),
            &json!({
                "CFBundleIdentifier": "com.openai.codex", "CFBundleVersion": ""
            }),
        )
        .unwrap();
        assert!(result.version.is_none());
        assert!(result.build.is_none());
        assert!(from_metadata("unknown".into(), &json!({})).is_none());
    }
}
