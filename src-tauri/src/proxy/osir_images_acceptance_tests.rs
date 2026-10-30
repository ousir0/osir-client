//! Loopback-only Images admission acceptance. All keys and image bytes are synthetic.
//! This suite proves standard Images routing/failure boundaries, not Codex's
//! Responses image_generation tool, production permissions, keychain or billing.
use super::server::ProxyServer;
use super::ProxyConfig;
use crate::{database::Database, provider::Provider};
use axum::{
    extract::Request,
    http::{header, StatusCode},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::Mutex;

const CUSTOM_SETTING: &str = "osir.codex.image-custom.v1";
const SAVED_SETTING: &str = "osir.codex.image-selection.v1";
// A complete 1x1 transparent PNG. Never fetch a remote image or use user data.
const PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR4nGNgAAIAAAUAAXpeqz8AAAAASUVORK5CYII=";
const ALIASES: [&str; 8] = [
    "/images/generations",
    "/v1/images/generations",
    "/v1/v1/images/generations",
    "/codex/v1/images/generations",
    "/images/edits",
    "/v1/images/edits",
    "/v1/v1/images/edits",
    "/codex/v1/images/edits",
];
#[derive(Clone)]
struct Capture {
    path: String,
    auth_matches: bool,
    body: Value,
}
struct Harness {
    db: Arc<Database>,
    proxy: ProxyServer,
    port: u16,
    upstream: tokio::task::JoinHandle<()>,
    images: Arc<AtomicUsize>,
    responses: Arc<AtomicUsize>,
    captured: Arc<Mutex<Vec<Capture>>>,
}
impl Harness {
    async fn new() -> Self {
        let images = Arc::new(AtomicUsize::new(0));
        let responses = Arc::new(AtomicUsize::new(0));
        let captured = Arc::new(Mutex::new(Vec::new()));
        let image = {
            let count = images.clone();
            let captured = captured.clone();
            move |request: Request| {
                let count = count.clone();
                let captured = captured.clone();
                async move {
                    let (parts, body) = request.into_parts();
                    let body: Value =
                        serde_json::from_slice(&axum::body::to_bytes(body, 65536).await.unwrap())
                            .unwrap();
                    count.fetch_add(1, Ordering::SeqCst);
                    captured.lock().await.push(Capture {
                        path: parts.uri.path().into(),
                        auth_matches: parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .is_some_and(|value| value == "Bearer synthetic-chat-key"),
                        body: body.clone(),
                    });
                    if body.get("prompt").and_then(Value::as_str) == Some("fixture-failure") {
                        (
                            StatusCode::BAD_REQUEST,
                            Json(
                                json!({"error":{"type":"invalid_request_error","message":"Synthetic image failure","code":"fixture_image_rejected"}}),
                            ),
                        )
                    } else {
                        (
                            StatusCode::OK,
                            Json(
                                json!({"created":1,"data":[{"b64_json":PNG_BASE64}],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}),
                            ),
                        )
                    }
                }
            }
        };
        let response = {
            let count = responses.clone();
            let captured = captured.clone();
            move |request: Request| {
                let count = count.clone();
                let captured = captured.clone();
                async move {
                    let (parts, body) = request.into_parts();
                    let body: Value =
                        serde_json::from_slice(&axum::body::to_bytes(body, 65536).await.unwrap())
                            .unwrap();
                    count.fetch_add(1, Ordering::SeqCst);
                    captured.lock().await.push(Capture {
                        path: parts.uri.path().into(),
                        auth_matches: parts
                            .headers
                            .get(header::AUTHORIZATION)
                            .is_some_and(|value| value == "Bearer synthetic-chat-key"),
                        body,
                    });
                    Json(
                        json!({"id":"resp_fixture","object":"response","created_at":1,"status":"completed","model":"chat-fixture","output":[{"id":"msg_fixture","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"synthetic response","annotations":[]}]}],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}),
                    )
                }
            }
        };
        let router = Router::new()
            .route("/v1/images/generations", post(image.clone()))
            .route("/v1/images/edits", post(image))
            .route("/v1/responses", post(response));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let upstream_addr = listener.local_addr().unwrap();
        let upstream = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let db = Arc::new(Database::memory().unwrap());
        for id in ["chat-a", "chat-b"] {
            db.save_provider("codex",&Provider::with_id(id.into(),id.into(),json!({"base_url":format!("http://{upstream_addr}/v1"),"auth":{"OPENAI_API_KEY":"synthetic-chat-key"}}),None)).unwrap();
        }
        db.set_current_provider("codex", "chat-a").unwrap();
        let proxy = ProxyServer::new(
            ProxyConfig {
                listen_port: 0,
                non_streaming_timeout: 3,
                ..ProxyConfig::default()
            },
            db.clone(),
            None,
        );
        let port = proxy.start().await.unwrap().port;
        Self {
            db,
            proxy,
            port,
            upstream,
            images,
            responses,
            captured,
        }
    }
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
    async fn close(self) {
        self.proxy.stop().await.unwrap();
        self.upstream.abort();
    }
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap()
}

#[tokio::test]
async fn loopback_standard_images_generate_and_edit_preserve_fixture_payload() {
    let h = Harness::new().await;
    let c = client();
    for path in ALIASES {
        let edit = path.ends_with("/edits");
        let mut body = json!({"model":"fixture-image","prompt":"synthetic image fixture"});
        if edit {
            body["images"] = json!([{"image_url":format!("data:image/png;base64,{PNG_BASE64}")}]);
        }
        let response = c
            .post(h.url(path))
            .header(header::AUTHORIZATION, "Bearer synthetic-client-key")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(
            response.json::<Value>().await.unwrap()["data"][0]["b64_json"],
            PNG_BASE64
        );
    }
    assert_eq!(h.images.load(Ordering::SeqCst), 8);
    assert_eq!(h.responses.load(Ordering::SeqCst), 0);
    let rows = h.captured.lock().await;
    assert!(rows
        .iter()
        .all(|r| r.auth_matches && r.body["model"] == "fixture-image"));
    assert_eq!(
        rows.iter()
            .filter(|r| r.path == "/v1/images/generations")
            .count(),
        4
    );
    assert_eq!(
        rows.iter()
            .filter(|r| r.path == "/v1/images/edits"
                && r.body["images"][0]["image_url"]
                    == format!("data:image/png;base64,{PNG_BASE64}"))
            .count(),
        4
    );
    drop(rows);
    h.close().await;
}

#[tokio::test]
async fn loopback_configured_images_fail_closed_after_chat_switch_without_native_context() {
    let h = Harness::new().await;
    let c = client();
    let selection=json!({"endpoint":"https://image-fixture.invalid/v1","model":"independent-image-model","version":"synthetic-version"}).to_string();
    h.db.set_setting(CUSTOM_SETTING, &selection).unwrap();
    for chat in ["chat-a", "chat-b", "chat-a"] {
        h.db.set_current_provider("codex", chat).unwrap();
        for path in ALIASES {
            let response = c
                .post(h.url(path))
                .json(&json!({"model":"ccs-chat-model","prompt":"synthetic"}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{chat} {path}");
            let error = response.text().await.unwrap();
            assert!(error.contains("图片"));
            assert!(!error.contains("synthetic-chat-key"));
        }
        assert_eq!(
            h.db.get_setting(CUSTOM_SETTING).unwrap().as_deref(),
            Some(selection.as_str())
        );
    }
    assert_eq!(h.images.load(Ordering::SeqCst), 0);
    assert_eq!(h.responses.load(Ordering::SeqCst), 0);
    h.close().await;
}

#[tokio::test]
async fn loopback_invalid_image_state_rejects_images_but_responses_tool_stays_separate() {
    let h = Harness::new().await;
    let c = client();
    h.db.set_setting(CUSTOM_SETTING, "{invalid-image-json")
        .unwrap();
    let rejected = c
        .post(h.url("/v1/images/generations"))
        .json(&json!({"prompt":"synthetic"}))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    // This only proves the standard Images admission hook is not applied to
    // Responses. It does NOT prove upstream implements the built-in image tool.
    let response=c.post(h.url("/v1/responses")).json(&json!({"model":"chat-fixture","input":"synthetic","tools":[{"type":"image_generation"}]})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(h.images.load(Ordering::SeqCst), 0);
    assert_eq!(h.responses.load(Ordering::SeqCst), 1);
    assert_eq!(
        h.captured.lock().await[0].body["tools"][0]["type"],
        "image_generation"
    );
    assert_eq!(
        h.db.get_setting(CUSTOM_SETTING).unwrap().as_deref(),
        Some("{invalid-image-json")
    );
    h.close().await;
}

#[tokio::test]
async fn loopback_missing_saved_binding_and_upstream_failure_never_report_image_success() {
    let h = Harness::new().await;
    let c = client();
    h.db.set_setting(SAVED_SETTING, "{}").unwrap();
    let rejected = c
        .post(h.url("/v1/images/edits"))
        .json(&json!({"model":"fixture-image","images":[]}))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    assert_eq!(h.images.load(Ordering::SeqCst), 0);
    h.db.set_setting(SAVED_SETTING, "null").unwrap();
    for path in ["/v1/images/generations", "/v1/images/edits"] {
        let response = c
            .post(h.url(path))
            .json(&json!({"model":"fixture-image","prompt":"fixture-failure"}))
            .send()
            .await
            .unwrap();
        assert!(!response.status().is_success());
        let value: Value = response.json().await.unwrap();
        assert!(value.get("error").is_some());
        assert!(value.get("data").is_none());
    }
    assert_eq!(h.images.load(Ordering::SeqCst), 2);
    assert_eq!(h.responses.load(Ordering::SeqCst), 0);
    h.close().await;
}
