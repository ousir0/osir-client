//! OSIR's current-configuration launch adapter. Uses CC Switch's resolver and
//! terminal transports without rerunning login/interactive shell startup files.
//! No provider selection, native-auth mutation or live configuration write here.
use super::*;

const CONFIG_MISSING: &str = "当前 OSIRAPI 连接尚未完整应用，请先连接后再启动";
const CONFIG_CHANGED: &str = "当前工具连接配置已变化，请重新连接后再启动";
const CONFIG_UNREADABLE: &str = "无法读取当前工具配置，请检查配置后重试";

/// Per-request metadata only. Never captures tool output, environment or argv.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CliLaunchState {
    Pending,
    Started { pid: i32 },
    Exited { pid: i32, code: i32 },
}

pub(crate) struct CliLaunchReceipt {
    path: PathBuf,
}

impl CliLaunchReceipt {
    fn new() -> Result<Self, String> {
        let path = unique_temp_path("osir_cli_receipt", "state");
        crate::config::atomic_write_private(&path, b"pending\n")
            .map_err(|_| "无法准备工具启动环境")?;
        Ok(Self { path })
    }

    pub(crate) fn state(&self) -> CliLaunchState {
        use std::io::Read;
        let Ok(metadata) = std::fs::symlink_metadata(&self.path) else {
            return CliLaunchState::Pending;
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 128 {
            return CliLaunchState::Pending;
        }
        let mut content = String::new();
        if std::fs::File::open(&self.path)
            .and_then(|file| file.take(129).read_to_string(&mut content))
            .is_err()
        {
            return CliLaunchState::Pending;
        }
        receipt_state(&content)
    }

    fn expire(&self, launch_files: Vec<PathBuf>) {
        self.expire_after(launch_files, std::time::Duration::from_secs(60));
    }

    fn expire_after(&self, mut launch_files: Vec<PathBuf>, lifetime: std::time::Duration) {
        launch_files.push(self.path.clone());
        tokio::spawn(async move {
            tokio::time::sleep(lifetime).await;
            for path in launch_files {
                let _ = std::fs::remove_file(path);
            }
        });
    }
}

impl Drop for CliLaunchReceipt {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn receipt_state(content: &str) -> CliLaunchState {
    // A concurrent append may leave an incomplete last line. Only consume a
    // complete record, and accept only this tiny fixed schema.
    let Some((complete, _)) = content.rsplit_once('\n') else {
        return CliLaunchState::Pending;
    };
    let Some(last) = complete.lines().last() else {
        return CliLaunchState::Pending;
    };
    let parts = last.split_ascii_whitespace().collect::<Vec<_>>();
    let pid = parts
        .get(1)
        .and_then(|pid| pid.parse::<i32>().ok())
        .filter(|pid| *pid > 1);
    match (parts.as_slice(), pid) {
        (["started", _], Some(pid)) => CliLaunchState::Started { pid },
        (["exited", _, code], Some(pid)) => code
            .parse::<i32>()
            .ok()
            .filter(|code| (0..=255).contains(code))
            .map_or(CliLaunchState::Pending, |code| CliLaunchState::Exited {
                pid,
                code,
            }),
        _ => CliLaunchState::Pending,
    }
}

pub(super) fn executable(id: &str) -> Result<PathBuf, String> {
    if !VALID_TOOLS.contains(&id) {
        return Err("不支持启动此命令行工具".into());
    }
    let path = locate_default_tool(
        id,
        CommandDeadline::from_timeout(Some(std::time::Duration::from_secs(4))),
    )
    .map_err(|_| "无法确定工具的默认安装，请重新检测安装状态".to_string())?;
    launchable_entry(path)
}

fn launchable_entry(path: PathBuf) -> Result<PathBuf, String> {
    // Preserve the resolved entry point (including npm/Volta shims). Executing a
    // symlink's canonical target can lose argv[0]/relative-sibling semantics.
    if !path.is_absolute()
        || !path.is_file()
        || !is_executable_file(&path)
        || path
            .to_str()
            .is_none_or(|p| p.chars().any(char::is_control))
    {
        return Err("工具安装路径不可用于启动".into());
    }
    Ok(path)
}

fn tool_app(id: &str) -> Result<AppType, String> {
    if id == "grok" {
        Ok(AppType::GrokBuild)
    } else if VALID_TOOLS.contains(&id) {
        AppType::from_str(id).map_err(|_| "不支持启动此命令行工具".into())
    } else {
        Err("不支持启动此命令行工具".into())
    }
}

fn path_value(path: &Path) -> Result<String, String> {
    path.to_str()
        .filter(|p| path.is_absolute() && !p.chars().any(char::is_control))
        .map(str::to_owned)
        .ok_or_else(|| CONFIG_UNREADABLE.into())
}

fn set_env(launch: &mut ProviderLaunch, key: &'static str, value: Option<String>) {
    launch.env.retain(|(name, _)| *name != key);
    launch.env.push((key, value));
}

fn read_claude_settings(path: &Path) -> Result<Option<serde_json::Value>, String> {
    match std::fs::read(path) {
        Ok(bytes) if bytes.len() <= 4 * 1024 * 1024 => serde_json::from_slice(&bytes)
            .map_err(|_| CONFIG_UNREADABLE.into())
            .and_then(|v: serde_json::Value| {
                if v.is_object() {
                    Ok(Some(v))
                } else {
                    Err(CONFIG_UNREADABLE.into())
                }
            }),
        Ok(_) => Err(CONFIG_UNREADABLE.into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(CONFIG_UNREADABLE.into()),
    }
}

fn claude_launch(directory: &Path, settings: &Path) -> Result<ProviderLaunch, String> {
    let mut launch = projected_terminal_launch(&AppType::Claude, directory, None)?;
    preserve_claude_default_mcp_path(&mut launch, directory, &crate::config::get_home_dir());
    let settings_value = read_claude_settings(settings)?;
    if settings_value.is_some() {
        // Explicit settings and native env win over project-local or terminal rc
        // credentials. Values only travel through the existing private env file.
        launch.args = vec!["--settings".into(), path_value(settings)?];
    }
    for key in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
    ] {
        let value = settings_value
            .as_ref()
            .and_then(|v| v.get("env"))
            .and_then(|v| v.get(key))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        set_env(&mut launch, key, value);
    }
    Ok(launch)
}

fn preserve_claude_default_mcp_path(launch: &mut ProviderLaunch, directory: &Path, home: &Path) {
    if directory == home.join(".claude") {
        // Claude treats an explicit CLAUDE_CONFIG_DIR as a custom layout, even
        // when it equals HOME/.claude: MCP then moves to .claude/.claude.json.
        // Our native default writer uses HOME/.claude.json. Leave the default
        // variable unset while preserving explicit --settings and isolated HOME.
        set_env(launch, "CLAUDE_CONFIG_DIR", None);
    }
}

fn env_value<'a>(launch: &'a ProviderLaunch, key: &str) -> Option<&'a str> {
    launch
        .env
        .iter()
        .find(|(name, _)| *name == key)
        .and_then(|(_, v)| v.as_deref())
        .filter(|v| !v.trim().is_empty())
}

fn is_osir_provider(row: &crate::provider::Provider) -> bool {
    row.meta.as_ref().is_some_and(|m| m.osir_binding.is_some())
        || row
            .settings_config
            .to_string()
            .to_ascii_lowercase()
            .contains("osirclaw.com")
}

fn same_endpoint(actual: Option<&str>, expected: Option<&str>) -> Result<(), String> {
    let actual = actual
        .filter(|v| !v.trim().is_empty())
        .ok_or(CONFIG_MISSING)?;
    let expected = expected
        .filter(|v| !v.trim().is_empty())
        .ok_or(CONFIG_MISSING)?;
    if actual.trim_end_matches('/') == expected.trim_end_matches('/') {
        Ok(())
    } else {
        Err(CONFIG_CHANGED.into())
    }
}

fn codex_endpoint(config: &str) -> Option<String> {
    let doc: toml::Value = config.parse().ok()?;
    let provider = doc.get("model_provider")?.as_str()?;
    doc.get("model_providers")?
        .get(provider)?
        .get("base_url")?
        .as_str()
        .map(str::to_owned)
}

fn validate_osir_projection(
    app: &AppType,
    row: Option<&crate::provider::Provider>,
    launch: &ProviderLaunch,
    proxy_endpoint: Option<&str>,
) -> Result<(), String> {
    let Some(row) = row.filter(|r| is_osir_provider(r)) else {
        return Ok(()); // Native mode keeps the tool's official login available.
    };
    match app {
        AppType::Claude | AppType::Gemini => {
            let (base, credentials): (&str, &[&str]) = if *app == AppType::Claude {
                (
                    "ANTHROPIC_BASE_URL",
                    &["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"],
                )
            } else {
                (
                    "GOOGLE_GEMINI_BASE_URL",
                    &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
                )
            };
            if !credentials
                .iter()
                .any(|key| env_value(launch, key).is_some())
            {
                return Err(CONFIG_MISSING.into());
            }
            same_endpoint(
                env_value(launch, base),
                proxy_endpoint.or_else(|| row.settings_config.get("env")?.get(base)?.as_str()),
            )
        }
        AppType::Codex => {
            let native = std::fs::read_to_string(crate::codex_config::get_codex_config_path())
                .map_err(|_| CONFIG_MISSING)?;
            let actual = codex_endpoint(&native);
            let expected = row
                .settings_config
                .get("config")
                .and_then(serde_json::Value::as_str)
                .and_then(codex_endpoint);
            same_endpoint(actual.as_deref(), proxy_endpoint.or(expected.as_deref()))
        }
        _ => Ok(()),
    }
}

fn plain_launch(id: &str) -> ProviderLaunch {
    ProviderLaunch {
        title: "正在打开所选项目的工具终端：",
        detail: None,
        program: id.into(),
        args: Vec::new(),
        batch_command: id.into(),
        env: Vec::new(),
        batch_vars: Vec::new(),
        cleanup: Vec::new(),
    }
}

/// Agy is launch-scoped in CC Switch: reuse an existing private profile, but
/// never invoke prepare_launch (which can create or rewrite that profile).
fn agy_launch(
    row: Option<&crate::provider::Provider>,
    profile: Option<&Path>,
) -> Result<ProviderLaunch, String> {
    let mut launch = plain_launch("agy");
    for key in [
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "GOOGLE_GEMINI_BASE_URL",
        "GEMINI_MODEL",
    ] {
        set_env(&mut launch, key, None);
    }
    let Some(row) = row else {
        return Ok(launch);
    };
    let string = |pointer: &str| -> Result<Option<String>, String> {
        match row.settings_config.pointer(pointer) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(value)) if !value.chars().any(char::is_control) => {
                Ok((!value.trim().is_empty()).then(|| value.clone()))
            }
            _ => Err(CONFIG_UNREADABLE.into()),
        }
    };
    let model = string("/config/model/name")?.or(string("/env/GEMINI_MODEL")?);
    let official = crate::services::provider::agy_direct::is_official(row);
    let key = if official {
        None
    } else {
        string("/env/GEMINI_API_KEY")?
    };
    let endpoint = if official {
        None
    } else {
        string("/env/GOOGLE_GEMINI_BASE_URL")?
    };
    if let Some(key) = key {
        let endpoint = endpoint.ok_or(CONFIG_MISSING)?;
        let directory = profile.filter(|p| p.is_dir()).ok_or(CONFIG_MISSING)?;
        let settings = read_claude_settings(&directory.join("antigravity-cli/settings.json"))?
            .ok_or(CONFIG_MISSING)?;
        if settings
            .get("modelProvider")
            .and_then(serde_json::Value::as_str)
            != Some("gemini")
            || settings.get("model").and_then(serde_json::Value::as_str) != model.as_deref()
        {
            return Err(CONFIG_CHANGED.into());
        }
        launch
            .args
            .push(format!("--gemini_dir={}", path_value(directory)?));
        set_env(&mut launch, "GEMINI_API_KEY", Some(key));
        set_env(&mut launch, "GOOGLE_GEMINI_BASE_URL", Some(endpoint));
    } else {
        if endpoint.is_some() {
            return Err(CONFIG_MISSING.into());
        }
        if let Some(model) = model {
            launch.args.push(format!("--model={model}"));
        }
    }
    Ok(launch)
}

fn extra_native_launch(id: &str) -> Result<ProviderLaunch, String> {
    let mut launch = plain_launch(id);
    let entry = match id {
        "hermes" => Some(("HERMES_HOME", crate::hermes_config::get_hermes_dir())),
        "pi" => Some((
            "PI_CODING_AGENT_DIR",
            crate::pi_config::get_pi_agent_dir().map_err(|_| CONFIG_UNREADABLE)?,
        )),
        "omp" => {
            let agent = crate::omp_config::get_omp_agent_dir().map_err(|_| CONFIG_UNREADABLE)?;
            let native =
                crate::omp_config::get_omp_native_agent_dir().map_err(|_| CONFIG_UNREADABLE)?;
            let profile = if agent == native {
                crate::omp_config::get_omp_active_profile().map_err(|_| CONFIG_UNREADABLE)?
            } else {
                None
            };
            let profile = profile.unwrap_or_else(|| "default".into());
            set_env(&mut launch, "OMP_PROFILE", Some(profile.clone()));
            set_env(&mut launch, "PI_PROFILE", Some(profile));
            for key in [
                "PI_CONFIG_DIR",
                "PI_CODING_AGENT_SESSION_DIR",
                "XDG_DATA_HOME",
                "XDG_STATE_HOME",
                "XDG_CACHE_HOME",
            ] {
                set_env(&mut launch, key, std::env::var(key).ok());
            }
            Some(("PI_CODING_AGENT_DIR", agent))
        }
        "mcode" => Some(("MINIMAX_DATA_DIR", crate::mcode_config::data_dir())),
        "kimi" => Some(("KIMI_CODE_HOME", crate::kimi_config::data_dir())),
        // Grok and OpenClaw defaults already share native HOME. Their custom
        // paths are not silently guessed as undocumented CLI flags.
        "grok" | "openclaw" => None,
        _ => return Err("不支持启动此命令行工具".into()),
    };
    if let Some((key, path)) = entry {
        set_env(&mut launch, key, Some(path_value(&path)?));
    }
    Ok(launch)
}

pub(super) async fn launch(
    state: &crate::store::AppState,
    id: &str,
    directory: &Path,
) -> Result<CliLaunchReceipt, String> {
    launch_with_resume(state, id, directory, None).await
}

pub(super) async fn launch_with_resume(
    state: &crate::store::AppState,
    id: &str,
    directory: &Path,
    session_id: Option<&str>,
) -> Result<CliLaunchReceipt, String> {
    let app = tool_app(id)?;
    if !directory.is_absolute() || !directory.is_dir() || path_value(directory).is_err() {
        return Err("项目文件夹已不可用，请重新选择".into());
    }
    let executable = executable(id)?;
    let _switch = state.proxy_service.lock_switch_for_app(app.as_str()).await;
    super::osir_cli_preferences::preserve_on_launch(id);
    let core = matches!(app, AppType::Codex | AppType::Claude | AppType::Gemini);
    let mode = if core {
        Some(
            crate::mode::state::mode_state(
                &crate::live::engine::DeviceStore::for_device(),
                app.as_str(),
            )
            .map_err(|_| "无法确认当前工具连接状态")?,
        )
    } else {
        None
    };
    if mode
        .as_ref()
        .is_some_and(|mode| mode.is_proxy() && !mode.attached)
    {
        return Err("代理尚未连接，请先重新连接".into());
    }
    let row = if mode.as_ref().is_some_and(|mode| mode.is_proxy()) {
        let route = mode
            .as_ref()
            .and_then(|mode| mode.proxy_route.as_deref())
            .ok_or("无法确认当前工具连接状态")?;
        Some(
            state
                .db
                .get_provider_by_id(route, app.as_str())
                .map_err(|_| "无法确认当前工具连接状态")?
                .ok_or("无法确认当前工具连接状态")?,
        )
    } else if core || app == AppType::Agy {
        crate::mode::current::direct_provider(&state.db, &app)
            .map_err(|_| "无法确认当前工具连接状态")?
    } else {
        None
    };
    let mut launch = match app {
        AppType::Codex => codex_terminal_launch(&crate::codex_config::get_codex_config_dir())?,
        AppType::Claude => claude_launch(
            &crate::config::get_claude_config_dir(),
            &crate::config::get_claude_settings_path(),
        )?,
        AppType::Gemini => {
            projected_terminal_launch(&app, &crate::gemini_config::get_gemini_dir(), None)?
        }
        AppType::OpenCode => {
            let mode =
                crate::opencode_config::osir_connection_mode().map_err(|_| CONFIG_UNREADABLE)?;
            if mode.mode == "unavailable" {
                return Err(CONFIG_CHANGED.into());
            }
            let path = crate::opencode_config::get_opencode_config_path()
                .map_err(|_| CONFIG_UNREADABLE)?;
            projected_terminal_launch(
                &app,
                &crate::opencode_config::get_opencode_dir(),
                Some(&path),
            )?
        }
        AppType::Agy => {
            let profile = row
                .as_ref()
                .map(|row| crate::agy_config::key_profile_dir(&row.id));
            agy_launch(row.as_ref(), profile.as_deref())?
        }
        _ => extra_native_launch(id)?,
    };
    let endpoints = if mode.as_ref().is_some_and(|mode| mode.is_proxy()) {
        Some(
            state
                .proxy_service
                .build_proxy_urls()
                .await
                .map_err(|_| "无法确认当前工具连接状态")?,
        )
    } else {
        None
    };
    validate_osir_projection(
        &app,
        row.as_ref(),
        &launch,
        endpoints.as_ref().map(|urls| {
            if app == AppType::Codex {
                urls.1.as_str()
            } else {
                urls.0.as_str()
            }
        }),
    )?;
    launch.program = path_value(&executable)?;
    let home = crate::config::get_home_dir();
    // Configuration HOME and project cwd are deliberately independent.
    current_configuration_home(&mut launch, &home, crate::distribution::is_isolated())?;
    #[cfg(not(target_os = "windows"))]
    {
        let inherited = std::env::var("PATH").unwrap_or_default();
        let login = login_shell_path(CommandDeadline::from_timeout(Some(
            std::time::Duration::from_secs(4),
        )))
        .unwrap_or_default();
        let path = merge_path_segments(
            &format!(
                "{}:{}",
                executable.parent().ok_or(CONFIG_UNREADABLE)?.display(),
                login
            ),
            &inherited,
        );
        let path = path
            .split(':')
            .filter(|p| Path::new(p).is_absolute() && !p.chars().any(char::is_control))
            .collect::<Vec<_>>()
            .join(":");
        set_env(&mut launch, "PATH", Some(path));
    }
    if let Some(session_id) = session_id {
        let args = crate::session_manager::resume::arguments(id, session_id)?;
        launch.batch_command.push(' ');
        launch.batch_command.push_str(&args.join(" "));
        launch.args.extend(args);
    }
    launch_terminal(&launch, directory)
}

fn current_configuration_home(
    launch: &mut ProviderLaunch,
    home: &Path,
    isolated: bool,
) -> Result<(), String> {
    if isolated {
        isolate_current_cli_launch(launch, home)?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn script(
    launch: &ProviderLaunch,
    directory: &Path,
    script_path: &Path,
    env_path: &Path,
    receipt_path: Option<&Path>,
) -> String {
    let cleanup = launch
        .cleanup
        .iter()
        .map(PathBuf::as_path)
        .chain([script_path, env_path])
        .map(|p| shell_single_quote(&p.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");
    let remove = format!("/bin/rm -f -- {cleanup}");
    let env_path = shell_single_quote(&env_path.to_string_lossy());
    let command = unix_launch_command(launch);
    let command = command
        .strip_prefix("env ")
        .map_or(command.clone(), |rest| format!("/usr/bin/env {rest}"));
    let (setup, finish, started, command) = if let Some(receipt) = receipt_path {
        (
            format!(
                "exec 3>{}\n",
                shell_single_quote(&receipt.to_string_lossy())
            ),
            format!("status=$?; printf 'exited %s %s\\n' \"$$\" \"$status\" >&3; {remove}"),
            "printf 'started %s\\n' \"$$\" >&3\n",
            format!("{command} 3>&-"),
        )
    } else {
        (String::new(), remove, "", command)
    };
    format!("#!/bin/sh\numask 077\n{setup}trap {} EXIT\nset -a\n. {env_path}\nset +a\n/bin/rm -f -- {env_path}\ncd {} || exit 1\nprintf '%s\\n' {}\n{started}{command}\nstatus=$?\nprintf '\\n%s %s\\n' '工具已退出，退出码：' \"$status\"\nexit \"$status\"\n", shell_single_quote(&finish), shell_single_quote(&directory.to_string_lossy()), shell_single_quote(launch.title))
}

fn launch_terminal(launch: &ProviderLaunch, directory: &Path) -> Result<CliLaunchReceipt, String> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let script_path = unique_temp_path("osir_current_launcher", "sh");
        let env_path = unique_temp_path("osir_current_env", "sh");
        let receipt = CliLaunchReceipt::new()?;
        let result = (|| {
            crate::config::atomic_write_private(&env_path, unix_launch_env_file(launch).as_bytes())
                .map_err(|_| "无法准备工具启动环境")?;
            crate::config::atomic_write_private(
                &script_path,
                script(
                    launch,
                    directory,
                    &script_path,
                    &env_path,
                    Some(&receipt.path),
                )
                .as_bytes(),
            )
            .map_err(|_| "无法准备工具启动环境")?;
            #[cfg(target_os = "macos")]
            {
                launch_macos_terminal(&script_path)
            }
            #[cfg(target_os = "linux")]
            {
                launch_linux_terminal(&script_path)
            }
        })();
        if result.is_err() {
            for path in [&script_path, &env_path] {
                let _ = std::fs::remove_file(path);
            }
        }
        result.map_err(|_| "无法打开工具终端，请检查终端设置后重试".to_string())?;
        receipt.expire(vec![script_path, env_path]);
        Ok(receipt)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        launch_provider_terminal(launch, Some(directory))?;
        let receipt = CliLaunchReceipt::new()?;
        receipt.expire(Vec::new());
        Ok(receipt)
    }
}

#[cfg(test)]
#[path = "osir_tools_launcher_tests.rs"]
mod tests;
