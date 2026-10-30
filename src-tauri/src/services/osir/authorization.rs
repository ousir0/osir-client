//! Native browser callback transport. No token or verifier is exposed to React.
//! Not enabled until OSIR implements the desktop session contract.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    time::{timeout, Instant},
};
use url::Url;

const CALLBACK_PATH: &str = "/osir/desktop/callback";
const MAX_HEAD: usize = 8192;
const FLOW_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum AuthorizationError {
    #[error("桌面授权地址配置无效")]
    InvalidConfiguration,
    #[error("无法启动本机授权回调服务")]
    CallbackUnavailable,
    #[error("授权等待已超时，请重新发起登录")]
    Expired,
    #[error("授权已取消")]
    Cancelled,
    #[error("网站未批准此次授权")]
    Denied,
}

// Deliberately not Debug/Serialize: these values belong in the native process.
pub struct AuthorizationGrant {
    pub code: String,
    pub verifier: String,
    pub redirect_uri: String,
}

pub struct PendingAuthorization {
    listener: TcpListener,
    state: String,
    verifier: String,
    redirect: Url,
    authorization_url: Url,
    deadline: Instant,
}

fn nonce() -> String {
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

impl PendingAuthorization {
    /// The endpoint must come from native trusted configuration, never a provider card.
    pub async fn bind(endpoint: &str, client_id: &str) -> Result<Self, AuthorizationError> {
        let mut authorization_url =
            Url::parse(endpoint).map_err(|_| AuthorizationError::InvalidConfiguration)?;
        if authorization_url.scheme() != "https"
            || authorization_url.host_str().is_none()
            || !authorization_url.username().is_empty()
            || authorization_url.password().is_some()
            || authorization_url.query().is_some()
            || authorization_url.fragment().is_some()
            || client_id.trim().is_empty()
        {
            return Err(AuthorizationError::InvalidConfiguration);
        }
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|_| AuthorizationError::CallbackUnavailable)?;
        let port = listener
            .local_addr()
            .map_err(|_| AuthorizationError::CallbackUnavailable)?
            .port();
        let redirect = Url::parse(&format!("http://127.0.0.1:{port}{CALLBACK_PATH}"))
            .map_err(|_| AuthorizationError::InvalidConfiguration)?;
        let state = nonce();
        let verifier = nonce();
        authorization_url
            .query_pairs_mut()
            .append_pair("client_id", client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", redirect.as_str())
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge(&verifier))
            .append_pair("code_challenge_method", "S256");
        Ok(Self {
            listener,
            state,
            verifier,
            redirect,
            authorization_url,
            deadline: Instant::now() + FLOW_TIMEOUT,
        })
    }

    /// Scope is chosen explicitly by the native session coordinator.
    pub fn set_scope(&mut self, scope: &str) -> Result<(), AuthorizationError> {
        super::auth::validate_scope(scope).map_err(|_| AuthorizationError::InvalidConfiguration)?;
        // Replacing scope must never create ambiguous duplicate query parameters.
        let pairs: Vec<(String, String)> = self
            .authorization_url
            .query_pairs()
            .filter(|(key, _)| key != "scope")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        self.authorization_url.set_query(None);
        self.authorization_url
            .query_pairs_mut()
            .extend_pairs(pairs)
            .append_pair("scope", scope);
        Ok(())
    }

    /// Only this URL may be passed to the system browser; it contains no verifier.
    pub fn browser_url(&self) -> &str {
        self.authorization_url.as_str()
    }

    /// Consumes the listener once. Cancellation, expiry and success all close the port.
    pub async fn wait(
        self,
        mut cancelled: oneshot::Receiver<()>,
    ) -> Result<AuthorizationGrant, AuthorizationError> {
        tokio::select! {
            biased;
            _ = &mut cancelled => Err(AuthorizationError::Cancelled),
            _ = tokio::time::sleep_until(self.deadline) => Err(AuthorizationError::Expired),
            result = self.receive() => result,
        }
    }

    async fn receive(&self) -> Result<AuthorizationGrant, AuthorizationError> {
        loop {
            let (mut stream, peer) = self
                .listener
                .accept()
                .await
                .map_err(|_| AuthorizationError::CallbackUnavailable)?;
            if !peer.ip().is_loopback() {
                continue;
            }
            let head = match timeout(Duration::from_secs(2), read_head(&mut stream)).await {
                Ok(Ok(head)) => head,
                _ => continue,
            };
            let callback = parse_callback(&head, &self.redirect, &self.state);
            let (status, message) = match &callback {
                Some(Ok(_)) => (
                    "200 OK",
                    "正在返回 OSIRAPI…客户端正在完成账户登录，请以客户端显示的结果为准。若未自动切回，请手动打开 OSIRAPI。",
                ),
                Some(Err(_)) => (
                    "200 OK",
                    "本次账户授权未获批准。请返回 OSIRAPI 查看结果或重新登录。",
                ),
                None => (
                    "400 Bad Request",
                    "此授权回调无效。请在原浏览器页面继续授权，或返回 OSIRAPI 重新登录。",
                ),
            };
            // Fixed text only: never echo codes, errors, state or URL parameters.
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{message}",
                message.len()
            );
            let _ = timeout(
                Duration::from_secs(1),
                stream.write_all(response.as_bytes()),
            )
            .await;
            if let Some(code) = callback {
                return code.map(|code| AuthorizationGrant {
                    code,
                    verifier: self.verifier.clone(),
                    redirect_uri: self.redirect.to_string(),
                });
            }
        }
    }
}

async fn read_head(stream: &mut TcpStream) -> Result<String, ()> {
    let mut head = Vec::new();
    let mut buf = [0; 1024];
    loop {
        let count = stream.read(&mut buf).await.map_err(|_| ())?;
        if count == 0 || head.len() + count > MAX_HEAD {
            return Err(());
        }
        head.extend_from_slice(&buf[..count]);
        if head.windows(4).any(|s| s == b"\r\n\r\n") {
            return String::from_utf8(head).map_err(|_| ());
        }
    }
}

fn parse_callback(
    head: &str,
    redirect: &Url,
    state: &str,
) -> Option<Result<String, AuthorizationError>> {
    let mut lines = head.split("\r\n");
    let line = lines.next()?;
    let parts: Vec<_> = line.split_whitespace().collect();
    if parts.len() != 3
        || parts[0] != "GET"
        || parts[2] != "HTTP/1.1"
        || !parts[1].starts_with("/osir/desktop/callback?")
    {
        return None;
    }
    let expected_host = format!("127.0.0.1:{}", redirect.port()?);
    let hosts: Vec<_> = lines
        .filter_map(|line| line.split_once(":"))
        .filter(|(name, _)| name.eq_ignore_ascii_case("host"))
        .map(|(_, value)| value.trim())
        .collect();
    if hosts != [expected_host.as_str()] {
        return None;
    }
    let callback = redirect.join(parts[1]).ok()?;
    if callback.path() != CALLBACK_PATH || callback.fragment().is_some() {
        return None;
    }
    let params: Vec<_> = callback.query_pairs().collect();
    let single = |name: &str| {
        let values: Vec<_> = params
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, v)| v.as_ref())
            .collect();
        if values.len() == 1 {
            Some(values[0])
        } else {
            None
        }
    };
    if single("state")? != state {
        return None;
    }
    let code = single("code");
    let error = single("error");
    // Reject duplicates and ambiguous outcomes rather than trusting parameter order.
    let code_count = params.iter().filter(|(key, _)| key == "code").count();
    let error_count = params.iter().filter(|(key, _)| key == "error").count();
    match (code, error, code_count, error_count) {
        (Some(code), None, 1, 0)
            if !code.is_empty() && code.len() <= 4096 && !code.chars().any(char::is_control) =>
        {
            Some(Ok(code.to_string()))
        }
        (None, Some(_), 0, 1) => Some(Err(AuthorizationError::Denied)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scope_replacement_is_unique_and_rejects_duplicate_permissions() {
        let mut flow = PendingAuthorization::bind("https://osir.example/authorize", "desktop")
            .await
            .unwrap();
        flow.set_scope("account:read keys:read keys:write").unwrap();
        flow.set_scope("account:read keys:read").unwrap();
        assert!(flow
            .set_scope("account:read keys:write keys:write")
            .is_err());
        let scopes: Vec<_> = flow
            .authorization_url
            .query_pairs()
            .filter(|(k, _)| k == "scope")
            .map(|(_, v)| v.into_owned())
            .collect();
        assert_eq!(scopes, vec!["account:read keys:read"]);
    }

    #[test]
    fn pkce_s256_matches_published_vector() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[tokio::test]
    async fn rejects_untrusted_authorization_endpoints() {
        for endpoint in [
            "http://osir.example/authorize",
            "https://u:p@osir.example/authorize",
            "https://osir.example/authorize?redirect_uri=bad",
            "https://osir.example/authorize#bad",
        ] {
            assert!(matches!(
                PendingAuthorization::bind(endpoint, "desktop").await,
                Err(AuthorizationError::InvalidConfiguration)
            ));
        }
    }

    #[tokio::test]
    async fn binds_random_loopback_port_and_keeps_verifier_native() {
        let flow = PendingAuthorization::bind("https://osir.example/authorize", "desktop")
            .await
            .unwrap();
        assert!(flow.listener.local_addr().unwrap().ip().is_loopback());
        assert_ne!(flow.redirect.port(), Some(0));
        assert_eq!(flow.verifier.len(), 43);
        assert!(!flow.browser_url().contains(&flow.verifier));
        let other = PendingAuthorization::bind("https://osir.example/authorize", "desktop")
            .await
            .unwrap();
        assert_ne!(flow.state, other.state);
        assert_ne!(flow.verifier, other.verifier);
    }

    #[test]
    fn callback_checks_host_path_state_and_single_outcome() {
        let redirect = Url::parse("http://127.0.0.1:43210/osir/desktop/callback").unwrap();
        let request = |query: &str| {
            format!("GET /osir/desktop/callback?{query} HTTP/1.1\r\nHost: 127.0.0.1:43210\r\n\r\n")
        };
        assert_eq!(
            parse_callback(&request("state=s&code=one"), &redirect, "s"),
            Some(Ok("one".into()))
        );
        assert_eq!(
            parse_callback(&request("state=s&error=access_denied"), &redirect, "s"),
            Some(Err(AuthorizationError::Denied))
        );
        for query in [
            "state=wrong&code=one",
            "state=s&state=s&code=one",
            "state=s&code=one&code=two",
            "state=s&code=one&error=no",
            "state=s&code=",
            "state=s&code=%0A",
        ] {
            assert_eq!(parse_callback(&request(query), &redirect, "s"), None);
        }
        let valid = request("state=s&code=one");
        for invalid in [
            valid.replace("Host: 127.0.0.1:43210", "Host: attacker.example"),
            valid.replace(CALLBACK_PATH, "/other"),
            valid.replace("GET", "POST"),
        ] {
            assert_eq!(parse_callback(&invalid, &redirect, "s"), None);
        }
    }

    #[tokio::test]
    async fn cancellation_and_expiry_close_listener() {
        let flow = PendingAuthorization::bind("https://osir.example/authorize", "desktop")
            .await
            .unwrap();
        let address = flow.listener.local_addr().unwrap();
        let (tx, rx) = oneshot::channel();
        tx.send(()).unwrap();
        assert!(matches!(
            flow.wait(rx).await,
            Err(AuthorizationError::Cancelled)
        ));
        assert!(TcpStream::connect(address).await.is_err());
        let mut flow = PendingAuthorization::bind("https://osir.example/authorize", "desktop")
            .await
            .unwrap();
        flow.deadline = Instant::now();
        let (_tx, rx) = oneshot::channel();
        assert!(matches!(
            flow.wait(rx).await,
            Err(AuthorizationError::Expired)
        ));
    }

    #[tokio::test]
    async fn real_callback_rejects_mismatch_then_accepts_one_time_code() {
        let flow = PendingAuthorization::bind("https://osir.example/authorize", "desktop")
            .await
            .unwrap();
        let address = flow.listener.local_addr().unwrap();
        let state = flow.state.clone();
        let (_tx, rx) = oneshot::channel();
        let task = tokio::spawn(flow.wait(rx));
        for (supplied_state, expected) in [("wrong", "400 Bad Request"), (state.as_str(), "200 OK")]
        {
            let mut socket = TcpStream::connect(address).await.unwrap();
            socket.write_all(format!("GET {CALLBACK_PATH}?state={supplied_state}&code=local-test-code HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes()).await.unwrap();
            let mut response = String::new();
            timeout(Duration::from_secs(3), socket.read_to_string(&mut response))
                .await
                .unwrap()
                .unwrap();
            assert!(response.contains(expected));
            assert!(!response.contains("local-test-code"));
            assert!(!response.contains(&state));
            if supplied_state == state {
                assert!(response.contains("正在返回 OSIRAPI"));
                assert!(response.contains("请以客户端显示的结果为准"));
                assert!(!response.contains("登录成功"));
                let (headers, body) = response.split_once("\r\n\r\n").unwrap();
                assert!(headers.contains(&format!("Content-Length: {}", body.len())));
                assert!(headers.contains("Cache-Control: no-store"));
                assert!(headers.contains("Referrer-Policy: no-referrer"));
            }
        }
        let grant = task.await.unwrap().unwrap();
        assert_eq!(grant.code, "local-test-code");
        assert_eq!(grant.verifier.len(), 43);
        assert!(grant.redirect_uri.starts_with("http://127.0.0.1:"));
        assert!(TcpStream::connect(address).await.is_err());
    }
}
