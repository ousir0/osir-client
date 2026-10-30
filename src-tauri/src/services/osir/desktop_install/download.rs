//! On-demand native transport; no IPC URLs, background work or installation.
use super::release::{self, Architecture, ReleasePlan, Source};
use codex_mac_engine::limits;
use reqwest::{header, Client, Response};
use std::{
    fs::File,
    future::Future,
    io::{Seek, SeekFrom, Write},
    os::unix::{fs::MetadataExt, io::AsRawFd},
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tempfile::{NamedTempFile, TempDir};
use tokio::sync::{watch, Mutex, Semaphore};
#[derive(Debug, PartialEq, Eq)]
pub enum DownloadError {
    Transport,
    Http,
    Policy,
    Size,
    Encoding,
    Signature,
    Io,
    Cancelled,
    Timeout,
    Busy,
    RateLimited,
    Feed,
}
pub type Result<T> = std::result::Result<T, DownloadError>;
// Initial request plus at most five additional GETs in the same operation.
// Redirects share this budget; neither a retry nor a redirect resets it.
const MAX_ARTIFACT_REQUESTS: usize = 6;
#[derive(Default)]
struct RequestBudget(AtomicUsize);
impl RequestBudget {
    fn charge(&self) -> Result<()> {
        self.0
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                (used < MAX_ARTIFACT_REQUESTS).then_some(used + 1)
            })
            .map(|_| ())
            .map_err(|_| DownloadError::Transport)
    }
    fn remaining(&self) -> bool {
        self.0.load(Ordering::SeqCst) < MAX_ARTIFACT_REQUESTS
    }
}
#[derive(Clone)]
pub struct Cancellation(watch::Sender<bool>);
impl Default for Cancellation {
    fn default() -> Self {
        Self(watch::channel(false).0)
    }
}
impl Cancellation {
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }
    pub(super) fn check(&self) -> Result<()> {
        if *self.0.borrow() {
            Err(DownloadError::Cancelled)
        } else {
            Ok(())
        }
    }
    async fn cancelled(&self) {
        let mut rx = self.0.subscribe();
        loop {
            if *rx.borrow_and_update() {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}
/// Native-only capability. Drop removes staging; only a read-only fd escapes.
pub struct VerifiedFile {
    file: File,
    _directory: TempDir,
    build: u64,
    size: u64,
}
impl VerifiedFile {
    pub fn file(&self) -> &File {
        &self.file
    }
    pub fn build(&self) -> u64 {
        self.build
    }
    pub fn size(&self) -> u64 {
        self.size
    }
}
struct Cached {
    source: Source,
    resolved_source: Source,
    arch: Architecture,
    xml: String,
    at: Instant,
}
#[derive(Default)]
struct FeedState {
    entries: Vec<Cached>,
    attempts: Vec<(Source, Architecture, Instant)>,
    last: Option<Instant>,
}
/// Share one instance per app lifetime. Clones preserve admission/cache state.
#[derive(Clone)]
pub struct ReleaseTransport {
    feeds: Arc<Mutex<FeedState>>,
    downloads: Arc<Semaphore>,
}
fn client_builder(https_only: bool) -> reqwest::ClientBuilder {
    Client::builder()
        .https_only(https_only)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(600))
}
fn client(https_only: bool, proxy_url: Option<&str>) -> Result<Client> {
    let mut builder = client_builder(https_only);
    if let Some(proxy_url) = proxy_url {
        let proxy = reqwest::Proxy::all(proxy_url).map_err(|_| DownloadError::Transport)?;
        builder = builder.proxy(proxy);
    }
    // Without an explicit CC Switch proxy, retain reqwest's system proxy
    // discovery. Routing does not change the fixed origins or TLS policy.
    builder.build().map_err(|_| DownloadError::Transport)
}
fn artifact_request_error(error: reqwest::Error) -> DownloadError {
    // Connection failures and timeouts are retryable. Invalid HTTP response
    // headers can fail before a Response exists and must remain terminal.
    if error.is_connect() || error.is_timeout() {
        DownloadError::Transport
    } else {
        DownloadError::Http
    }
}
async fn fetch_feed(
    http: &Client,
    source: Source,
    arch: Architecture,
    current_build: u64,
) -> Result<(String, ReleasePlan)> {
    let url = release::appcast_url(source, arch);
    let response = http
        .get(url)
        .header(header::ACCEPT_ENCODING, "identity")
        .send()
        .await
        .map_err(|_| DownloadError::Transport)?;
    release::validate_feed_response(source, arch, response.url().as_str())
        .map_err(|_| DownloadError::Policy)?;
    let bytes = read_feed(response).await?;
    let xml = String::from_utf8(bytes).map_err(|_| DownloadError::Feed)?;
    let plan = release::parse_release(source, arch, url, &xml, current_build)
        .map_err(|_| DownloadError::Feed)?;
    Ok((xml, plan))
}
fn choose_automatic(
    mirror: Result<(String, ReleasePlan)>,
    official: Result<(String, ReleasePlan)>,
) -> Result<(Source, String, ReleasePlan)> {
    let (source, (xml, plan)) = match (mirror, official) {
        (Ok(mirror), Ok(official)) if official.1.metadata().build > mirror.1.metadata().build => {
            (Source::Official, official)
        }
        (Ok(mirror), _) => (Source::OsirMirror, mirror),
        (Err(_), Ok(official)) => (Source::Official, official),
        (Err(error), Err(_)) => return Err(error),
    };
    Ok((source, xml, plan))
}
impl ReleaseTransport {
    pub fn new() -> Result<Self> {
        Ok(Self {
            feeds: Arc::new(Mutex::new(FeedState::default())),
            downloads: Arc::new(Semaphore::new(1)),
        })
    }
    pub async fn fetch_release(
        &self,
        source: Source,
        arch: Architecture,
        current_build: u64,
        cancel: &Cancellation,
    ) -> Result<ReleasePlan> {
        cancel.check()?;
        let mut state = self.feeds.try_lock().map_err(|_| DownloadError::Busy)?;
        let now = Instant::now();
        if let Some(c) = state.entries.iter().find(|c| {
            c.source == source
                && c.arch == arch
                && now.duration_since(c.at) < Duration::from_secs(60)
        }) {
            return release::parse_release(
                c.resolved_source,
                arch,
                release::appcast_url(c.resolved_source, arch),
                &c.xml,
                current_build,
            )
            .map_err(|_| DownloadError::Feed);
        }
        if state
            .last
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(15))
            || state.attempts.iter().any(|(s, a, t)| {
                *s == source && *a == arch && now.duration_since(*t) < Duration::from_secs(60)
            })
        {
            return Err(DownloadError::RateLimited);
        }
        state.last = Some(now);
        state
            .attempts
            .retain(|(s, a, _)| *s != source || *a != arch);
        state.attempts.push((source, arch, now)); // At most four pairs; failures count.
        let operation = async {
            // The installer is created before global proxy initialization;
            // resolve it on demand so saved settings and later changes apply.
            let http = client(
                true,
                crate::proxy::http_client::get_current_proxy_url().as_deref(),
            )?;
            let (resolved_source, xml, plan) = if source == Source::Auto {
                // Match Manager's automatic source: prefer the newer reachable
                // release. A short official probe keeps offline regions usable.
                let (mirror, official) = tokio::join!(
                    tokio::time::timeout(
                        Duration::from_secs(10),
                        fetch_feed(&http, Source::OsirMirror, arch, current_build)
                    ),
                    tokio::time::timeout(
                        Duration::from_secs(8),
                        fetch_feed(&http, Source::Official, arch, current_build)
                    )
                );
                choose_automatic(
                    mirror.unwrap_or(Err(DownloadError::Timeout)),
                    official.unwrap_or(Err(DownloadError::Timeout)),
                )?
            } else {
                let (xml, plan) = fetch_feed(&http, source, arch, current_build).await?;
                (source, xml, plan)
            };
            state
                .entries
                .retain(|c| c.source != source || c.arch != arch);
            state.entries.push(Cached {
                source,
                resolved_source,
                arch,
                xml,
                at: Instant::now(),
            });
            Ok(plan)
        };
        tokio::select! { biased; _ = cancel.cancelled() => Err(DownloadError::Cancelled), r = tokio::time::timeout(Duration::from_secs(30),operation) => r.map_err(|_| DownloadError::Timeout)? }
    }
    /// Parent is a native private cache directory, never an IPC argument.
    pub async fn download(
        &self,
        plan: ReleasePlan,
        parent: &Path,
        cancel: &Cancellation,
    ) -> Result<VerifiedFile> {
        self.download_observed(plan, parent, cancel, || {}).await
    }
    pub(super) async fn download_observed(
        &self,
        plan: ReleasePlan,
        parent: &Path,
        cancel: &Cancellation,
        verifying: impl FnOnce() + Send,
    ) -> Result<VerifiedFile> {
        cancel.check()?;
        let permit = self
            .downloads
            .clone()
            .try_acquire_owned()
            .map_err(|_| DownloadError::Busy)?;
        if !plan.metadata().update_available {
            return Err(DownloadError::Policy);
        }
        let directory = private_directory(parent)?;
        let operation = async {
            // Fix the HTTP client and proxy choice for the whole operation.
            // Every retry still starts from this same authenticated release plan.
            let http = client(
                true,
                crate::proxy::http_client::get_current_proxy_url().as_deref(),
            )?;
            let budget = RequestBudget::default();
            let file = stream_artifact(
                &directory,
                plan.metadata().full_size,
                cancel,
                &budget,
                |offset| self.artifact_response(&http, &plan, offset, &budget),
            )
            .await?;
            cancel.check()?;
            verifying();
            let token = cancel.clone();
            // A cancelled blocking verifier retains its staging and admission
            // permit until it exits; it cannot publish to a dropped caller.
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                token.check()?;
                verify_file(&file, &plan)?;
                token.check()?;
                Ok(VerifiedFile {
                    file,
                    _directory: directory,
                    build: plan.metadata().build,
                    size: plan.metadata().full_size,
                })
            })
            .await
            .map_err(|_| DownloadError::Io)?
        };
        tokio::select! { biased; _ = cancel.cancelled() => Err(DownloadError::Cancelled), r = tokio::time::timeout(Duration::from_secs(600),operation) => r.map_err(|_| DownloadError::Timeout)? }
    }
    async fn artifact_response(
        &self,
        http: &Client,
        plan: &ReleasePlan,
        offset: u64,
        budget: &RequestBudget,
    ) -> Result<Response> {
        let mut url = plan.download_url().to_owned();
        for hop in 0..=5 {
            // The initial GET is charged by stream_artifact; each redirect is
            // another actual GET and consumes the same bounded request budget.
            if hop > 0 {
                budget.charge()?;
            }
            let mut request = http.get(&url).header(header::ACCEPT_ENCODING, "identity");
            if offset > 0 {
                request = request.header(header::RANGE, format!("bytes={offset}-"));
            }
            let response = request.send().await.map_err(artifact_request_error)?;
            if response.url().as_str() != url {
                return Err(DownloadError::Policy);
            }
            if response.status().is_redirection() {
                if !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                    return Err(DownloadError::Policy);
                }
                let values: Vec<_> = response
                    .headers()
                    .get_all(header::LOCATION)
                    .iter()
                    .collect();
                if values.len() != 1 {
                    return Err(DownloadError::Policy);
                }
                let next = values[0].to_str().map_err(|_| DownloadError::Policy)?;
                plan.validate_redirect(&url, next, hop)
                    .map_err(|_| DownloadError::Policy)?;
                url = next.to_owned();
            } else {
                return Ok(response);
            }
        }
        Err(DownloadError::Policy)
    }
}
fn headers(response: &Response, limit: u64, exact: bool) -> Result<()> {
    if response.status() != reqwest::StatusCode::OK {
        return Err(DownloadError::Http);
    }
    for encoding in response.headers().get_all(header::CONTENT_ENCODING) {
        if encoding.as_bytes() != b"identity" {
            return Err(DownloadError::Encoding);
        }
    }
    let lengths: Vec<_> = response
        .headers()
        .get_all(header::CONTENT_LENGTH)
        .iter()
        .collect();
    if lengths.len() > 1 {
        return Err(DownloadError::Size);
    }
    if let Some(v) = lengths.first() {
        let size = v
            .to_str()
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or(DownloadError::Size)?;
        if size > limit || (exact && size != limit) {
            return Err(DownloadError::Size);
        }
    }
    Ok(())
}
async fn read_feed(mut response: Response) -> Result<Vec<u8>> {
    headers(&response, limits::MAX_TEXT_BYTES, false)?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| DownloadError::Transport)?
    {
        if chunk.len() as u64 > limits::MAX_TEXT_BYTES - bytes.len() as u64 {
            return Err(DownloadError::Size);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn private_directory(parent: &Path) -> Result<TempDir> {
    let canonical = parent.canonicalize().map_err(|_| DownloadError::Io)?;
    let meta = std::fs::symlink_metadata(parent).map_err(|_| DownloadError::Io)?;
    if canonical != parent
        || !meta.is_dir()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
    {
        return Err(DownloadError::Policy);
    }
    tempfile::Builder::new()
        .prefix("codex-download-")
        .tempdir_in(parent)
        .map_err(|_| DownloadError::Io)
}
fn artifact_offset(response: &Response, offset: u64, expected: u64) -> Result<u64> {
    if response.status() == reqwest::StatusCode::OK {
        headers(response, expected, true)?;
        // A full response must not also claim to be a range body.
        if response.headers().contains_key(header::CONTENT_RANGE) {
            return Err(DownloadError::Size);
        }
        return Ok(0);
    }
    if offset == 0 || response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(DownloadError::Http);
    }
    for encoding in response.headers().get_all(header::CONTENT_ENCODING) {
        if encoding.as_bytes() != b"identity" {
            return Err(DownloadError::Encoding);
        }
    }
    if response
        .headers()
        .get_all(header::CONTENT_ENCODING)
        .iter()
        .count()
        > 1
        || response.headers().contains_key(header::TRANSFER_ENCODING)
    {
        return Err(DownloadError::Encoding);
    }
    let single = |name: header::HeaderName| -> Result<&str> {
        let all = response.headers().get_all(name);
        let mut values = all.iter();
        let value = values.next().ok_or(DownloadError::Size)?;
        if values.next().is_some() {
            return Err(DownloadError::Size);
        }
        value.to_str().map_err(|_| DownloadError::Size)
    };
    let number = |value: &str| -> Result<u64> {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(DownloadError::Size);
        }
        value.parse().map_err(|_| DownloadError::Size)
    };
    let range = single(header::CONTENT_RANGE)?
        .strip_prefix("bytes ")
        .ok_or(DownloadError::Size)?;
    let (bounds, total) = range.split_once('/').ok_or(DownloadError::Size)?;
    let (start, end) = bounds.split_once('-').ok_or(DownloadError::Size)?;
    let (start, end, total) = (number(start)?, number(end)?, number(total)?);
    let remaining = expected
        .checked_sub(offset)
        .filter(|length| *length > 0)
        .ok_or(DownloadError::Size)?;
    if start != offset
        || total != expected
        || end.checked_add(1) != Some(expected)
        || end
            .checked_sub(start)
            .and_then(|length| length.checked_add(1))
            != Some(remaining)
        || number(single(header::CONTENT_LENGTH)?)? != remaining
    {
        return Err(DownloadError::Size);
    }
    Ok(offset)
}

async fn retry_transport(budget: &RequestBudget, cancel: &Cancellation) -> Result<()> {
    cancel.check()?;
    if !budget.remaining() {
        return Err(DownloadError::Transport);
    }
    let delay = Duration::from_millis(100 * budget.0.load(Ordering::SeqCst) as u64);
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(DownloadError::Cancelled),
        _ = tokio::time::sleep(delay) => cancel.check(),
    }
}

// The private request seam permits loopback fixtures without adding a production
// URL/client override. Production binds it to artifact_response and one plan.
async fn stream_artifact<F, Fut>(
    directory: &TempDir,
    expected: u64,
    cancel: &Cancellation,
    budget: &RequestBudget,
    mut response_for: F,
) -> Result<File>
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = Result<Response>>,
{
    if expected == 0 || expected > limits::MAX_PACKAGE_BYTES {
        return Err(DownloadError::Size);
    }
    let mut temp = NamedTempFile::new_in(directory.path()).map_err(|_| DownloadError::Io)?;
    let mut size = 0u64;
    'requests: loop {
        cancel.check()?;
        budget.charge()?;
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(DownloadError::Cancelled),
            response = response_for(size) => response,
        };
        let mut response = match response {
            Ok(response) => response,
            Err(DownloadError::Transport) => {
                retry_transport(budget, cancel).await?;
                continue;
            }
            Err(error) => return Err(error),
        };
        let start = artifact_offset(&response, size, expected)?;
        if start == 0 && size > 0 {
            // A server may ignore Range. A validated 200 replaces the partial
            // body; it must never be appended to previously received bytes.
            temp.as_file().set_len(0).map_err(|_| DownloadError::Io)?;
            temp.as_file_mut()
                .seek(SeekFrom::Start(0))
                .map_err(|_| DownloadError::Io)?;
            size = 0;
        }
        loop {
            let chunk = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(DownloadError::Cancelled),
                chunk = response.chunk() => chunk,
            };
            let chunk = match chunk {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(_) => {
                    // If all bytes arrived but the framing failed afterward,
                    // restart safely rather than requesting an empty range or
                    // treating unconfirmed EOF as a successfully completed body.
                    if size == expected {
                        temp.as_file().set_len(0).map_err(|_| DownloadError::Io)?;
                        temp.as_file_mut()
                            .seek(SeekFrom::Start(0))
                            .map_err(|_| DownloadError::Io)?;
                        size = 0;
                    }
                    retry_transport(budget, cancel).await?;
                    continue 'requests;
                }
            };
            cancel.check()?;
            if chunk.len() as u64 > expected.checked_sub(size).ok_or(DownloadError::Size)? {
                return Err(DownloadError::Size);
            }
            temp.write_all(&chunk).map_err(|_| DownloadError::Io)?;
            size = size
                .checked_add(chunk.len() as u64)
                .ok_or(DownloadError::Size)?;
        }
        // A clean EOF at the wrong size is a protocol/size failure, not a
        // transport interruption; do not retry it or weaken range validation.
        if size != expected {
            return Err(DownloadError::Size);
        }
        break;
    }
    cancel.check()?;
    temp.as_file().sync_all().map_err(|_| DownloadError::Io)?;
    let file = File::open(temp.path()).map_err(|_| DownloadError::Io)?;
    temp.close().map_err(|_| DownloadError::Io)?; // Unlink before authentication.
    Ok(file)
}
fn verify_file(file: &File, plan: &ReleasePlan) -> Result<()> {
    let len = file.metadata().map_err(|_| DownloadError::Io)?.len();
    if len == 0
        || len != plan.metadata().full_size
        || len > limits::MAX_PACKAGE_BYTES
        || len > isize::MAX as u64
    {
        return Err(DownloadError::Size);
    }
    // Pure Ed25519 requires the whole message; read-only mmap avoids a second
    // package-sized allocation. This fd is unlinked and has no writable clone.
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len as usize,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        return Err(DownloadError::Io);
    }
    struct Mapping(*mut libc::c_void, usize);
    impl Drop for Mapping {
        fn drop(&mut self) {
            unsafe {
                libc::munmap(self.0, self.1);
            }
        }
    }
    let mapping = Mapping(ptr, len as usize);
    let bytes = unsafe { std::slice::from_raw_parts(mapping.0.cast::<u8>(), mapping.1) };
    plan.verify_artifact(bytes)
        .map_err(|_| DownloadError::Signature)?;
    Ok(())
}
#[cfg(test)]
#[path = "download_tests.rs"]
mod tests;
