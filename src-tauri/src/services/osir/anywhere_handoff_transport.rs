//! macOS-only, native handoff transport. Intentionally not wired into an entry point.
//!
//! Discovery: <trusted home>/Library/Application Support/OSIR/handoff/v1/<flowId>.json
//! contains ONLY {version:1,flowId,socketPath,clientPid}. The socket is under a
//! fresh 0700 /private/tmp/oh-* directory (short enough for sockaddr_un).
//! Wire: u32 big-endian payload length + UTF-8 JSON; TOTAL frame <= 4096 bytes.
//! Exactly one frame each way; receiver MUST half-close its write side.
//! PeerVerifier is mandatory: bind the kernel PID/path/parent to the full bundle
//! pinned by the release-signed handoff-trust.json. Same UID is not trust.
//! No launch/API/logging is performed here. Call launch_arguments() only after
//! the controller has verified the installed receiver and explicit user intent.
#![cfg(target_os = "macos")]

use super::anywhere_handoff::{HandoffCode, HandoffRequest};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Serialize;
use std::{
    ffi::{CStr, CString},
    fs::{self, File},
    future::Future,
    io::{Read, Write},
    mem,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, PermissionsExt},
        },
    },
    path::{Component, Path, PathBuf},
    pin::Pin,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::watch,
};

pub(crate) const DISCOVERY_RELATIVE: &str = "Library/Application Support/OSIR/handoff/v1";
pub(crate) const MAX_FRAME_BYTES: usize = 4096;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const FLOW_TIMEOUT: Duration = Duration::from_secs(300);

/// Deliberately fixed, non-sensitive errors. Never propagate raw API/I/O bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportError {
    InvalidFrame,
    UnsafePath,
    PeerUntrusted,
    Timeout,
    Cancelled,
    Unavailable,
    AuthorizationFailed,
}
type Result<T> = std::result::Result<T, TransportError>;

/// Every field comes from this connected socket and libproc, never request JSON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PeerIdentity {
    pub uid: u32,
    pub pid: i32,
    pub executable_path: PathBuf,
    pub parent_pid: i32,
    pub parent_executable_path: PathBuf,
    // Detect PID reuse during authorization; controller must additionally bind
    // the running executable/build and prevent replacement, as the contract says.
    pub started_at: (u64, u64),
}

pub(crate) trait PeerVerifier: Send + Sync {
    fn verify<'a>(
        &'a self,
        peer: &'a PeerIdentity,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Discovery<'a> {
    version: u8,
    flow_id: &'a str,
    socket_path: &'a Path,
    client_pid: u32,
}

/// Owns one flow. Dropping/cancelling it closes the listener and removes discovery.
pub(crate) struct HandoffTransport {
    listener: UnixListener,
    directory: tempfile::TempDir,
    discovery_dir: OwnedFd,
    discovery_name: CString,
    discovery_identity: (u64, u64),
    flow_id: String,
    created_at: Instant,
    peer_connected: watch::Sender<bool>,
}

// Walk anchored directory descriptors with O_NOFOLLOW. Never follow a symlink,
// even if a checked pathname is concurrently replaced. Only the final protocol
// directory requires 0700; pre-existing Library/Application Support may be 0755.
fn private_discovery(home: &Path) -> Result<OwnedFd> {
    if !home.is_absolute() {
        return Err(TransportError::UnsafePath);
    }
    let mut fd = unsafe {
        libc::open(
            c"/".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(TransportError::UnsafePath);
    }
    let mut owned = unsafe { OwnedFd::from_raw_fd(fd) };
    let target = home.join(DISCOVERY_RELATIVE);
    let uid = unsafe { libc::geteuid() };
    let mut reached_home = false;
    let mut walked = PathBuf::from("/");
    for component in target.components() {
        let Component::Normal(name) = component else {
            if component == Component::RootDir {
                continue;
            }
            return Err(TransportError::UnsafePath);
        };
        walked.push(name);
        let name = CString::new(name.as_bytes()).map_err(|_| TransportError::UnsafePath)?;
        fd = unsafe {
            libc::openat(
                owned.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0
            && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
            && reached_home
        {
            if unsafe { libc::mkdirat(owned.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
                return Err(TransportError::UnsafePath);
            }
            fd = unsafe {
                libc::openat(
                    owned.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
        }
        if fd < 0 {
            return Err(TransportError::UnsafePath);
        }
        owned = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut stat: libc::stat = unsafe { mem::zeroed() };
        if unsafe { libc::fstat(owned.as_raw_fd(), &mut stat) } != 0 {
            return Err(TransportError::UnsafePath);
        }
        if walked == home {
            reached_home = true;
        }
        if reached_home && (stat.st_uid != uid || stat.st_mode & 0o022 != 0) {
            return Err(TransportError::UnsafePath);
        }
        if walked == target && stat.st_mode & 0o777 != 0o700 {
            return Err(TransportError::UnsafePath);
        }
    }
    Ok(owned)
}

impl HandoffTransport {
    /// `home` MUST come from trusted native OS discovery, never renderer/argv.
    pub(crate) fn bind(home: &Path) -> Result<Self> {
        let discovery_dir = private_discovery(home)?;
        let mut entropy = [0u8; 32];
        File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut entropy))
            .map_err(|_| TransportError::Unavailable)?;
        let flow_id = URL_SAFE_NO_PAD.encode(entropy);
        let directory = tempfile::Builder::new()
            .prefix("oh-")
            .tempdir_in("/private/tmp")
            .map_err(|_| TransportError::Unavailable)?;
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
            .map_err(|_| TransportError::UnsafePath)?;
        let socket_path = directory.path().join("s");
        let listener = std::os::unix::net::UnixListener::bind(&socket_path)
            .map_err(|_| TransportError::Unavailable)?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))
            .map_err(|_| TransportError::UnsafePath)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| TransportError::Unavailable)?;
        let listener = UnixListener::from_std(listener).map_err(|_| TransportError::Unavailable)?;
        let discovery_name = CString::new(format!("{flow_id}.json")).unwrap();
        let fd = unsafe {
            libc::openat(
                discovery_dir.as_raw_fd(),
                discovery_name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(TransportError::UnsafePath);
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata().map_err(|_| TransportError::UnsafePath)?;
        let (peer_connected, _) = watch::channel(false);
        let transport = Self {
            listener,
            directory,
            discovery_dir,
            discovery_name,
            discovery_identity: (metadata.dev(), metadata.ino()),
            flow_id,
            created_at: Instant::now(),
            peer_connected,
        };
        let manifest = serde_json::to_vec(&Discovery {
            version: 1,
            flow_id: &transport.flow_id,
            socket_path: &socket_path,
            client_pid: std::process::id(),
        })
        .map_err(|_| TransportError::Unavailable)?;
        file.write_all(&manifest)
            .and_then(|_| file.sync_all())
            .map_err(|_| TransportError::Unavailable)?;
        Ok(transport)
    }

    pub(crate) fn flow_id(&self) -> &str {
        &self.flow_id
    }
    pub(crate) fn launch_arguments(&self) -> [&str; 2] {
        ["--osir-client-handoff-flow", &self.flow_id]
    }

    pub(crate) fn peer_connected(&self) -> watch::Receiver<bool> {
        self.peer_connected.subscribe()
    }

    /// Consumes the flow even on malformed/untrusted peers. No automatic replay.
    /// The authorization callback must also check account generation before and
    /// after its request. Cancellation drops its future; it must not detach work.
    pub(crate) async fn serve<V, A, F>(
        self,
        verifier: &V,
        authorize: A,
        configured_tools: Vec<String>,
        mut cancelled: watch::Receiver<bool>,
    ) -> Result<()>
    where
        V: PeerVerifier,
        A: FnOnce(HandoffRequest) -> F,
        F: Future<Output = Result<HandoffCode>>,
    {
        let mut previous = None;
        for tool in &configured_tools {
            let index = ["claude", "codex", "dsh"]
                .iter()
                .position(|allowed| allowed == tool)
                .ok_or(TransportError::InvalidFrame)?;
            if previous.is_some_and(|value| index <= value) {
                return Err(TransportError::InvalidFrame);
            }
            previous = Some(index);
        }
        let remaining = FLOW_TIMEOUT
            .checked_sub(self.created_at.elapsed())
            .ok_or(TransportError::Timeout)?;
        let operation = async {
            let (mut stream, _) = self
                .listener
                .accept()
                .await
                .map_err(|_| TransportError::Unavailable)?;
            let peer = peer_identity(&stream)?;
            verifier
                .verify(&peer)
                .await
                .map_err(|_| TransportError::PeerUntrusted)?;
            let _ = self.peer_connected.send(true);
            let bytes = tokio::time::timeout(IO_TIMEOUT, read_frame(&mut stream))
                .await
                .map_err(|_| TransportError::Timeout)??;
            // Direct typed deserialization rejects duplicate/unknown fields and
            // non-integer version tokens, unlike parsing through a JSON Value.
            let request: HandoffRequest =
                serde_json::from_slice(&bytes).map_err(|_| TransportError::InvalidFrame)?;
            if !request.validate() || request.flow_id != self.flow_id {
                return Err(TransportError::InvalidFrame);
            }
            let state = request.state.clone();
            let authorization_started = Instant::now();
            let code = authorize(request)
                .await
                .map_err(|_| TransportError::AuthorizationFailed)?;
            if !code.validate() {
                return Err(TransportError::AuthorizationFailed);
            }
            if peer_identity(&stream)? != peer {
                return Err(TransportError::PeerUntrusted);
            }
            verifier
                .verify(&peer)
                .await
                .map_err(|_| TransportError::PeerUntrusted)?;
            // Start before the POST, conservatively rounding elapsed time up.
            // Never extend the server's 60-second lifetime across local checks.
            let elapsed = authorization_started.elapsed().as_secs().saturating_add(1);
            let expires_in = 60u64
                .checked_sub(elapsed)
                .filter(|n| *n > 0)
                .ok_or(TransportError::Timeout)?;
            #[derive(Serialize)]
            #[serde(rename_all = "camelCase")]
            struct Delivery<'a> {
                flow_id: &'a str,
                state: &'a str,
                code: &'a str,
                expires_in: u64,
                configured_tool_types: &'a [String],
            }
            let mut bytes = serde_json::to_vec(&Delivery {
                flow_id: &self.flow_id,
                state: &state,
                code: &code.code,
                expires_in,
                configured_tool_types: &configured_tools,
            })
            .map_err(|_| TransportError::InvalidFrame)?;
            tokio::time::timeout(IO_TIMEOUT, write_frame(&mut stream, &bytes))
                .await
                .map_err(|_| TransportError::Timeout)??;
            bytes.fill(0);
            drop(code);
            // Keep the kernel connection alive for Main's second inspection.
            // The helper retains Main's duplicated endpoint even after Node's
            // EOF auto-close. No second frame/secret acknowledgment is needed.
            let until = tokio::time::Instant::now() + IO_TIMEOUT;
            while tokio::time::Instant::now() < until {
                let mut pid: libc::pid_t = 0;
                let mut length = mem::size_of_val(&pid) as libc::socklen_t;
                if unsafe {
                    libc::getsockopt(
                        stream.as_raw_fd(),
                        libc::SOL_LOCAL,
                        libc::LOCAL_PEERPID,
                        (&mut pid as *mut libc::pid_t).cast(),
                        &mut length,
                    )
                } != 0
                    || pid != peer.pid
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok(())
        };
        tokio::select! {
            biased;
            _ = async { loop { if *cancelled.borrow() { break; } if cancelled.changed().await.is_err() { break; } } } => Err(TransportError::Cancelled),
            result = tokio::time::timeout(remaining, operation) => result.map_err(|_| TransportError::Timeout)?,
        }
    }
}

impl Drop for HandoffTransport {
    fn drop(&mut self) {
        let mut stat: libc::stat = unsafe { mem::zeroed() };
        if unsafe {
            libc::fstatat(
                self.discovery_dir.as_raw_fd(),
                self.discovery_name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
            && (stat.st_dev as u64, stat.st_ino) == self.discovery_identity
        {
            unsafe {
                libc::unlinkat(
                    self.discovery_dir.as_raw_fd(),
                    self.discovery_name.as_ptr(),
                    0,
                );
            }
        }
        // TempDir cleans only the private flow directory, never a shared root.
        let _ = &self.directory;
    }
}

fn process_path(pid: i32) -> Result<PathBuf> {
    let mut bytes = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    if unsafe { libc::proc_pidpath(pid, bytes.as_mut_ptr().cast(), bytes.len() as u32) } <= 0 {
        return Err(TransportError::PeerUntrusted);
    }
    let value = CStr::from_bytes_until_nul(&bytes).map_err(|_| TransportError::PeerUntrusted)?;
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(value.to_bytes()));
    if !path.is_absolute() {
        return Err(TransportError::PeerUntrusted);
    }
    Ok(path)
}

fn peer_identity(stream: &UnixStream) -> Result<PeerIdentity> {
    let mut uid = 0;
    let mut gid = 0;
    let mut pid: libc::pid_t = 0;
    let mut length = mem::size_of_val(&pid) as libc::socklen_t;
    let fd = stream.as_raw_fd();
    if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0
        || unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                (&mut pid as *mut libc::pid_t).cast(),
                &mut length,
            )
        } != 0
        || length as usize != mem::size_of_val(&pid)
        || pid <= 0
        || uid != unsafe { libc::geteuid() }
    {
        return Err(TransportError::PeerUntrusted);
    }
    let mut info: libc::proc_bsdinfo = unsafe { mem::zeroed() };
    let size = mem::size_of_val(&info) as i32;
    if unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    } != size
        || info.pbi_pid != pid as u32
        || info.pbi_uid != uid
        || info.pbi_ppid == 0
    {
        return Err(TransportError::PeerUntrusted);
    }
    Ok(PeerIdentity {
        uid,
        pid,
        executable_path: process_path(pid)?,
        parent_pid: info.pbi_ppid as i32,
        parent_executable_path: process_path(info.pbi_ppid as i32)?,
        started_at: (info.pbi_start_tvsec, info.pbi_start_tvusec),
    })
}

async fn read_frame(stream: &mut UnixStream) -> Result<Vec<u8>> {
    let length = stream
        .read_u32()
        .await
        .map_err(|_| TransportError::InvalidFrame)? as usize;
    if length == 0 || length > MAX_FRAME_BYTES - 4 {
        return Err(TransportError::InvalidFrame);
    }
    let mut bytes = vec![0; length];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|_| TransportError::InvalidFrame)?;
    let mut extra = [0u8; 1];
    if stream
        .read(&mut extra)
        .await
        .map_err(|_| TransportError::InvalidFrame)?
        != 0
    {
        return Err(TransportError::InvalidFrame);
    }
    Ok(bytes)
}

async fn write_frame(stream: &mut UnixStream, bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME_BYTES - 4 {
        return Err(TransportError::InvalidFrame);
    }
    stream
        .write_u32(bytes.len() as u32)
        .await
        .map_err(|_| TransportError::Unavailable)?;
    stream
        .write_all(bytes)
        .await
        .map_err(|_| TransportError::Unavailable)?;
    stream
        .shutdown()
        .await
        .map_err(|_| TransportError::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Verify {
        calls: AtomicUsize,
        deny: bool,
    }
    impl PeerVerifier for Verify {
        fn verify<'a>(
            &'a self,
            peer: &'a PeerIdentity,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                assert_eq!(peer.pid, std::process::id() as i32);
                assert!(peer.executable_path.is_absolute());
                assert!(peer.parent_pid > 0 && peer.parent_executable_path.is_absolute());
                if self.deny {
                    Err(TransportError::PeerUntrusted)
                } else {
                    Ok(())
                }
            })
        }
    }
    fn verifier(deny: bool) -> Verify {
        Verify {
            calls: AtomicUsize::new(0),
            deny,
        }
    }
    fn home() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("oh-test-")
            .tempdir_in("/private/tmp")
            .unwrap()
    }
    fn request(flow: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({"version":1,"flowId":flow,"receiverInstanceId":"r".repeat(43),
            "state":URL_SAFE_NO_PAD.encode([1;32]),"codeChallenge":URL_SAFE_NO_PAD.encode([2;32]),
            "codeChallengeMethod":"S256","recipientClientId":"osir-anywhere-desktop",
            "targetOrigin":"https://anywhere.osirclaw.com","actions":["identity","connect-local","bootstrap-configured-tools"]})).unwrap()
    }
    fn code() -> HandoffCode {
        HandoffCode {
            code: URL_SAFE_NO_PAD.encode([3; 32]),
            expires_in: 60,
            recipient_client_id: "osir-anywhere-desktop".into(),
            audience: "osir-anywhere-server".into(),
        }
    }

    #[tokio::test]
    async fn permissions_discovery_launch_and_drop_are_private() {
        let home = home();
        let t = HandoffTransport::bind(home.path()).unwrap();
        assert_eq!(t.flow_id().len(), 43);
        assert_eq!(
            t.launch_arguments(),
            ["--osir-client-handoff-flow", t.flow_id()]
        );
        let file = home
            .path()
            .join(DISCOVERY_RELATIVE)
            .join(format!("{}.json", t.flow_id()));
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 4);
        assert_eq!(
            fs::metadata(file.parent().unwrap()).unwrap().mode() & 0o777,
            0o700
        );
        assert_eq!(fs::metadata(&file).unwrap().mode() & 0o777, 0o600);
        let socket = PathBuf::from(value["socketPath"].as_str().unwrap());
        assert_eq!(fs::metadata(&socket).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            fs::metadata(socket.parent().unwrap()).unwrap().mode() & 0o777,
            0o700
        );
        drop(t);
        assert!(!file.exists() && !socket.exists());
    }

    #[tokio::test]
    async fn authenticated_one_shot_delivery_rechecks_peer() {
        let home = home();
        let t = HandoffTransport::bind(home.path()).unwrap();
        let mut connected = t.peer_connected();
        let bytes = request(t.flow_id());
        let socket = t.directory.path().join("s");
        let verify = verifier(false);
        let (cancel, rx) = watch::channel(false);
        let client = async {
            let mut stream = UnixStream::connect(socket).await.unwrap();
            write_frame(&mut stream, &bytes).await.unwrap();
            let delivery = read_frame(&mut stream).await.unwrap();
            let v: serde_json::Value = serde_json::from_slice(&delivery).unwrap();
            assert_eq!(v["code"], URL_SAFE_NO_PAD.encode([3; 32]));
            assert_eq!(v["expiresIn"], 59);
            assert_eq!(v.as_object().unwrap().len(), 5);
            assert_eq!(v["configuredToolTypes"], json!(["claude", "codex"]));
            // A second real socket identity lookup still works after EOF.
            assert_eq!(
                peer_identity(&stream).unwrap().pid,
                std::process::id() as i32
            );
        };
        let (result, _) = tokio::join!(
            t.serve(
                &verify,
                |_| async { Ok(code()) },
                vec!["claude".into(), "codex".into()],
                rx
            ),
            client
        );
        drop(cancel);
        assert_eq!(result, Ok(()));
        assert!(*connected.borrow_and_update());
        assert_eq!(verify.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn failed_peer_never_authorizes() {
        let home = home();
        let t = HandoffTransport::bind(home.path()).unwrap();
        let connected = t.peer_connected();
        let socket = t.directory.path().join("s");
        let verify = verifier(true);
        let (_cancel, rx) = watch::channel(false);
        let (result, stream) = tokio::join!(
            t.serve(
                &verify,
                |_| async { panic!("must not authorize") },
                vec![],
                rx
            ),
            UnixStream::connect(socket)
        );
        assert!(stream.is_ok());
        assert_eq!(result, Err(TransportError::PeerUntrusted));
        assert!(!*connected.borrow());
    }

    #[tokio::test]
    async fn rejects_unknown_duplicate_wrong_flow_and_non_integer() {
        for variant in 0..5 {
            let home = home();
            let t = HandoffTransport::bind(home.path()).unwrap();
            let socket = t.directory.path().join("s");
            let mut text = String::from_utf8(request(t.flow_id())).unwrap();
            match variant {
                0 => text.insert_str(1, "\"codeVerifier\":\"never\","),
                1 => text.insert_str(1, "\"version\":1,"),
                2 => text = text.replace(t.flow_id(), &"x".repeat(43)),
                3 => text = text.replace("\"version\":1", "\"version\":1.0"),
                _ => text.push_str("{}"),
            }
            let verify = verifier(false);
            let (_cancel, rx) = watch::channel(false);
            let (result, _) = tokio::join!(
                t.serve(
                    &verify,
                    |_| async { panic!("must not authorize") },
                    vec![],
                    rx
                ),
                async {
                    let mut stream = UnixStream::connect(socket).await.unwrap();
                    write_frame(&mut stream, text.as_bytes()).await.unwrap();
                    // Keep the connected endpoint alive until peer inspection.
                    let mut discarded = Vec::new();
                    let _ = stream.read_to_end(&mut discarded).await;
                }
            );
            assert_eq!(result, Err(TransportError::InvalidFrame));
        }
    }

    #[tokio::test]
    async fn frame_rejects_oversize_truncation_extra_frame_and_no_half_close() {
        for bytes in [
            vec![0, 0, 16, 0],
            vec![0, 0, 0, 4, b'{'],
            vec![0, 0, 0, 2, b'{', b'}', 0],
        ] {
            let (mut tx, mut rx) = UnixStream::pair().unwrap();
            tx.write_all(&bytes).await.unwrap();
            tx.shutdown().await.unwrap();
            assert_eq!(read_frame(&mut rx).await, Err(TransportError::InvalidFrame));
        }
        let (mut tx, mut rx) = UnixStream::pair().unwrap();
        tx.write_all(&[0, 0, 0, 2, b'{', b'}']).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(15), read_frame(&mut rx))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn cancellation_discards_late_authorization_and_cleans_discovery() {
        let home = home();
        let t = HandoffTransport::bind(home.path()).unwrap();
        let bytes = request(t.flow_id());
        let socket = t.directory.path().join("s");
        let file = home
            .path()
            .join(DISCOVERY_RELATIVE)
            .join(format!("{}.json", t.flow_id()));
        let verify = verifier(false);
        let (cancel, rx) = watch::channel(false);
        let (entered, mut reached) = tokio::sync::mpsc::channel(1);
        let (result, _) = tokio::join!(
            t.serve(
                &verify,
                |_| async {
                    entered.send(()).await.unwrap();
                    std::future::pending::<Result<HandoffCode>>().await
                },
                vec![],
                rx
            ),
            async {
                let mut stream = UnixStream::connect(socket).await.unwrap();
                write_frame(&mut stream, &bytes).await.unwrap();
                reached.recv().await.unwrap();
                cancel.send(true).unwrap();
                let mut bytes = Vec::new();
                stream.read_to_end(&mut bytes).await.unwrap();
                assert!(bytes.is_empty());
            }
        );
        assert_eq!(result, Err(TransportError::Cancelled));
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn symlink_or_permissive_discovery_is_rejected() {
        let home = home();
        let t = HandoffTransport::bind(home.path()).unwrap();
        drop(t);
        let root = home.path().join(DISCOVERY_RELATIVE);
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            HandoffTransport::bind(home.path()),
            Err(TransportError::UnsafePath)
        ));
        fs::remove_dir(&root).unwrap();
        let other = home.path().join("other");
        fs::create_dir(&other).unwrap();
        std::os::unix::fs::symlink(other, &root).unwrap();
        assert!(matches!(
            HandoffTransport::bind(home.path()),
            Err(TransportError::UnsafePath)
        ));
    }

    #[tokio::test]
    async fn rejects_unconfigured_tool_names_duplicates_and_wrong_order() {
        for tools in [
            vec!["opencode"],
            vec!["claude", "claude"],
            vec!["dsh", "codex"],
        ] {
            let home = home();
            let t = HandoffTransport::bind(home.path()).unwrap();
            let verify = verifier(false);
            let (_cancel, rx) = watch::channel(false);
            assert_eq!(
                t.serve(
                    &verify,
                    |_| async { panic!("must not authorize") },
                    tools.into_iter().map(String::from).collect(),
                    rx
                )
                .await,
                Err(TransportError::InvalidFrame)
            );
            assert_eq!(verify.calls.load(Ordering::SeqCst), 0);
        }
    }
}
