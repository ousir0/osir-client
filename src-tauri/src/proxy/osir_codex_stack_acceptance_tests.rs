//! Explicit, loopback-only acceptance through an installed Codex app-server.
//! This uses real provider/configuration writers and the Rust proxy; every key,
//! prompt, response, model and native HOME is synthetic. It does not exercise
//! production OSIR entitlement, keychain, billing or a paid upstream model.
use crate::{
    app_config::AppType,
    database::Database,
    mode::{controller, state as mode_state},
    provider::{Provider, ProviderMeta},
    proxy::ProxyConfig,
    services::ProviderService,
    store::AppState,
};
use axum::{
    body::Body,
    extract::Request,
    http::{header, StatusCode},
    response::Response,
    routing::post,
    Router,
};
use serde_json::{json, Value};
use serial_test::serial;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout},
    sync::Mutex,
};

const MODEL: &str = "same-synthetic-model";
const BIN_ENV: &str = "CCS_OSIR_STACK_ACCEPTANCE_CODEX_BIN";

struct IsolatedHome {
    dir: TempDir,
    saved: Vec<(&'static str, Option<OsString>)>,
}
impl IsolatedHome {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("osir-stack-acceptance-")
            .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
        let saved = ["HOME", "USERPROFILE", "CC_SWITCH_TEST_HOME", "CODEX_HOME"]
            .into_iter()
            .map(|name| {
                let old = std::env::var_os(name);
                std::env::set_var(
                    name,
                    if name == "CODEX_HOME" {
                        dir.path().join(".codex")
                    } else {
                        dir.path().to_owned()
                    },
                );
                (name, old)
            })
            .collect();
        // Assert native path ownership before reading or writing any config.
        assert!(crate::config::get_home_dir().starts_with(dir.path()));
        assert!(crate::config::get_app_config_dir().starts_with(dir.path()));
        crate::settings::reload_settings().unwrap();
        assert!(crate::codex_config::get_codex_config_dir().starts_with(dir.path()));
        Self { dir, saved }
    }
}
impl Drop for IsolatedHome {
    fn drop(&mut self) {
        for (name, old) in self.saved.drain(..) {
            if let Some(old) = old {
                std::env::set_var(name, old);
            } else {
                std::env::remove_var(name);
            }
        }
        let _ = crate::settings::reload_settings();
    }
}

#[derive(Clone)]
struct RoutedRequest {
    upstream: &'static str,
    correct_synthetic_key: bool,
    upstream_model: String,
}

async fn upstream(
    label: &'static str,
    synthetic_key: &'static str,
    captured: Arc<Mutex<Vec<RoutedRequest>>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let router = Router::new().route("/v1/responses", post(move |request: Request| {
        let captured = captured.clone();
        async move {
            let (parts, body) = request.into_parts();
            let body = axum::body::to_bytes(body, 2 * 1024 * 1024).await.unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            let correct = parts.headers.get(header::AUTHORIZATION)
                .is_some_and(|value| value == format!("Bearer {synthetic_key}").as_str());
            // Capture only routing metadata. Do not retain prompts or key values.
            captured.lock().await.push(RoutedRequest { upstream: label, correct_synthetic_key: correct, upstream_model: body["model"].as_str().unwrap_or_default().into() });
            if !correct || body["model"] != MODEL {
                return Response::builder().status(StatusCode::BAD_REQUEST).body(Body::from("synthetic route rejected")).unwrap();
            }
            let marker = format!("OSIR_ACCEPTANCE_{label}");
            let message = json!({"id":format!("msg_{label}"),"type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":marker,"annotations":[]}]});
            let response = json!({"id":format!("resp_{label}"),"object":"response","created_at":1,"status":"completed","model":MODEL,"output":[message],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}});
            let events = [
                json!({"type":"response.created","response":{"id":format!("resp_{label}"),"object":"response","created_at":1,"status":"in_progress","model":MODEL,"output":[]}}),
                json!({"type":"response.output_item.added","output_index":0,"item":{"id":format!("msg_{label}"),"type":"message","status":"in_progress","role":"assistant","content":[]}}),
                json!({"type":"response.content_part.added","item_id":format!("msg_{label}"),"output_index":0,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}),
                json!({"type":"response.output_text.delta","item_id":format!("msg_{label}"),"output_index":0,"content_index":0,"delta":marker}),
                json!({"type":"response.output_text.done","item_id":format!("msg_{label}"),"output_index":0,"content_index":0,"text":marker}),
                json!({"type":"response.content_part.done","item_id":format!("msg_{label}"),"output_index":0,"content_index":0,"part":{"type":"output_text","text":marker,"annotations":[]}}),
                json!({"type":"response.output_item.done","output_index":0,"item":message}),
                json!({"type":"response.completed","response":response}),
            ];
            let mut sse = String::new();
            for (sequence, mut event) in events.into_iter().enumerate() {
                event["sequence_number"] = json!(sequence);
                sse.push_str(&format!("event: {}\ndata: {}\n\n", event["type"].as_str().unwrap(), event));
            }
            Response::builder().status(StatusCode::OK).header(header::CONTENT_TYPE, "text/event-stream").body(Body::from(sse)).unwrap()
        }
    }));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (url, task)
}

fn provider(id: &str, url: &str, key: &str) -> Provider {
    let mut provider = Provider::with_id(
        id.into(),
        "Same Provider".into(),
        json!({
            "auth":{"OPENAI_API_KEY":key},
            "config":format!("model_provider = \"fixture\"\nmodel = \"{MODEL}\"\n[model_providers.fixture]\nname = \"fixture\"\nbase_url = \"{url}\"\nwire_api = \"responses\"\n"),
            "modelCatalog":{"models":[{"model":MODEL,"displayName":"Same Model"}]}
        }),
        None,
    );
    provider.meta = Some(ProviderMeta {
        api_format: Some("openai_responses".into()),
        ..Default::default()
    });
    provider
}

struct Rpc {
    child: Child,
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
    notifications: Vec<Value>,
    next_id: u64,
}
impl Rpc {
    fn start(binary: &Path, home: &Path, cwd: &Path) -> Self {
        let mut child = tokio::process::Command::new(binary)
            .args([
                "-c",
                "analytics.enabled=false",
                "-c",
                "feedback.enabled=false",
                "app-server",
            ])
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", home)
            .env("USERPROFILE", home)
            .env("CODEX_HOME", home.join(".codex"))
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("USER", "osir-fixture")
            .env("HTTP_PROXY", "http://127.0.0.1:9")
            .env("HTTPS_PROXY", "http://127.0.0.1:9")
            .env("ALL_PROXY", "http://127.0.0.1:9")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn explicit test-owned Codex app-server");
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap()).lines();
        Self {
            child,
            input,
            output,
            notifications: Vec::new(),
            next_id: 1,
        }
    }
    async fn send(&mut self, value: Value) {
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        self.input
            .write_all(&bytes)
            .await
            .expect("write synthetic RPC");
        self.input.flush().await.unwrap();
    }
    async fn read(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(20), self.output.next_line())
            .await
            .expect("Codex app-server RPC timed out")
            .expect("read test-owned app-server RPC")
            .expect("Codex app-server closed before completion");
        serde_json::from_str(&line).expect("Codex app-server emitted invalid JSON-RPC")
    }
    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"id":id,"method":method,"params":params}))
            .await;
        for _ in 0..1000 {
            let message = self.read().await;
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                assert!(
                    message.get("error").is_none(),
                    "{method} failed; JSON-RPC code={:?}",
                    message.pointer("/error/code")
                );
                return message["result"].clone();
            }
            if message.get("id").is_some() && message.get("method").is_some() {
                // Never approve tools or commands in this routing acceptance.
                self.send(json!({"id":message["id"],"error":{"code":-32601,"message":"Synthetic acceptance does not permit tools"}})).await;
            } else {
                self.notifications.push(message);
            }
        }
        panic!("too many unrelated synthetic app-server events");
    }
    async fn turn(&mut self, model: &str, cwd: &Path, marker: &str) {
        let thread = self.request("thread/start", json!({"model":model,"cwd":cwd,"approvalPolicy":"never","sandbox":"read-only","experimentalRawEvents":false})).await;
        let thread_id = thread
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .expect("thread/start must return a thread id")
            .to_owned();
        let turn = self.request("turn/start", json!({"threadId":thread_id,"model":model,"input":[{"type":"text","text":"Return the synthetic acceptance marker only.","text_elements":[]}]})).await;
        let turn_id = turn
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .expect("turn/start must return a turn id")
            .to_owned();
        let mut marker_seen = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        for _ in 0..1000 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "synthetic turn did not complete"
            );
            let message = if self.notifications.is_empty() {
                self.read().await
            } else {
                self.notifications.remove(0)
            };
            if message
                .pointer("/params/threadId")
                .and_then(Value::as_str)
                .is_some_and(|id| id != thread_id)
            {
                continue;
            }
            marker_seen |= message.to_string().contains(marker);
            if message.get("method").and_then(Value::as_str) == Some("turn/completed")
                && message.pointer("/params/turn/id").and_then(Value::as_str)
                    == Some(turn_id.as_str())
            {
                assert_eq!(
                    message
                        .pointer("/params/turn/status")
                        .and_then(Value::as_str),
                    Some("completed"),
                    "synthetic upstream turn failed"
                );
                assert!(
                    marker_seen,
                    "model result did not reach the real Codex app-server"
                );
                return;
            }
        }
        panic!("synthetic turn completion was not observed");
    }
}

/// Run only with an explicitly selected installed binary and test-threads=1.
/// All configured upstream origins are loopback; no existing auth/config is copied.
#[tokio::test(flavor = "current_thread")]
#[serial]
#[ignore = "explicit installed Codex binary required; loopback-only synthetic acceptance"]
async fn installed_codex_app_server_lists_and_routes_duplicate_models_through_real_stack() {
    let binary = std::env::var_os(BIN_ENV)
        .map(PathBuf::from)
        .expect("set CCS_OSIR_STACK_ACCEPTANCE_CODEX_BIN to the approved installed Codex binary");
    assert!(
        binary.is_absolute() && binary.is_file(),
        "explicit installed binary required"
    );
    let home = IsolatedHome::new();
    let project = home.dir.path().join("项目 O'Brien fixture");
    std::fs::create_dir(&project).unwrap();
    let native = crate::codex_config::get_codex_config_dir();
    std::fs::create_dir_all(&native).unwrap();
    let user_fields = "# synthetic user-owned field\nproject_doc_fallback_filenames = [\"FIXTURE.md\"]\n[mcp_servers.keep]\ncommand = \"fixture-must-not-run\"\nenabled = false\n";
    std::fs::write(native.join("config.toml"), user_fields).unwrap();
    std::fs::write(native.join("auth.json"), "{}").unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let (url_a, upstream_a) = upstream("KEY_A", "synthetic-key-a", captured.clone()).await;
    let (url_b, upstream_b) = upstream("KEY_B", "synthetic-key-b", captured.clone()).await;
    let db = Arc::new(Database::memory().unwrap());
    for row in [
        provider("key-a", &url_a, "synthetic-key-a"),
        provider("key-b", &url_b, "synthetic-key-b"),
    ] {
        db.save_provider("codex", &row).unwrap();
    }
    db.update_proxy_config(ProxyConfig {
        listen_port: 0,
        enable_logging: false,
        max_retries: 0,
        ..Default::default()
    })
    .await
    .unwrap();
    let state = AppState::new(db);
    ProviderService::switch(&state, AppType::Codex, "key-a")
        .expect("real provider service direct writer");
    controller::enter_with_route(&state, &AppType::Codex, true, Some("key-a"))
        .await
        .expect("real proxy stack writer");
    controller::set_stack_member(&state, &AppType::Codex, "key-b", true)
        .await
        .expect("real second stack member writer");
    let live =
        mode_state::mode_state(&crate::live::engine::DeviceStore::for_device(), "codex").unwrap();
    assert!(live.is_proxy() && live.attached);
    let stack =
        mode_state::stack(&crate::live::engine::DeviceStore::for_device(), "codex").unwrap();
    assert!(stack.enabled);
    let second = format!("ccs-{}/{MODEL}", stack.key_of("key-b").unwrap());
    assert_ne!(
        stack.key_of("key-a"),
        stack.key_of("key-b"),
        "same provider names must not collide"
    );
    let projected = std::fs::read_to_string(native.join("config.toml")).unwrap();
    assert!(
        projected.contains("project_doc_fallback_filenames")
            && projected.contains("mcp_servers.keep")
    );
    let mut rpc = Rpc::start(&binary, home.dir.path(), &project);
    rpc.request("initialize", json!({"clientInfo":{"name":"osir-stack-acceptance","version":"1.0.0"},"capabilities":{"experimentalApi":true}})).await;
    rpc.send(json!({"method":"initialized","params":{}})).await;
    let listed = rpc
        .request("model/list", json!({"limit":100,"includeHidden":true}))
        .await;
    let names = listed["data"]
        .as_array()
        .expect("real model/list must expose catalog rows")
        .iter()
        .filter_map(|row| row.get("model").or_else(|| row.get("id")))
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(
        names.contains(&MODEL) && names.contains(&second.as_str()),
        "real app-server did not load both duplicate-model catalog entries"
    );
    rpc.turn(MODEL, &project, "OSIR_ACCEPTANCE_KEY_A").await;
    rpc.turn(&second, &project, "OSIR_ACCEPTANCE_KEY_B").await;
    let rows = captured.lock().await;
    assert!(
        rows.iter().any(|row| row.upstream == "KEY_A")
            && rows.iter().any(|row| row.upstream == "KEY_B")
    );
    assert!(
        rows.iter()
            .all(|row| row.correct_synthetic_key && row.upstream_model == MODEL),
        "stack prefix stripping or key isolation failed"
    );
    assert_eq!(
        rows.len(),
        2,
        "a model selection must not retry/fail over to another key"
    );
    drop(rows);
    rpc.child
        .kill()
        .await
        .expect("stop only this test-owned app-server");
    state.proxy_service.stop().await.unwrap();
    upstream_b.abort();
    upstream_a.abort();
    println!("OSIR_STACK_ACCEPTANCE provider_writer=real proxy=real model_list=real turns=2 same_model_keys_isolated=true upstream_origins=loopback");
}
