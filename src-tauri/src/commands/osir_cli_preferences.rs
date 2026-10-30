//! Narrow, optional DEV preference migration on an explicit launch. Never
//! imports authentication, trust, providers, MCP, sessions or arbitrary fields.
use std::path::{Component, Path, PathBuf};

const MAX_PREFERENCES_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ClaudePreferences {
    has_completed_onboarding: Option<bool>,
    last_onboarding_version: Option<String>,
    theme: Option<String>,
}

fn preference_fields(bytes: &[u8]) -> Option<serde_json::Map<String, serde_json::Value>> {
    let preferences: ClaudePreferences = serde_json::from_slice(bytes).ok()?;
    let mut fields = serde_json::Map::new();
    if let Some(value) = preferences.has_completed_onboarding {
        fields.insert("hasCompletedOnboarding".into(), value.into());
    }
    if let Some(value) = preferences.last_onboarding_version.filter(|value| {
        !value.is_empty()
            && value.len() <= 32
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.')
    }) {
        fields.insert("lastOnboardingVersion".into(), value.into());
    }
    if let Some(value) = preferences.theme.filter(|value| {
        matches!(
            value.as_str(),
            "light" | "dark" | "light-daltonized" | "dark-daltonized" | "light-ansi" | "dark-ansi"
        )
    }) {
        fields.insert("theme".into(), value.into());
    }
    Some(fields)
}

/// Reject links in every existing path component, including the root supplied
/// by the launcher. A missing/custom directory retains Claude's first-run UX.
fn checked_directory(directory: &Path) -> Result<(), ()> {
    if !directory.is_absolute() {
        return Err(());
    }
    let mut current = PathBuf::new();
    for component in directory.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                current.push(component.as_os_str());
            }
            Component::CurDir | Component::ParentDir => return Err(()),
        }
        // Windows drive prefixes are not independently openable directories.
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&current).map_err(|_| ())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(());
        }
    }
    Ok(())
}

fn read_preferences(path: &Path) -> Result<Option<Vec<u8>>, ()> {
    use std::io::Read;
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_PREFERENCES_BYTES
    {
        return Err(());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| ())?;
    let metadata = file.metadata().map_err(|_| ())?;
    if !metadata.is_file() || metadata.len() > MAX_PREFERENCES_BYTES {
        return Err(());
    }
    let mut bytes = Vec::new();
    file.take(MAX_PREFERENCES_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() as u64 > MAX_PREFERENCES_BYTES {
        return Err(());
    }
    Ok(Some(bytes))
}

fn migrate_claude_preferences(
    source_root: &Path,
    target_root: &Path,
    sandbox: &Path,
) -> Result<(), ()> {
    checked_directory(sandbox)?;
    checked_directory(source_root)?;
    checked_directory(target_root)?;
    if !target_root.starts_with(sandbox)
        || source_root.starts_with(sandbox)
        || source_root == target_root
    {
        return Err(());
    }
    // Neither source nor target file names can be supplied by the renderer.
    let source = source_root.join(".claude.json");
    let target = target_root.join(".claude.json");
    let bytes = read_preferences(&source)?.ok_or(())?;
    let fields = preference_fields(&bytes).ok_or(())?;
    if fields.is_empty() {
        return Ok(());
    }
    let lock_path = target_root.join(".claude.json.lock");
    // Claude Code uses the same directory-creation lock. Skipping on EEXIST
    // leaves its live process as the sole writer.
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&lock_path).map_err(|_| ())?;
    let _lock = PreferenceLock(lock_path);
    checked_directory(target_root)?;
    let original = read_preferences(&target)?;
    let mut target_value: serde_json::Value = original
        .as_deref()
        .map_or_else(|| Ok(serde_json::json!({})), serde_json::from_slice)
        .map_err(|_| ())?;
    let object = target_value.as_object_mut().ok_or(())?;
    let mut changed = false;
    for (key, value) in fields {
        if !object.contains_key(&key) {
            object.insert(key, value);
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }
    // Avoid replacing a target that a running tool updated while we prepared
    // the missing fields. The launcher's native provider switch lock is held.
    checked_directory(target_root)?;
    if read_preferences(&target)? != original {
        return Err(());
    }
    crate::config::atomic_write_private(
        &target,
        &serde_json::to_vec_pretty(&target_value).map_err(|_| ())?,
    )
    .map_err(|_| ())
}

struct PreferenceLock(PathBuf);

impl Drop for PreferenceLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

pub(super) fn preserve_on_launch(id: &str) {
    if id != "claude" || !cfg!(debug_assertions) || !crate::distribution::is_isolated() {
        return;
    }
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let Ok(_guard) = LOCK.try_lock() else {
        return;
    };
    let Some(source_root) = crate::session_manager::sources::claude_preferences_root() else {
        return;
    };
    let Some(sandbox) = std::env::var_os("CC_SWITCH_TEST_HOME").map(PathBuf::from) else {
        return;
    };
    let home = crate::config::get_home_dir();
    let claude_directory = crate::config::get_claude_config_dir();
    let target_root = if claude_directory == home.join(".claude") {
        home
    } else {
        claude_directory
    };
    // Missing/corrupt source preferences simply retain normal first-run UX.
    // Never replace the user's native login or bypass a project trust dialog.
    let _ = migrate_claude_preferences(&source_root, &target_root, &sandbox);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preference_allowlist_never_contains_auth_trust_or_mcp() {
        let fields = preference_fields(br#"{"hasCompletedOnboarding":true,"lastOnboardingVersion":"2.1.310","theme":"light","oauthAccount":{"token":"synthetic"},"apiKey":"synthetic","projects":{"/fixture":{"hasTrustDialogAccepted":true}},"mcpServers":{"fixture":{}}}"#).unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields["hasCompletedOnboarding"], true);
        assert!(!fields.contains_key("oauthAccount"));
        assert!(!fields.contains_key("projects"));
        assert!(!fields.contains_key("mcpServers"));
    }

    #[test]
    fn migration_preserves_existing_sandbox_state_and_does_not_reimport() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source_root = root.join("source");
        let sandbox = root.join("sandbox");
        std::fs::create_dir(&source_root).unwrap();
        std::fs::create_dir(&sandbox).unwrap();
        let target_root = sandbox.join(".claude");
        std::fs::create_dir(&target_root).unwrap();
        let source = source_root.join(".claude.json");
        let target = target_root.join(".claude.json");
        std::fs::write(&source, r#"{"hasCompletedOnboarding":true,"lastOnboardingVersion":"2.1.310","theme":"light","oauthAccount":"synthetic"}"#).unwrap();
        std::fs::write(
            &target,
            r#"{"theme":"dark","mcpServers":{"fixture":{}},"localOnly":true}"#,
        )
        .unwrap();
        migrate_claude_preferences(&source_root, &target_root, &sandbox).unwrap();
        let bytes = std::fs::read(&target).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["theme"], "dark");
        assert_eq!(value["hasCompletedOnboarding"], true);
        assert_eq!(value["localOnly"], true);
        assert!(value.get("oauthAccount").is_none());
        assert!(value["mcpServers"].get("fixture").is_some());
        migrate_claude_preferences(&source_root, &target_root, &sandbox).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), bytes);
    }

    #[cfg(unix)]
    #[test]
    fn migration_rejects_symlinks_and_targets_outside_isolated_home() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let sandbox = root.join("sandbox");
        let outside = root.join("outside");
        let source = root.join("source");
        std::fs::create_dir(&sandbox).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::create_dir(&source).unwrap();
        std::fs::write(
            source.join(".claude.json"),
            r#"{"hasCompletedOnboarding":true,"lastOnboardingVersion":"2.1.310","theme":"light"}"#,
        )
        .unwrap();

        assert!(migrate_claude_preferences(&source, &outside, &sandbox).is_err());

        symlink(&outside, sandbox.join("linked-target")).unwrap();
        assert!(
            migrate_claude_preferences(&source, &sandbox.join("linked-target"), &sandbox).is_err()
        );

        let linked_source = root.join("linked-source");
        symlink(&source, &linked_source).unwrap();
        assert!(migrate_claude_preferences(&linked_source, &sandbox, &sandbox).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn migration_rejects_symlink_preferences_files() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source_root = root.join("source");
        let sandbox = root.join("sandbox");
        std::fs::create_dir(&source_root).unwrap();
        std::fs::create_dir(&sandbox).unwrap();
        let outside_file = root.join("outside.json");
        std::fs::write(
            &outside_file,
            r#"{"hasCompletedOnboarding":true,"lastOnboardingVersion":"2.1.310","theme":"light"}"#,
        )
        .unwrap();
        symlink(&outside_file, source_root.join(".claude.json")).unwrap();
        assert!(migrate_claude_preferences(&source_root, &sandbox, &sandbox).is_err());
        std::fs::remove_file(source_root.join(".claude.json")).unwrap();
        std::fs::copy(&outside_file, source_root.join(".claude.json")).unwrap();
        symlink(&outside_file, sandbox.join(".claude.json")).unwrap();
        let original = std::fs::read(&outside_file).unwrap();
        assert!(migrate_claude_preferences(&source_root, &sandbox, &sandbox).is_err());
        assert_eq!(std::fs::read(&outside_file).unwrap(), original);
        assert!(!sandbox.join(".claude.json.lock").exists());
    }

    #[test]
    fn missing_preferences_copy_only_three_fields_with_private_atomic_write() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source_root = root.join("source");
        let sandbox = root.join("sandbox");
        std::fs::create_dir(&source_root).unwrap();
        std::fs::create_dir(&sandbox).unwrap();
        let source = source_root.join(".claude.json");
        let target = sandbox.join(".claude.json");
        let original = br#"{"hasCompletedOnboarding":true,"lastOnboardingVersion":"2.1.310","theme":"light","oauthAccount":{"token":"synthetic"},"projects":{"/fixture":{"hasTrustDialogAccepted":true}},"mcpServers":{"fixture":{}}}"#;
        std::fs::write(&source, original).unwrap();
        migrate_claude_preferences(&source_root, &sandbox, &sandbox).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 3);
        assert_eq!(value["hasCompletedOnboarding"], true);
        assert_eq!(value["lastOnboardingVersion"], "2.1.310");
        assert_eq!(value["theme"], "light");
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert_eq!(std::fs::read_dir(&sandbox).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn existing_false_null_and_locked_or_corrupt_preferences_are_left_untouched() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source_root = root.join("source");
        let sandbox = root.join("sandbox");
        std::fs::create_dir(&source_root).unwrap();
        std::fs::create_dir(&sandbox).unwrap();
        std::fs::write(
            source_root.join(".claude.json"),
            br#"{"hasCompletedOnboarding":true,"lastOnboardingVersion":"2.1.310","theme":"light"}"#,
        )
        .unwrap();
        let target = sandbox.join(".claude.json");
        let original = br#"{"hasCompletedOnboarding":false,"lastOnboardingVersion":null,"theme":"dark","oauthAccount":"sandbox-only"}"#;
        std::fs::write(&target, original).unwrap();
        migrate_claude_preferences(&source_root, &sandbox, &sandbox).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), original);

        std::fs::create_dir(sandbox.join(".claude.json.lock")).unwrap();
        assert!(migrate_claude_preferences(&source_root, &sandbox, &sandbox).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), original);
        std::fs::remove_dir(sandbox.join(".claude.json.lock")).unwrap();

        for invalid in [b"[1,2]".as_slice(), b"{broken".as_slice()] {
            std::fs::write(&target, invalid).unwrap();
            assert!(migrate_claude_preferences(&source_root, &sandbox, &sandbox).is_err());
            assert_eq!(std::fs::read(&target).unwrap(), invalid);
            assert!(!sandbox.join(".claude.json.lock").exists());
        }
    }

    #[test]
    fn reads_reject_oversized_and_non_regular_files() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".claude.json");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_PREFERENCES_BYTES + 1).unwrap();
        assert!(read_preferences(&path).is_err());
        assert!(read_preferences(temp.path()).is_err());
    }
}
