//! Child-scoped lifecycle networking and bounded, redacted process results.
//! No global environment, npmrc, proxy preference or tool source changes.
#[cfg(not(target_os = "windows"))]
use super::isolate_child_process_group;
use super::terminate_child_tree;
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    process::{Command, Output, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

pub(super) const LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const CAPTURE_LIMIT: usize = 256 * 1024;
const PROXY_KEYS: &[&str] = &[
    "http_proxy",
    "HTTP_PROXY",
    "https_proxy",
    "HTTPS_PROXY",
    "all_proxy",
    "ALL_PROXY",
    "npm_config_proxy",
    "NPM_CONFIG_PROXY",
    "npm_config_https_proxy",
    "NPM_CONFIG_HTTPS_PROXY",
    "no_proxy",
    "NO_PROXY",
    "npm_config_noproxy",
    "NPM_CONFIG_NOPROXY",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Dns,
    Network,
    Permission,
    DiskFull,
    Node,
    NativeIncomplete,
    Timeout,
    Failed,
}
impl Failure {
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::Dns => "TOOL_INSTALL_DNS",
            Self::Network => "TOOL_INSTALL_NETWORK",
            Self::Permission => "TOOL_INSTALL_PERMISSION",
            Self::DiskFull => "TOOL_INSTALL_DISK_FULL",
            Self::Node => "TOOL_INSTALL_NODE",
            Self::NativeIncomplete => "TOOL_INSTALL_NATIVE_MISSING",
            Self::Timeout => "TOOL_INSTALL_TIMEOUT",
            Self::Failed => "TOOL_INSTALL_FAILED",
        }
    }
    pub(super) fn message(self) -> String {
        let text = match self {
            Self::Dns => "无法解析下载地址，请检查网络或代理后重试。",
            Self::Network => "下载连接失败，请检查网络或代理后重试。",
            Self::Permission => "没有安装目录的写入权限，请检查权限后重试。",
            Self::DiskFull => "磁盘空间不足，请释放空间后重试。",
            Self::Node => "Node.js 环境不可用或版本不兼容，请更新 Node.js 后重试。",
            Self::NativeIncomplete => "运行组件未安装完整，请重试修复安装。",
            Self::Timeout => "安装或更新超时，请检查网络后重试。",
            Self::Failed => "安装或更新未完成，请重新检测工具后重试。",
        };
        format!("{}：{text}", self.code())
    }
}

pub(super) fn classify(output: &Output) -> Result<(), Failure> {
    if output.status.success() {
        return Ok(());
    }
    // Never return/log any slice of output. Match only fixed diagnostic tokens.
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    )
    .to_ascii_lowercase();
    let contains = |terms: &[&str]| terms.iter().any(|term| text.contains(term));
    Err(
        if contains(&["enospc", "no space left on device", "disk full"]) {
            Failure::DiskFull
        } else if contains(&["eacces", "eperm", "permission denied", "access is denied"]) {
            Failure::Permission
        } else if contains(&[
            "enotfound",
            "eai_again",
            "could not resolve host",
            "could not resolve proxy",
            "getaddrinfo",
            "temporary failure in name resolution",
        ]) {
            Failure::Dns
        } else if contains(&[
            "tool_install_native_missing",
            "run the postinstall manually",
            "could not find native binary package",
            "missing optional dependency",
        ]) {
            Failure::NativeIncomplete
        } else if contains(&[
            "ebadengine",
            "unsupported engine",
            "node: command not found",
            "node: no such file",
            "npm: command not found",
            "npm: no such file",
            "'node' is not recognized",
            "'npm' is not recognized",
            "cannot find module",
            "requires node",
            "node.js version",
        ]) {
            Failure::Node
        } else if contains(&["etimedout", "timed out", "timeout"]) {
            Failure::Timeout
        } else if contains(&[
            "econnreset",
            "econnrefused",
            "enetunreach",
            "ehostunreach",
            "unable to get local issuer",
            "certificate verify failed",
            "unable_to_verify_leaf_signature",
            "fetch failed",
            "ssl connect error",
            "failed to connect",
            "proxy connect",
            "http error",
            "curl: (",
            "404 not found",
            "502 bad gateway",
            "503 service unavailable",
        ]) {
            Failure::Network
        } else {
            Failure::Failed
        },
    )
}

pub(super) fn io_failure(error: &std::io::Error) -> Failure {
    #[cfg(target_os = "windows")]
    let disk_full = error.raw_os_error() == Some(112);
    #[cfg(not(target_os = "windows"))]
    let disk_full = error.raw_os_error() == Some(libc::ENOSPC);
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => Failure::Permission,
        std::io::ErrorKind::TimedOut => Failure::Timeout,
        _ => {
            if disk_full {
                Failure::DiskFull
            } else {
                Failure::Failed
            }
        }
    }
}

fn tail(mut reader: impl Read, limit: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        let Ok(count) = reader.read(&mut buffer) else {
            break;
        };
        if count == 0 {
            break;
        }
        if kept.len() + count > limit {
            truncated = true;
            let excess = (kept.len() + count - limit).min(kept.len());
            kept.drain(..excess);
        }
        kept.extend_from_slice(&buffer[count.saturating_sub(limit)..count]);
    }
    (kept, truncated)
}

struct Capture {
    output: Output,
    truncated: bool,
}
fn capture(command: &mut Command, timeout: Duration, limit: usize) -> Result<Capture, Failure> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(not(target_os = "windows"))]
    isolate_child_process_group(command);
    let mut child = command.spawn().map_err(|error| io_failure(&error))?;
    let deadline = Instant::now() + timeout;
    let (sender, receiver) = mpsc::channel();
    for (is_stdout, pipe) in [
        (
            true,
            child
                .stdout
                .take()
                .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
        ),
        (
            false,
            child
                .stderr
                .take()
                .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
        ),
    ] {
        if let Some(pipe) = pipe {
            let sender = sender.clone();
            std::thread::spawn(move || {
                let (bytes, truncated) = tail(pipe, limit);
                let _ = sender.send((is_stdout, bytes, truncated));
            });
        }
    }
    drop(sender);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = terminate_child_tree(&mut child);
                let _ = child.wait();
                return Err(Failure::Timeout);
            }
            Err(error) => {
                let _ = terminate_child_tree(&mut child);
                let _ = child.wait();
                return Err(io_failure(&error));
            }
        }
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut truncated = false;
    // A background descendant can retain a pipe after its parent exits. The
    // same total deadline applies to capture, then its process group is killed.
    for _ in 0..2 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok((is_stdout, bytes, cut)) => {
                if is_stdout {
                    stdout = bytes;
                } else {
                    stderr = bytes;
                }
                truncated |= cut;
            }
            Err(_) => {
                let _ = terminate_child_tree(&mut child);
                return Err(Failure::Timeout);
            }
        }
    }
    Ok(Capture {
        output: Output {
            status,
            stdout,
            stderr,
        },
        truncated,
    })
}

pub(super) fn execute(command: &mut Command, timeout: Duration) -> Result<Output, Failure> {
    capture(command, timeout, CAPTURE_LIMIT).map(|capture| capture.output)
}

pub(super) fn run_lifecycle(command: &mut Command, label: &str) -> Result<(), String> {
    let started = Instant::now();
    log::info!("tool_lifecycle start target={label}");
    apply_proxy_environment(command);
    let (result, exit) = match execute(command, LIFECYCLE_TIMEOUT) {
        Ok(output) => (classify(&output), output.status.code()),
        Err(error) => (Err(error), None),
    };
    let category = result.as_ref().err().map_or("ok", |error| error.code());
    log::info!(
        "tool_lifecycle finish target={label} category={category} exit={exit:?} elapsed_ms={}",
        started.elapsed().as_millis()
    );
    result.map_err(Failure::message)
}

#[derive(Default, Debug, PartialEq, Eq)]
struct StaticProxies {
    http: Option<String>,
    https: Option<String>,
}

fn valid_proxy(value: &str) -> Option<String> {
    if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        return None;
    }
    let url = url::Url::parse(value).ok()?;
    (matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h") && url.host_str().is_some())
        .then(|| value.to_owned())
}

fn parse_static_proxies(text: &str) -> StaticProxies {
    let mut depth = 0usize;
    let mut fields = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if depth == 1 && !line.contains('{') && !line.contains('}') {
            if let Some((key, value)) = line.split_once(':') {
                if matches!(
                    key.trim(),
                    "HTTPEnable"
                        | "HTTPProxy"
                        | "HTTPPort"
                        | "HTTPSEnable"
                        | "HTTPSProxy"
                        | "HTTPSPort"
                ) {
                    fields.insert(key.trim(), value.trim());
                }
            }
        }
        depth = depth.saturating_add(line.bytes().filter(|byte| *byte == b'{').count());
        depth = depth.saturating_sub(line.bytes().filter(|byte| *byte == b'}').count());
    }
    let route = |prefix: &str| {
        if fields.get(format!("{prefix}Enable").as_str()).copied() != Some("1") {
            return None;
        }
        let host = fields.get(format!("{prefix}Proxy").as_str())?;
        if host.is_empty()
            || !host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
        {
            return None;
        }
        let port: u16 = fields.get(format!("{prefix}Port").as_str())?.parse().ok()?;
        if port == 0 {
            return None;
        }
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_string()
        };
        valid_proxy(&format!("http://{host}:{port}"))
    };
    StaticProxies {
        http: route("HTTP"),
        https: route("HTTPS"),
    }
}

fn system_static_proxies() -> StaticProxies {
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("/usr/sbin/scutil");
        command.arg("--proxy");
        if let Ok(capture) = capture(&mut command, Duration::from_secs(3), 64 * 1024) {
            if capture.output.status.success() && !capture.truncated {
                if let Ok(text) = std::str::from_utf8(&capture.output.stdout) {
                    return parse_static_proxies(text);
                }
            }
        }
    }
    StaticProxies::default()
}

fn proxy_additions(
    explicit: Option<&str>,
    inherited: &HashMap<String, String>,
    system: StaticProxies,
) -> Vec<(String, String)> {
    let existing = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| inherited.get(*key).and_then(|value| valid_proxy(value)))
    };
    let explicit = explicit.and_then(valid_proxy);
    let http = explicit
        .clone()
        .or_else(|| {
            existing(&[
                "npm_config_proxy",
                "NPM_CONFIG_PROXY",
                "http_proxy",
                "HTTP_PROXY",
                "all_proxy",
                "ALL_PROXY",
            ])
        })
        .or(system.http);
    let https = explicit
        .or_else(|| {
            existing(&[
                "npm_config_https_proxy",
                "NPM_CONFIG_HTTPS_PROXY",
                "https_proxy",
                "HTTPS_PROXY",
                "all_proxy",
                "ALL_PROXY",
                "npm_config_proxy",
                "NPM_CONFIG_PROXY",
            ])
        })
        .or(system.https);
    let mut additions = Vec::new();
    for (keys, value) in [
        (
            &[
                "http_proxy",
                "HTTP_PROXY",
                "npm_config_proxy",
                "NPM_CONFIG_PROXY",
            ][..],
            http,
        ),
        (
            &[
                "https_proxy",
                "HTTPS_PROXY",
                "npm_config_https_proxy",
                "NPM_CONFIG_HTTPS_PROXY",
            ][..],
            https,
        ),
    ] {
        if let Some(value) = value {
            for key in keys {
                // Existing exact variables, including an explicit empty value,
                // remain user-owned. Fill missing equivalents only.
                if !inherited.contains_key(*key) {
                    additions.push(((*key).to_owned(), value.clone()));
                }
            }
        }
    }
    if let Some(value) = inherited
        .get("no_proxy")
        .or_else(|| inherited.get("NO_PROXY"))
    {
        for key in [
            "no_proxy",
            "NO_PROXY",
            "npm_config_noproxy",
            "NPM_CONFIG_NOPROXY",
        ] {
            if !inherited.contains_key(key) {
                additions.push((key.to_owned(), value.clone()));
            }
        }
    }
    additions
}

fn apply_proxy_sources(
    command: &mut Command,
    explicit: Option<&str>,
    inherited: HashMap<String, String>,
    system: StaticProxies,
) {
    let mut effective = inherited;
    let mut removed = HashSet::new();
    for (key, value) in command.get_envs() {
        let Some(key) = key.to_str().filter(|key| PROXY_KEYS.contains(key)) else {
            continue;
        };
        match value {
            Some(value) => {
                // A non-Unicode command override also remains user-owned.
                effective.insert(key.to_owned(), value.to_str().unwrap_or("").to_owned());
            }
            None => {
                effective.remove(key);
                removed.insert(key.to_owned());
            }
        }
    }
    let additions = proxy_additions(explicit, &effective, system);
    // Set inherited proxy values explicitly as well: callers using env_clear()
    // still receive the same child-scoped networking, without global writes.
    for (key, value) in &effective {
        if !command
            .get_envs()
            .any(|(existing, _)| existing == std::ffi::OsStr::new(key))
        {
            command.env(key, value);
        }
    }
    for (key, value) in additions {
        if !removed.contains(&key) {
            command.env(key, value);
        }
    }
}

pub(super) fn apply_proxy_environment(command: &mut Command) {
    let inherited = PROXY_KEYS
        .iter()
        .filter_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| ((*key).to_owned(), value))
        })
        .collect();
    let explicit = crate::proxy::http_client::get_current_proxy_url();
    apply_proxy_sources(
        command,
        explicit.as_deref(),
        inherited,
        system_static_proxies(),
    );
}

#[cfg(test)]
#[path = "tool_lifecycle_support_tests.rs"]
mod tests;
