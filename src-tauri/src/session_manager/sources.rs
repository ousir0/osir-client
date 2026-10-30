//! History discovery has its own read-only roots. Development keeps all live
//! configuration writes in the isolated home, while browsing existing history.
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Captured before distribution isolation changes HOME. These paths are used
/// only by history readers, never by provider/configuration services.
#[derive(Default)]
struct HistoryRoots {
    home: PathBuf,
    roots: BTreeMap<String, PathBuf>,
    session_dir: Option<String>,
}

static SYSTEM_ROOTS: OnceLock<HistoryRoots> = OnceLock::new();

fn expand_path(value: &str, home: &Path) -> Option<PathBuf> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let path = if value == "~" {
        home.to_path_buf()
    } else if let Some(suffix) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        home.join(suffix)
    } else {
        PathBuf::from(value)
    };
    if path.is_absolute() {
        Some(path)
    } else {
        std::env::current_dir().ok().map(|cwd| cwd.join(path))
    }
}

/// Read only directory preferences; do not initialize the mutable settings
/// cache against the real user's home or deserialize account credentials.
fn directory_preferences(home: &Path) -> BTreeMap<String, String> {
    for dir in [".osir_client", ".cc-switch"] {
        let path = home.join(dir).join("settings.json");
        let Ok(file) = std::fs::File::open(path) else {
            continue;
        };
        let mut bytes = Vec::new();
        if file.take(1024 * 1024 + 1).read_to_end(&mut bytes).is_err() || bytes.len() > 1024 * 1024
        {
            return BTreeMap::new();
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return BTreeMap::new();
        };
        return [
            "codexConfigDir",
            "claudeConfigDir",
            "geminiConfigDir",
            "grokConfigDir",
            "openclawConfigDir",
            "hermesConfigDir",
            "piConfigDir",
            "ompConfigDir",
        ]
        .into_iter()
        .filter_map(|name| {
            value
                .get(name)
                .and_then(|v| v.as_str())
                .map(|v| (name.to_owned(), v.to_owned()))
        })
        .collect();
    }
    BTreeMap::new()
}

impl HistoryRoots {
    fn resolve(
        home: &Path,
        sandbox: &Path,
        env: &BTreeMap<String, String>,
        preferences: &BTreeMap<String, String>,
    ) -> Self {
        let external = |value: &str| {
            expand_path(value, home).filter(|path| {
                !path.starts_with(sandbox)
                    && !path.starts_with(home.join(".osir-client-dev/sandbox"))
                    && !path.starts_with(home.join(".osir-client-beta/sandbox"))
            })
        };
        let env_path = |names: &[&str]| {
            names
                .iter()
                .find_map(|name| env.get(*name).and_then(|v| external(v)))
        };
        let preference = |name: &str| preferences.get(name).and_then(|v| external(v));
        let mut roots = BTreeMap::new();
        for (provider, setting, names, fallback) in [
            (
                "codex",
                "codexConfigDir",
                &["OSIR_HISTORY_CODEX_HOME", "CODEX_HOME"][..],
                ".codex",
            ),
            (
                "claude",
                "claudeConfigDir",
                &["OSIR_HISTORY_CLAUDE_HOME", "CLAUDE_CONFIG_DIR"][..],
                ".claude",
            ),
            (
                "gemini",
                "geminiConfigDir",
                &["OSIR_HISTORY_GEMINI_HOME"][..],
                ".gemini",
            ),
            ("grokbuild", "grokConfigDir", &[][..], ".grok"),
            ("openclaw", "openclawConfigDir", &[][..], ".openclaw"),
            ("hermes", "hermesConfigDir", &["HERMES_HOME"][..], ".hermes"),
            (
                "pi",
                "piConfigDir",
                &["PI_CODING_AGENT_DIR"][..],
                ".pi/agent",
            ),
            ("kimi", "", &["KIMI_CODE_HOME"][..], ".kimi-code"),
            (
                "mcode",
                "",
                &["MINIMAX_DATA_DIR", "MAVIS_DATA_DIR"][..],
                ".minimax",
            ),
        ] {
            roots.insert(
                provider.into(),
                preference(setting)
                    .or_else(|| env_path(names))
                    .unwrap_or_else(|| home.join(fallback)),
            );
        }
        let data = env_path(&["XDG_DATA_HOME"]).unwrap_or_else(|| home.join(".local/share"));
        let opencode =
            env_path(&["OSIR_HISTORY_OPENCODE_DATA"]).unwrap_or_else(|| data.join("opencode"));
        let db = env
            .get("OSIR_HISTORY_OPENCODE_DB")
            .or_else(|| env.get("OPENCODE_DB"))
            .filter(|v| !v.trim().is_empty())
            .map(|v| {
                let path = PathBuf::from(v);
                if path.is_absolute() {
                    external(v).unwrap_or_else(|| opencode.join("opencode.db"))
                } else {
                    opencode.join(path)
                }
            })
            .filter(|p| !p.starts_with(sandbox))
            .unwrap_or_else(|| opencode.join("opencode.db"));
        roots.insert("opencode".into(), opencode);
        roots.insert("opencode-db".into(), db);
        let profile = env
            .get("OMP_PROFILE")
            .or_else(|| env.get("PI_PROFILE"))
            .map(String::as_str)
            .filter(|p| !p.is_empty() && *p != "default")
            .filter(|p| {
                p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
                    && !matches!(*p, "." | "..")
            });
        let omp_base = env
            .get("PI_CONFIG_DIR")
            .filter(|p| !p.is_empty())
            .map(|p| home.join(p.trim_start_matches(std::path::MAIN_SEPARATOR)))
            .unwrap_or_else(|| home.join(".omp"));
        let default_omp = profile
            .map(|p| omp_base.join("profiles").join(p).join("agent"))
            .unwrap_or_else(|| omp_base.join("agent"));
        let agent = preference("ompConfigDir")
            .or_else(|| {
                if profile.is_some() {
                    None
                } else {
                    env_path(&["PI_CODING_AGENT_DIR"])
                }
            })
            .unwrap_or_else(|| default_omp.clone());
        let xdg_omp = profile
            .map(|p| data.join("omp/profiles").join(p))
            .unwrap_or_else(|| data.join("omp"));
        roots.insert(
            "omp".into(),
            if agent == default_omp && env_path(&["XDG_DATA_HOME"]).is_some() && xdg_omp.is_dir() {
                xdg_omp
            } else {
                agent
            },
        );
        Self {
            home: home.into(),
            roots,
            session_dir: env
                .get("PI_CODING_AGENT_SESSION_DIR")
                .filter(|p| !p.is_empty())
                .filter(|p| !Path::new(p).is_absolute() || external(p).is_some())
                .cloned(),
        }
    }
}

pub(crate) fn capture_system_roots(home: &Path, sandbox: &Path) {
    // An inherited sandbox must never become the system history source.
    let env = [
        "OSIR_HISTORY_CODEX_HOME",
        "OSIR_HISTORY_CLAUDE_HOME",
        "OSIR_HISTORY_GEMINI_HOME",
        "OSIR_HISTORY_OPENCODE_DATA",
        "OSIR_HISTORY_OPENCODE_DB",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "XDG_DATA_HOME",
        "OPENCODE_DB",
        "HERMES_HOME",
        "PI_CODING_AGENT_DIR",
        "PI_CODING_AGENT_SESSION_DIR",
        "PI_CONFIG_DIR",
        "OMP_PROFILE",
        "PI_PROFILE",
        "KIMI_CODE_HOME",
        "MINIMAX_DATA_DIR",
        "MAVIS_DATA_DIR",
    ]
    .into_iter()
    .filter_map(|name| std::env::var(name).ok().map(|v| (name.to_owned(), v)))
    .collect();
    let _ = SYSTEM_ROOTS.set(HistoryRoots::resolve(
        home,
        sandbox,
        &env,
        &directory_preferences(home),
    ));
}

pub(crate) fn isolated_root(provider: &str) -> Option<PathBuf> {
    SYSTEM_ROOTS.get()?.roots.get(provider).cloned()
}

pub(crate) fn history_user_home() -> PathBuf {
    SYSTEM_ROOTS
        .get()
        .map(|r| r.home.clone())
        .unwrap_or_else(crate::config::get_home_dir)
}

pub(crate) fn session_directory_override() -> Option<String> {
    if let Some(roots) = SYSTEM_ROOTS.get() {
        return roots.session_dir.clone();
    }
    std::env::var("PI_CODING_AGENT_SESSION_DIR")
        .ok()
        .filter(|v| !v.is_empty())
}

pub(crate) fn tool_home(provider: &str, fallback: impl FnOnce() -> PathBuf) -> PathBuf {
    isolated_root(provider).unwrap_or_else(fallback)
}

fn development_source(name: &str) -> Option<PathBuf> {
    if !cfg!(debug_assertions) || std::env::var_os("CC_SWITCH_TEST_HOME").is_none() {
        return None;
    }
    let path = PathBuf::from(std::env::var_os(name)?);
    path.is_absolute().then_some(path)
}

pub fn codex_home() -> PathBuf {
    isolated_root("codex")
        .or_else(|| development_source("OSIR_HISTORY_CODEX_HOME"))
        .or_else(crate::settings::get_codex_override_dir)
        .or_else(|| {
            std::env::var("CODEX_HOME")
                .ok()
                .and_then(|v| expand_path(&v, &history_user_home()))
        })
        .unwrap_or_else(crate::codex_config::get_codex_config_dir)
}

pub fn claude_home() -> PathBuf {
    isolated_root("claude")
        .or_else(|| development_source("OSIR_HISTORY_CLAUDE_HOME"))
        .or_else(crate::settings::get_claude_override_dir)
        .or_else(|| {
            std::env::var("CLAUDE_CONFIG_DIR")
                .ok()
                .and_then(|v| expand_path(&v, &history_user_home()))
        })
        .unwrap_or_else(crate::config::get_claude_config_dir)
}

/// Explicit DEV preference source captured before HOME isolation. No fallback
/// to the current configuration or inference from a history directory name.
/// Release builds and ordinary launches never consult this source.
pub(crate) fn claude_preferences_root() -> Option<PathBuf> {
    development_source("OSIR_DEV_CLAUDE_PREFERENCES_DIR")
}

pub fn gemini_home() -> PathBuf {
    isolated_root("gemini")
        .or_else(|| development_source("OSIR_HISTORY_GEMINI_HOME"))
        .unwrap_or_else(crate::gemini_config::get_gemini_dir)
}

pub fn opencode_base_override() -> Option<PathBuf> {
    isolated_root("opencode").or_else(|| development_source("OSIR_HISTORY_OPENCODE_DATA"))
}

pub fn opencode_database() -> PathBuf {
    isolated_root("opencode-db")
        .or_else(|| development_source("OSIR_HISTORY_OPENCODE_DB"))
        .unwrap_or_else(crate::opencode_config::get_opencode_db_path)
}

/// Sources explicitly passed by the development launcher are always read-only.
/// This guard is also applied natively before either file or SQLite deletion.
pub fn is_read_only(provider_id: &str) -> bool {
    if provider_id == "claude-desktop" || SYSTEM_ROOTS.get().is_some() {
        return true;
    }
    let names: &[&str] = match provider_id {
        "codex" => &["OSIR_HISTORY_CODEX_HOME"],
        "claude" => &["OSIR_HISTORY_CLAUDE_HOME"],
        "gemini" => &["OSIR_HISTORY_GEMINI_HOME"],
        "opencode" => &["OSIR_HISTORY_OPENCODE_DATA", "OSIR_HISTORY_OPENCODE_DB"],
        _ => &[],
    };
    names.iter().any(|name| development_source(name).is_some())
}

pub fn ensure_writable(provider_id: &str) -> Result<(), String> {
    if is_read_only(provider_id) {
        return Err("此来源为本机只读历史，请在原应用中管理或删除。".into());
    }
    Ok(())
}

/// Inspect only a provider's expected history folders, never its auth files.
pub fn ensure_readable_roots(roots: &[PathBuf]) -> Result<(), String> {
    for root in roots {
        match std::fs::read_dir(root) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("无法读取本机会话目录，请检查文件访问权限后重试。".into()),
        }
    }
    Ok(())
}

pub fn codex_roots(home: &Path) -> Vec<PathBuf> {
    vec![home.join("sessions"), home.join("archived_sessions")]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_history_defaults_to_system_home_for_every_tool() {
        let home = Path::new("/fixture/system-user");
        let roots = HistoryRoots::resolve(
            home,
            Path::new("/fixture/beta/sandbox"),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        for (tool, suffix) in [
            ("codex", ".codex"),
            ("claude", ".claude"),
            ("gemini", ".gemini"),
            ("hermes", ".hermes"),
            ("grokbuild", ".grok"),
            ("openclaw", ".openclaw"),
            ("pi", ".pi/agent"),
            ("omp", ".omp/agent"),
            ("kimi", ".kimi-code"),
            ("mcode", ".minimax"),
            ("opencode", ".local/share/opencode"),
        ] {
            assert_eq!(roots.roots.get(tool), Some(&home.join(suffix)));
        }
    }

    #[test]
    fn history_honors_saved_custom_directories_and_native_environment() {
        let env = BTreeMap::from([
            ("CODEX_HOME".into(), "/native/codex".into()),
            ("CLAUDE_CONFIG_DIR".into(), "/native/claude".into()),
            ("XDG_DATA_HOME".into(), "/native/data".into()),
            ("OPENCODE_DB".into(), "previous.db".into()),
            ("HERMES_HOME".into(), "/native/hermes".into()),
        ]);
        let preferences = BTreeMap::from([
            ("codexConfigDir".into(), "~/custom-codex".into()),
            ("piConfigDir".into(), "/custom/pi".into()),
        ]);
        let roots = HistoryRoots::resolve(
            Path::new("/fixture/user"),
            Path::new("/fixture/sandbox"),
            &env,
            &preferences,
        );
        assert_eq!(
            roots.roots["codex"],
            Path::new("/fixture/user/custom-codex")
        );
        assert_eq!(roots.roots["claude"], Path::new("/native/claude"));
        assert_eq!(roots.roots["pi"], Path::new("/custom/pi"));
        assert_eq!(roots.roots["hermes"], Path::new("/native/hermes"));
        assert_eq!(
            roots.roots["opencode-db"],
            Path::new("/native/data/opencode/previous.db")
        );
    }

    #[test]
    fn relative_native_directories_keep_their_provider_specific_base() {
        let env = BTreeMap::from([
            ("HERMES_HOME".into(), "relative-hermes".into()),
            ("MINIMAX_DATA_DIR".into(), "relative-mcode".into()),
            ("XDG_DATA_HOME".into(), "relative-data".into()),
            ("OPENCODE_DB".into(), "custom.db".into()),
        ]);
        let roots = HistoryRoots::resolve(
            Path::new("/fixture/user"),
            Path::new("/fixture/sandbox"),
            &env,
            &BTreeMap::new(),
        );
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(roots.roots["hermes"], cwd.join("relative-hermes"));
        assert_eq!(roots.roots["mcode"], cwd.join("relative-mcode"));
        assert_eq!(roots.roots["opencode"], cwd.join("relative-data/opencode"));
        assert_eq!(
            roots.roots["opencode-db"],
            cwd.join("relative-data/opencode/custom.db")
        );
    }

    #[test]
    fn inherited_isolated_environment_cannot_hide_system_history() {
        let home = Path::new("/fixture/user");
        let sandbox = home.join(".osir-client-beta/sandbox");
        let env = BTreeMap::from([
            (
                "CODEX_HOME".into(),
                sandbox.join(".codex").to_string_lossy().into_owned(),
            ),
            (
                "CLAUDE_CONFIG_DIR".into(),
                sandbox.join(".claude").to_string_lossy().into_owned(),
            ),
            (
                "XDG_DATA_HOME".into(),
                sandbox.join(".local/share").to_string_lossy().into_owned(),
            ),
            (
                "OPENCODE_DB".into(),
                sandbox
                    .join(".local/share/opencode/opencode.db")
                    .to_string_lossy()
                    .into_owned(),
            ),
        ]);
        let roots = HistoryRoots::resolve(home, &sandbox, &env, &BTreeMap::new());
        assert_eq!(roots.roots["codex"], home.join(".codex"));
        assert_eq!(roots.roots["claude"], home.join(".claude"));
        assert_eq!(
            roots.roots["opencode-db"],
            home.join(".local/share/opencode/opencode.db")
        );
        assert!(roots.roots.values().all(|p| !p.starts_with(&sandbox)));
    }

    #[test]
    fn custom_preferences_are_read_only_and_do_not_initialize_the_settings_cache() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(".osir_client");
        std::fs::create_dir_all(&dir).unwrap();
        let content = br#"{"codexConfigDir":"/fixture/codex","unrelated":"fixture-value"}"#;
        std::fs::write(dir.join("settings.json"), content).unwrap();
        let prefs = directory_preferences(temp.path());
        assert_eq!(prefs.len(), 1);
        assert_eq!(prefs["codexConfigDir"], "/fixture/codex");
        assert_eq!(std::fs::read(dir.join("settings.json")).unwrap(), content);
    }
}
