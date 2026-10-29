use super::*;
use std::io::{Read, Write};

// Only this private helper can use HTTP; production client remains HTTPS-only.
async fn fixture(raw: Vec<u8>) -> Response {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = [0; 4096];
        let _ = socket.read(&mut request);
        socket.write_all(&raw).unwrap();
    });
    client_builder(false)
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("http://{address}/fixture"))
        .send()
        .await
        .unwrap()
}

async fn single_stream(response: Response, directory: &TempDir, expected: u64) -> Result<File> {
    let cancel = Cancellation::default();
    let budget = RequestBudget::default();
    let mut response = Some(response);
    stream_artifact(directory, expected, &cancel, &budget, |_| {
        std::future::ready(response.take().ok_or(DownloadError::Transport))
    })
    .await
}

fn raw_response(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut raw = format!("HTTP/1.1 {status}\r\nConnection: close\r\n").into_bytes();
    for (name, value) in headers {
        raw.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    raw.extend_from_slice(b"\r\n");
    raw.extend_from_slice(body);
    raw
}

async fn scripted_stream(
    responses: Vec<Vec<u8>>,
    expected: u64,
    cancel: &Cancellation,
) -> (Result<File>, Vec<String>, TempDir) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/fixture", listener.local_addr().unwrap());
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = requests.clone();
    let server = tokio::spawn(async move {
        for raw in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                if socket.read(&mut byte).await.unwrap() == 0 {
                    break;
                }
                request.push(byte[0]);
                assert!(request.len() <= 8192);
            }
            seen.lock()
                .unwrap()
                .push(String::from_utf8(request).unwrap());
            socket.write_all(&raw).await.unwrap();
            socket.flush().await.unwrap();
            // Deliver the valid partial bytes before intentionally closing an
            // incomplete Content-Length body in transport-interruption tests.
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    });
    let http = client_builder(false).no_proxy().build().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let budget = RequestBudget::default();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        stream_artifact(&directory, expected, cancel, &budget, |offset| {
            let mut request = http.get(&url).header(header::ACCEPT_ENCODING, "identity");
            if offset > 0 {
                request = request.header(header::RANGE, format!("bytes={offset}-"));
            }
            async move { request.send().await.map_err(artifact_request_error) }
        }),
    )
    .await
    .expect("scripted artifact download must terminate");
    server.abort();
    let requests = requests.lock().unwrap().clone();
    (result, requests, directory)
}

fn has_range(request: &str, offset: u64) -> bool {
    request
        .to_ascii_lowercase()
        .contains(&format!("\r\nrange: bytes={offset}-\r\n"))
}

fn assert_empty(directory: &TempDir) {
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn configured_proxy_routes_https_without_allowing_http_targets() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    let proxy = tokio::spawn(async move {
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut buffer = [0; 4096];
        let length = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        socket
            .write_all(
                b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        String::from_utf8_lossy(&buffer[..length]).into_owned()
    });
    let http = client(true, Some(&proxy_url)).unwrap();
    assert!(http
        .get("http://persistent.oaistatic.com/fixture")
        .send()
        .await
        .is_err());
    assert!(http
        .get("https://persistent.oaistatic.com/fixture")
        .send()
        .await
        .is_err());
    let request = proxy.await.unwrap();
    assert!(request.starts_with("CONNECT persistent.oaistatic.com:443 HTTP/1.1\r\n"));
}

#[test]
fn invalid_configured_proxy_is_rejected_without_direct_fallback() {
    assert!(matches!(
        client(true, Some("://invalid")),
        Err(DownloadError::Transport)
    ));
}
#[tokio::test]
async fn bounded_stream_and_pinned_signature_failure() {
    let response = fixture(
        b"HTTP/1.1 200 OK
Content-Length: 1
Connection: close

x"
        .to_vec(),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let file = single_stream(response, &dir, 1).await.unwrap();
    assert_eq!(file.metadata().unwrap().len(), 1);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    let xml = include_str!("fixtures/release-appcast.xml").replace("406581087", "1");
    let plan = release::parse_release(
        Source::Official,
        Architecture::Arm64,
        release::appcast_url(Source::Official, Architecture::Arm64),
        &xml,
        0,
    )
    .unwrap();
    assert_eq!(verify_file(&file, &plan), Err(DownloadError::Signature));
}
#[tokio::test]
async fn rejects_encoding_redirect_and_wrong_size() {
    for (raw, error) in [
        (
            b"HTTP/1.1 200 OK
Content-Encoding: gzip
Content-Length: 1
Connection: close

x"
            .to_vec(),
            DownloadError::Encoding,
        ),
        (
            b"HTTP/1.1 302 Found
Location: https://evil.test/a.zip
Content-Length: 0
Connection: close

"
            .to_vec(),
            DownloadError::Http,
        ),
        (
            b"HTTP/1.1 200 OK
Content-Length: 2
Connection: close

xx"
            .to_vec(),
            DownloadError::Size,
        ),
    ] {
        let response = fixture(raw).await;
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(single_stream(response,&dir,1).await, Err(e) if e == error));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}

#[tokio::test]
async fn interrupted_artifact_resumes_only_written_bytes_and_still_requires_signature() {
    let (result, requests, directory) = scripted_stream(
        vec![
            raw_response("200 OK", &[("Content-Length", "10")], b"abc"),
            raw_response(
                "206 Partial Content",
                &[("Content-Range", "bytes 3-9/10"), ("Content-Length", "7")],
                b"defghij",
            ),
        ],
        10,
        &Cancellation::default(),
    )
    .await;
    let mut file = result.unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"abcdefghij");
    assert_eq!(requests.len(), 2);
    assert!(!requests[0].to_ascii_lowercase().contains("\r\nrange:"));
    assert!(has_range(&requests[1], 3));
    assert_empty(&directory);
    let xml = include_str!("fixtures/release-appcast.xml").replace("406581087", "10");
    let plan = release::parse_release(
        Source::Official,
        Architecture::Arm64,
        release::appcast_url(Source::Official, Architecture::Arm64),
        &xml,
        0,
    )
    .unwrap();
    assert_eq!(verify_file(&file, &plan), Err(DownloadError::Signature));
}

#[tokio::test]
async fn two_interruptions_resume_cumulative_offsets_before_successful_completion() {
    let (result, requests, directory) = scripted_stream(
        vec![
            raw_response("200 OK", &[("Content-Length", "10")], b"abc"),
            raw_response(
                "206 Partial Content",
                &[("Content-Range", "bytes 3-9/10"), ("Content-Length", "7")],
                b"def",
            ),
            raw_response(
                "206 Partial Content",
                &[("Content-Range", "bytes 6-9/10"), ("Content-Length", "4")],
                b"ghij",
            ),
        ],
        10,
        &Cancellation::default(),
    )
    .await;
    let mut file = result.unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"abcdefghij");
    assert_eq!(requests.len(), 3);
    assert!(!requests[0].to_ascii_lowercase().contains("\r\nrange:"));
    assert!(has_range(&requests[1], 3));
    assert!(has_range(&requests[2], 6));
    assert_empty(&directory);
    let xml = include_str!("fixtures/release-appcast.xml").replace("406581087", "10");
    let plan = release::parse_release(
        Source::Official,
        Architecture::Arm64,
        release::appcast_url(Source::Official, Architecture::Arm64),
        &xml,
        0,
    )
    .unwrap();
    assert_eq!(verify_file(&file, &plan), Err(DownloadError::Signature));
}

#[tokio::test]
async fn ignored_range_replaces_partial_body_instead_of_appending() {
    let (result, requests, directory) = scripted_stream(
        vec![
            raw_response("200 OK", &[("Content-Length", "10")], b"old"),
            raw_response("200 OK", &[("Content-Length", "10")], b"0123456789"),
        ],
        10,
        &Cancellation::default(),
    )
    .await;
    let mut bytes = Vec::new();
    result.unwrap().read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"0123456789");
    assert_eq!(requests.len(), 2);
    assert!(has_range(&requests[1], 3));
    assert_empty(&directory);
}

#[tokio::test]
async fn resumed_artifact_requires_one_exact_complete_content_range() {
    let cases = [
        vec![("Content-Length", "7")],
        vec![("Content-Range", "bytes 2-9/10"), ("Content-Length", "7")],
        vec![("Content-Range", "bytes 3-8/10"), ("Content-Length", "7")],
        vec![("Content-Range", "bytes 3-9/11"), ("Content-Length", "7")],
        vec![("Content-Range", "bytes 3-9/*"), ("Content-Length", "7")],
        vec![
            ("Content-Range", "bytes 3-18446744073709551615/10"),
            ("Content-Length", "7"),
        ],
        vec![
            ("Content-Range", "bytes 3-9/10, bytes 3-9/10"),
            ("Content-Length", "7"),
        ],
        vec![
            ("Content-Range", "bytes 3-9/10"),
            ("Content-Range", "bytes 3-9/10"),
            ("Content-Length", "7"),
        ],
        vec![("Content-Range", "bytes 3-9/10"), ("Content-Length", "6")],
        vec![("Content-Range", "bytes 3-9/10")],
        vec![("Content-Range", "bytes 3-5/10"), ("Content-Length", "3")],
    ];
    for headers in cases {
        let (result, requests, directory) = scripted_stream(
            vec![
                raw_response("200 OK", &[("Content-Length", "10")], b"abc"),
                raw_response("206 Partial Content", &headers, b"defghij"),
            ],
            10,
            &Cancellation::default(),
        )
        .await;
        assert!(matches!(result, Err(DownloadError::Size)), "{headers:?}");
        assert_eq!(requests.len(), 2, "{headers:?}");
        assert!(has_range(&requests[1], 3));
        assert_empty(&directory);
    }
}

#[tokio::test]
async fn resumed_http_encoding_and_invalid_full_headers_are_terminal() {
    for raw in [
        raw_response("416 Range Not Satisfiable", &[("Content-Length", "0")], b""),
        raw_response("503 Service Unavailable", &[("Content-Length", "0")], b""),
        raw_response(
            "206 Partial Content",
            &[
                ("Content-Range", "bytes 3-9/10"),
                ("Content-Length", "7"),
                ("Content-Encoding", "gzip"),
            ],
            b"defghij",
        ),
        raw_response(
            "206 Partial Content",
            &[
                ("Content-Range", "bytes 3-9/10"),
                ("Content-Length", "7, 7"),
            ],
            b"defghij",
        ),
        raw_response("200 OK", &[("Content-Length", "7")], b"defghij"),
    ] {
        let (result, requests, directory) = scripted_stream(
            vec![
                raw_response("200 OK", &[("Content-Length", "10")], b"abc"),
                raw,
            ],
            10,
            &Cancellation::default(),
        )
        .await;
        assert!(matches!(
            result,
            Err(DownloadError::Http | DownloadError::Encoding | DownloadError::Size)
        ));
        assert_eq!(requests.len(), 2);
        assert_empty(&directory);
    }
}

#[tokio::test]
async fn range_validator_rejects_duplicate_length_encoding_and_transfer_encoding() {
    for (name, value, error) in [
        (header::CONTENT_LENGTH, "7", DownloadError::Size),
        (
            header::CONTENT_ENCODING,
            "identity",
            DownloadError::Encoding,
        ),
        (
            header::TRANSFER_ENCODING,
            "chunked",
            DownloadError::Encoding,
        ),
    ] {
        let mut response = fixture(raw_response(
            "206 Partial Content",
            &[
                ("Content-Range", "bytes 3-9/10"),
                ("Content-Length", "7"),
                ("Content-Encoding", "identity"),
            ],
            b"defghij",
        ))
        .await;
        response
            .headers_mut()
            .append(name, header::HeaderValue::from_static(value));
        assert_eq!(artifact_offset(&response, 3, 10), Err(error));
    }
}

#[tokio::test]
async fn clean_short_artifact_eof_is_terminal_size_failure() {
    let (result, requests, directory) = scripted_stream(
        vec![raw_response("200 OK", &[], b"abc")],
        10,
        &Cancellation::default(),
    )
    .await;
    assert!(matches!(result, Err(DownloadError::Size)));
    assert_eq!(requests.len(), 1);
    assert_empty(&directory);
}

#[tokio::test]
async fn framing_failure_after_expected_bytes_restarts_without_empty_range() {
    let (result, requests, directory) = scripted_stream(
        vec![
            raw_response(
                "200 OK",
                &[("Transfer-Encoding", "chunked")],
                b"a\r\nabcdefghij\r\n",
            ),
            raw_response("200 OK", &[("Content-Length", "10")], b"0123456789"),
        ],
        10,
        &Cancellation::default(),
    )
    .await;
    let mut bytes = Vec::new();
    result.unwrap().read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"0123456789");
    assert_eq!(requests.len(), 2);
    assert!(!requests[1].to_ascii_lowercase().contains("\r\nrange:"));
    assert_empty(&directory);
}

#[tokio::test]
async fn repeated_interruptions_exhaust_exactly_six_artifact_gets() {
    let mut responses = vec![raw_response("200 OK", &[("Content-Length", "10")], b"a")];
    for offset in 1..6 {
        responses.push(raw_response(
            "206 Partial Content",
            &[
                ("Content-Range", &format!("bytes {offset}-9/10")),
                ("Content-Length", &(10 - offset).to_string()),
            ],
            b"a",
        ));
    }
    let (result, requests, directory) =
        scripted_stream(responses, 10, &Cancellation::default()).await;
    assert!(matches!(result, Err(DownloadError::Transport)));
    assert_eq!(requests.len(), MAX_ARTIFACT_REQUESTS);
    for (offset, request) in requests.iter().enumerate().skip(1) {
        assert!(has_range(request, offset as u64));
    }
    assert_empty(&directory);
}

#[tokio::test]
async fn cancellation_during_retry_delay_prevents_another_request() {
    let directory = tempfile::tempdir().unwrap();
    let budget = RequestBudget::default();
    let cancel = Cancellation::default();
    let count = AtomicUsize::new(0);
    let (started, observed) = tokio::sync::oneshot::channel();
    let mut started = Some(started);
    let download = stream_artifact(&directory, 10, &cancel, &budget, |_| {
        count.fetch_add(1, Ordering::SeqCst);
        started.take().unwrap().send(()).unwrap();
        std::future::ready(Err(DownloadError::Transport))
    });
    let cancellation = async {
        observed.await.unwrap();
        // The response is ready; yield lets stream_artifact enter its pending
        // retry sleep before this task delivers sticky cancellation.
        tokio::task::yield_now().await;
        cancel.cancel();
    };
    let (result, ()) = tokio::join!(download, cancellation);
    assert!(matches!(result, Err(DownloadError::Cancelled)));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_empty(&directory);
}

#[tokio::test]
async fn already_cancelled_stream_never_requests_or_creates_an_artifact() {
    let directory = tempfile::tempdir().unwrap();
    let cancel = Cancellation::default();
    cancel.cancel();
    let count = AtomicUsize::new(0);
    let result = stream_artifact(&directory, 10, &cancel, &RequestBudget::default(), |_| {
        count.fetch_add(1, Ordering::SeqCst);
        std::future::pending::<Result<Response>>()
    })
    .await;
    assert!(matches!(result, Err(DownloadError::Cancelled)));
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_empty(&directory);
}

#[test]
fn redirects_and_retries_share_one_six_get_budget() {
    let budget = RequestBudget::default();
    for _ in 0..MAX_ARTIFACT_REQUESTS {
        assert!(budget.remaining());
        budget.charge().unwrap();
    }
    assert!(!budget.remaining());
    assert_eq!(budget.charge(), Err(DownloadError::Transport));
    assert_eq!(budget.0.load(Ordering::SeqCst), MAX_ARTIFACT_REQUESTS);
}

#[tokio::test]
async fn cancellation_is_sticky_and_cached_feed_rechecks_current_build() {
    let transport = ReleaseTransport::new().unwrap();
    let cancel = Cancellation::default();
    cancel.cancel();
    assert!(matches!(
        transport
            .fetch_release(Source::Official, Architecture::Arm64, 0, &cancel)
            .await,
        Err(DownloadError::Cancelled)
    ));
    let cancel = Cancellation::default();
    transport.feeds.lock().await.entries.push(Cached {
        source: Source::Official,
        resolved_source: Source::Official,
        arch: Architecture::Arm64,
        xml: include_str!("fixtures/release-appcast.xml").into(),
        at: Instant::now(),
    });
    let p = transport
        .fetch_release(Source::Official, Architecture::Arm64, 9999, &cancel)
        .await
        .unwrap();
    assert!(!p.metadata().update_available);
    transport.feeds.lock().await.last = Some(Instant::now());
    assert!(matches!(
        transport
            .fetch_release(Source::OsirMirror, Architecture::Arm64, 0, &cancel)
            .await,
        Err(DownloadError::RateLimited)
    ));
}

#[test]
fn automatic_source_selects_newer_reachable_feed_and_prefers_mirror_on_ties() {
    let feed = |source, build: u64| {
        let xml = include_str!("fixtures/release-appcast.xml").replace("3575", &build.to_string());
        let plan = release::parse_release(
            source,
            Architecture::Arm64,
            release::appcast_url(source, Architecture::Arm64),
            &xml,
            0,
        )
        .unwrap();
        Ok((xml, plan))
    };
    for (mirror, official, expected, build) in [
        (Some(3575), Some(3576), Source::Official, 3576),
        (Some(3576), Some(3575), Source::OsirMirror, 3576),
        (Some(3575), Some(3575), Source::OsirMirror, 3575),
        (None, Some(3575), Source::Official, 3575),
        (Some(3575), None, Source::OsirMirror, 3575),
    ] {
        let mirror = mirror
            .map(|build| feed(Source::OsirMirror, build))
            .unwrap_or(Err(DownloadError::Http));
        let official = official
            .map(|build| feed(Source::Official, build))
            .unwrap_or(Err(DownloadError::Timeout));
        let (source, _, plan) = choose_automatic(mirror, official).unwrap();
        assert_eq!(source, expected);
        assert_eq!(plan.source(), expected);
        assert_eq!(plan.metadata().build, build);
    }
    assert!(choose_automatic(Err(DownloadError::Http), Err(DownloadError::Timeout)).is_err());
}

#[tokio::test]
async fn automatic_cache_keeps_resolved_source_and_recompares_selected_target() {
    let transport = ReleaseTransport::new().unwrap();
    transport.feeds.lock().await.entries.push(Cached {
        source: Source::Auto,
        resolved_source: Source::Official,
        arch: Architecture::Arm64,
        xml: include_str!("fixtures/release-appcast.xml").into(),
        at: Instant::now(),
    });
    let first = transport
        .fetch_release(
            Source::Auto,
            Architecture::Arm64,
            100,
            &Cancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!(first.source(), Source::Official);
    assert!(first.metadata().update_available);
    let current = transport
        .fetch_release(
            Source::Auto,
            Architecture::Arm64,
            3575,
            &Cancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!(current.source(), Source::Official);
    assert!(!current.metadata().update_available);
}
