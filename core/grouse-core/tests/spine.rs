//! End-to-end spine tests against an in-process scripted WebSocket server
//! (the Rust twin of the desktop's tests/fakeserver.h).
//!
//! The fake server speaks just enough ACP JSON-RPC: initialize, session/new,
//! session/list and session/prompt handlers, and it streams
//! `agent_message_chunk` notifications before answering a prompt — exactly
//! what the desktop's FakeGooseServer scripts. Every request frame (and the
//! `X-Secret-Key` upgrade header) is recorded so tests can assert the wire
//! shape — the protocol footguns live there.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use async_tungstenite::tungstenite::Message as WsMessage;
use futures::stream::StreamExt;
use grouse_core::{
    ConfigOption, ConnectionStatus, Core, CoreListener, PermissionRequest, ProjectSummary, Prompt,
    PromptBlock, SendExpect, SessionSummary, StreamEvent, TranscriptEvent,
};
use parking_lot::Mutex;
use serde_json::{Value, json};

/// Both cache e2e tests set the process-global XDG_DATA_HOME; they must not
/// run concurrently or each reads the other's cache dir.
static CACHE_TEST_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// Recording listener
// ---------------------------------------------------------------------------

#[allow(dead_code)] // full listener surface; tests match a subset
enum Ev {
    Status(ConnectionStatus),
    Sessions(Vec<SessionSummary>),
    Transcript(TranscriptEvent),
    Stream(StreamEvent),
    Config(Vec<ConfigOption>),
    Permission(PermissionRequest),
    Touched(String, String, String),
    Projects(Vec<ProjectSummary>),
    RoamPeerStatus(String, String),
    RoamSessions(String, Vec<SessionSummary>),
    PeerNewSession(String, String),
    ActiveRun(String, String),
    Commands(Vec<String>),
}

struct RecordingListener {
    tx: mpsc::Sender<Ev>,
}

impl RecordingListener {
    fn new(tx: mpsc::Sender<Ev>) -> Self {
        Self { tx }
    }
}

impl CoreListener for RecordingListener {
    fn on_status(&self, status: ConnectionStatus) {
        let _ = self.tx.send(Ev::Status(status));
    }
    fn on_sessions(&self, sessions: Vec<SessionSummary>) {
        let _ = self.tx.send(Ev::Sessions(sessions));
    }
    fn on_transcript(&self, event: TranscriptEvent) {
        let _ = self.tx.send(Ev::Transcript(event));
    }
    fn on_stream(&self, event: StreamEvent) {
        let _ = self.tx.send(Ev::Stream(event));
    }
    fn on_config(&self, options: Vec<ConfigOption>) {
        let _ = self.tx.send(Ev::Config(options));
    }
    fn on_permission_request(&self, request: PermissionRequest) {
        let _ = self.tx.send(Ev::Permission(request));
    }
    fn on_session_touched(&self, session_id: String, title: String, updated_at: String) {
        let _ = self.tx.send(Ev::Touched(session_id, title, updated_at));
    }
    fn on_projects(&self, projects: Vec<ProjectSummary>) {
        let _ = self.tx.send(Ev::Projects(projects));
    }
    fn on_roam_peer_status(&self, label: String, status: String) {
        let _ = self.tx.send(Ev::RoamPeerStatus(label, status));
    }
    fn on_roam_sessions(&self, label: String, sessions: Vec<SessionSummary>) {
        let _ = self.tx.send(Ev::RoamSessions(label, sessions));
    }
    fn on_peer_new_session(&self, label: String, session_id: String) {
        let _ = self.tx.send(Ev::PeerNewSession(label, session_id));
    }
    fn on_commands(&self, commands: Vec<String>) {
        let _ = self.tx.send(Ev::Commands(commands));
    }
    fn on_active_run(&self, session_id: String, run_id: String) {
        let _ = self.tx.send(Ev::ActiveRun(session_id, run_id));
    }
}

/// Wait for an event matching `pred`, skipping unrelated traffic.
fn wait_for<F: Fn(&Ev) -> bool>(rx: &Receiver<Ev>, pred: F, what: &str) -> Ev {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(ev) => {
                if pred(&ev) {
                    return ev;
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                panic!("listener dropped while waiting for {what}")
            }
        }
    }
    panic!("timed out waiting for {what}");
}

// ---------------------------------------------------------------------------
// Fake goosed server (in-process, scripted per method)
// ---------------------------------------------------------------------------

/// The scripted ACP server. Lives on its own tokio runtime thread; the test
/// drives it only through the port + the recorded frames.
struct FakeServer {
    /// Every JSON-RPC frame the client sent, in arrival order.
    frames: Arc<Mutex<Vec<Value>>>,
    /// The `X-Secret-Key` upgrade header, if the handshake carried one.
    secret_header: Arc<Mutex<Option<String>>>,
    /// When set, the fake pushes `session_info_update` (with an activeRunId)
    /// + `available_commands_update` notifications right after session/new.
    notify: Arc<std::sync::atomic::AtomicBool>,
}

impl FakeServer {
    /// Bind on 127.0.0.1:0 and spawn the accept+serve loop. Returns when the
    /// listener is bound (the port is delivered through `port_tx`).
    fn spawn(port_tx: mpsc::Sender<u16>) -> Arc<Self> {
        let server = Arc::new(Self {
            frames: Arc::new(Mutex::new(Vec::new())),
            secret_header: Arc::new(Mutex::new(None)),
            notify: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("fake server runtime");
        let for_thread = server.clone();
        std::thread::spawn(move || {
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                    .await
                    .expect("bind fake server");
                let port = listener.local_addr().expect("fake server addr").port();
                let _ = port_tx.send(port);
                // Accept repeatedly: open_session / reconnect each bring a new
                // WebSocket; each is served on its own task.
                loop {
                    let (stream, _peer) = listener.accept().await.expect("fake server accept");
                    let server = for_thread.clone();
                    tokio::spawn(async move { serve_connection(server, stream).await });
                }
            });
        });
        server
    }

    fn frames_for(&self, method: &str) -> Vec<Value> {
        self.frames
            .lock()
            .iter()
            .filter(|frame| frame.get("method").and_then(Value::as_str) == Some(method))
            .cloned()
            .collect()
    }
}

#[allow(clippy::result_large_err)] // tungstenite mandates the large ErrorResponse here
async fn serve_connection(server: Arc<FakeServer>, stream: tokio::net::TcpStream) {
    let secret = server.secret_header.clone();
    let ws = async_tungstenite::tokio::accept_hdr_async(
        stream,
        move |req: &Request, res: Response| {
            let header = req
                .headers()
                .get("X-Secret-Key")
                .and_then(|value| value.to_str().ok())
                .map(|value| value.to_string());
            *secret.lock() = header;
            Ok::<_, ErrorResponse>(res)
        },
    )
    .await
    .expect("fake server ws handshake");

    let (mut tx, mut rx) = ws.split();
    while let Some(Ok(message)) = rx.next().await {
        let WsMessage::Text(text) = message else { continue };
        let Ok(frame) = serde_json::from_str::<Value>(text.as_str()) else {
            continue;
        };
        server.frames.lock().push(frame.clone());
        let method = frame
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let id = frame.get("id").cloned();
        let params = frame.get("params").cloned().unwrap_or(Value::Null);

        // Notifications (no id): record and never reply.
        let Some(id) = id else { continue };

        let result = match method.as_str() {
            "initialize" => json!({ "protocolVersion": 1 }),
            "session/new" => {
                if server.notify.load(std::sync::atomic::Ordering::SeqCst) {
                    // session/new params carry no sessionId; the fake's reply
                    // is fixed to sess-e2e, so the notifications use that.
                    let session = "sess-e2e";
                    let run = json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session,
                            "update": {
                                "sessionUpdate": "session_info_update",
                                "title": "E2E Chat",
                                "updated_at": "2026-08-12T00:00:00.000Z",
                                "_meta": { "goose": { "activeRunId": "run-abc" } }
                            }
                        }
                    });
                    let _ = tx.send(WsMessage::Text(run.to_string().into())).await;
                    let commands = json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session,
                            "update": {
                                "sessionUpdate": "available_commands_update",
                                "availableCommands": [
                                    { "name": "/compact", "description": "compact" },
                                    { "name": "/undo", "description": "undo" }
                                ]
                            }
                        }
                    });
                    let _ = tx.send(WsMessage::Text(commands.to_string().into())).await;
                }
                json!({
                    "sessionId": "sess-e2e",
                    "configOptions": [
                        { "id": "provider", "name": "Provider", "currentValue": "openai" }
                    ]
                })
            }
            "session/list" => json!({
                "sessions": [{
                    "sessionId": "sess-e2e",
                    "title": "E2E Chat",
                    "updatedAt": "2026-08-12T00:00:00.000Z",
                    "cwd": "/tmp",
                    "_meta": { "messageCount": 1, "lastMessageSnippet": "hi from the list" }
                }]
            }),
            "session/load" => {
                let session = params
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                for (tag, text, message_id) in [
                    ("agent_message_chunk", "replayed line one", "r-1"),
                    ("agent_message_chunk", " and two", "r-1"),
                ] {
                    let frame = json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session,
                            "update": {
                                "sessionUpdate": tag,
                                "content": { "type": "text", "text": text },
                                "messageId": message_id
                            }
                        }
                    });
                    let _ = tx.send(WsMessage::Text(frame.to_string().into())).await;
                }
                // The replay's own session_info_update carries the CURRENT
                // updatedAt — what the fresh cache must be stamped with.
                let info = json!({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": session,
                        "update": {
                            "sessionUpdate": "session_info_update",
                            "title": "Replayed",
                            // camelCase: the schema's rename_all makes this updatedAt.
                            "updatedAt": "2026-08-12T12:00:00.000Z"
                        }
                    }
                });
                let _ = tx.send(WsMessage::Text(info.to_string().into())).await;
                json!({
                    "sessionId": session,
                    "configOptions": [
                        { "id": "provider", "name": "Provider", "currentValue": "openai" }
                    ]
                })
            }
            "session/prompt" => {
                // Stream the turn BEFORE the reply: user echo + two chunks of
                // one agent message (same messageId -> one bubble), then end.
                let session = params
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let prompt_text = params
                    .pointer("/prompt/0/text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                for (tag, text, message_id) in [
                    ("user_message_chunk", prompt_text.as_str(), "u-1"),
                    ("agent_message_chunk", "hello from fake goose", "m-1"),
                    ("agent_message_chunk", " and more", "m-1"),
                ] {
                    let frame = json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session,
                            "update": {
                                "sessionUpdate": tag,
                                "content": { "type": "text", "text": text },
                                "messageId": message_id
                            }
                        }
                    });
                    let _ = tx.send(WsMessage::Text(frame.to_string().into())).await;
                }
                json!({ "stopReason": "end_turn" })
            }
            "_goose/unstable/session/info" => json!({
                "session": {
                    "updatedAt": "2026-08-12T13:00:00.000Z",
                    "_meta": { "messageCount": 3 }
                }
            }),
            _ => json!({}),
        };
        let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        let _ = tx.send(WsMessage::Text(reply.to_string().into())).await;
    }
}

// ---------------------------------------------------------------------------
// Cache: a resume replay must persist the fresh transcript + updatedAt so the
// next open renders from cache instead of re-streaming the same delta
// ---------------------------------------------------------------------------

#[test]
fn resume_replay_persists_the_fresh_cache() {
    let _cache_guard = CACHE_TEST_LOCK.lock();
    let data_dir =
        std::env::temp_dir().join(format!("grouse-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    std::env::set_var("XDG_DATA_HOME", &data_dir);
    // Seed a STALE cache: old content, old updatedAt.
    let cache = grouse_core::cache::CacheStore::new(data_dir.join("grouse"));
    let stale = vec![grouse_core::Message {
        id: "m-old".into(),
        role: "user".into(),
        content: "stale cached line".into(),
        output: String::new(),
    }];
    assert!(cache.save_transcript("sess-r", &stale, "2026-01-01T00:00:00.000Z"));

    let (port_tx, port_rx) = mpsc::channel();
    let _server = FakeServer::spawn(port_tx);
    let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake server port");

    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(Box::new(RecordingListener::new(ev_tx)), String::new());
    core.connect(grouse_core::ServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        secret_key: "test-secret".to_string(),
        use_tls: false,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
    });
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");

    // Stale cache for sess-r (no session/list yet -> unknown updatedAt ->
    // replay). The replay must end with the cache rewritten.
    core.open_session("sess-r".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "resume ready");

    // The probe (13:00) lands asynchronously after Ready; poll for it. It
    // wins over the replay's own session_info_update (12:00): the stamp is
    // deterministic and matches the session/list entry byte-for-byte, so the
    // next open is fresh across restarts.
    let (messages, updated_at) = {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut last: Option<(Vec<grouse_core::Message>, String)> = None;
        while Instant::now() < deadline {
            if let Some(v) = cache.load_transcript("sess-r") {
                last = Some(v.clone());
                if v.1 == "2026-08-12T13:00:00.000Z" { break; }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        last.expect("cache written after the replay")
    };
    assert_eq!(updated_at, "2026-08-12T13:00:00.000Z", "cache stamped from the session/info probe");
    let text: String = messages.iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("");
    assert!(text.contains("replayed line one and two"), "cache holds the replayed content: {text}");

    core.disconnect();
    let _ = std::fs::remove_dir_all(&data_dir);
}

// ---------------------------------------------------------------------------
// Directory cache: a cold start emits the cached session names immediately
// ---------------------------------------------------------------------------

#[test]
fn cold_start_emits_cached_directory() {
    let _cache_guard = CACHE_TEST_LOCK.lock();
    let data_dir = std::env::temp_dir().join(format!("grouse-dir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    std::env::set_var("XDG_DATA_HOME", &data_dir);
    let cache = grouse_core::cache::CacheStore::new(data_dir.join("grouse"));
    let sessions = vec![grouse_core::SessionSummary {
        id: "sess-c".into(),
        title: "Cached Chat".into(),
        updated_at: "2026-08-12T10:00:00Z".into(),
        last_message_snippet: None,
        project_id: None,
        message_count: 3,
        model: String::new(),
        has_recipe: false,
        has_new: false,
    }];
    let mut cwds = std::collections::BTreeMap::new();
    cwds.insert("sess-c".into(), "/tmp".into());
    assert!(cache.save_directory(&sessions, &cwds));

    // A brand-new Core (no connect yet) must emit the cached names right away.
    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(Box::new(RecordingListener::new(ev_tx)), String::new());
    match wait_for(&ev_rx, |ev| matches!(ev, Ev::Sessions(_)), "cached sessions") {
        Ev::Sessions(s) => {
            assert_eq!(s.len(), 1);
            assert_eq!(s[0].id, "sess-c");
            assert_eq!(s[0].title, "Cached Chat");
        }
        _ => unreachable!(),
    }
    core.disconnect();
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// The Android regression: the UI supplies a real cache dir at construction
/// (context.filesDir). The env-based default resolves nowhere writable there,
/// so a core built without the dir silently cached nothing — the drawer was
/// empty on every cold start.
#[test]
fn cold_start_emits_cached_directory_from_supplied_dir() {
    let dir = std::env::temp_dir().join(format!("grouse-supplied-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cache = grouse_core::cache::CacheStore::new(dir.clone());
    let sessions = vec![grouse_core::SessionSummary {
        id: "sess-d".into(),
        title: "Supplied Chat".into(),
        updated_at: "2026-08-13T11:00:00Z".into(),
        last_message_snippet: None,
        project_id: None,
        message_count: 2,
        model: String::new(),
        has_recipe: false,
        has_new: false,
    }];
    assert!(cache.save_directory(&sessions, &std::collections::BTreeMap::new()));

    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(
        Box::new(RecordingListener::new(ev_tx)),
        dir.to_string_lossy().into_owned(),
    );
    match wait_for(&ev_rx, |ev| matches!(ev, Ev::Sessions(_)), "cached sessions") {
        Ev::Sessions(s) => {
            assert_eq!(s.len(), 1);
            assert_eq!(s[0].id, "sess-d");
            assert_eq!(s[0].title, "Supplied Chat");
        }
        _ => unreachable!(),
    }
    core.disconnect();
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Gap fixes: active-run + commands events, recipe on a cold-start connect
// ---------------------------------------------------------------------------

#[test]
fn spine_e2e_active_run_commands_and_recipe_connect() {
    let (port_tx, port_rx) = mpsc::channel();
    let server = FakeServer::spawn(port_tx);
    server.notify.store(true, std::sync::atomic::Ordering::SeqCst);
    let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake server port");

    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(Box::new(RecordingListener::new(ev_tx)), String::new());

    core.connect(grouse_core::ServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        secret_key: "test-secret".to_string(),
        use_tls: false,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: Some("r-42".to_string()),
    });

    // The recipe rode the session/new call (cold-start recipe run, gap 4).
    let new_frames = server.frames_for("session/new");
    assert_eq!(new_frames.len(), 1, "exactly one session/new");
    assert_eq!(
        new_frames[0].pointer("/params/_meta/recipeId").and_then(Value::as_str),
        Some("r-42"),
        "recipeId must ride _meta on session/new"
    );

    // The run-id event (gap 1) and the commands event (gap 2) arrived.
    match wait_for(&ev_rx, |ev| matches!(ev, Ev::ActiveRun(..)), "active run") {
        Ev::ActiveRun(sid, run_id) => {
            assert_eq!(sid, "sess-e2e");
            assert_eq!(run_id, "run-abc");
        }
        _ => unreachable!(),
    }
    match wait_for(&ev_rx, |ev| matches!(ev, Ev::Commands(..)), "commands") {
        Ev::Commands(names) => assert_eq!(names, vec!["/compact", "/undo"]),
        _ => unreachable!(),
    }

    core.disconnect();
}

// ---------------------------------------------------------------------------
// End-to-end: connect -> ready -> send_prompt -> streamed chunk on the listener
// ---------------------------------------------------------------------------

#[test]
fn spine_e2e_connect_prompt_stream() {
    let (port_tx, port_rx) = mpsc::channel();
    let server = FakeServer::spawn(port_tx);
    let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake server port");

    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(Box::new(RecordingListener::new(ev_tx)), String::new());

    // connect() blocks (bounded) until the handshake completes.
    core.connect(grouse_core::ServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        secret_key: "test-secret".to_string(),
        use_tls: false,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
    });

    // Ready + the bound session + config.
    assert!(
        matches!(
            wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "Ready"),
            Ev::Status(ConnectionStatus::Ready)
        ),
        "expected the connection to reach Ready"
    );
    assert!(core.ready(), "ready() should be true after the handshake");
    assert_eq!(core.active_session_id().as_deref(), Some("sess-e2e"));
    assert_eq!(
        core.config(),
        vec![ConfigOption {
            id: "provider".to_string(),
            value: "openai".to_string(),
            name: "Provider".to_string(),
            choices: vec![],
        }]
    );

    // The wire shape: initialize carried the goose client caps + the secret
    // key rode the upgrade header.
    assert_eq!(
        server.secret_header.lock().as_deref(),
        Some("test-secret"),
        "the custom transport must send X-Secret-Key on the upgrade"
    );
    let init = server.frames_for("initialize");
    assert_eq!(init.len(), 1, "exactly one initialize");
    assert_eq!(
        init[0]
            .pointer("/params/protocolVersion")
            .and_then(Value::as_i64),
        Some(1)
    );
    assert_eq!(
        init[0]
            .pointer("/params/clientCapabilities/_meta/goose/recipeParameterRequests")
            .and_then(Value::as_bool),
        Some(true),
        "session/new would hard-fail for parameterized recipes without this cap"
    );
    let new_session = server.frames_for("session/new");
    assert_eq!(
        new_session[0]
            .pointer("/params/_meta/client")
            .and_then(Value::as_str),
        Some("grouse-core-test")
    );

    // Prompt with the matching expect -> streams back on the listener.
    core.send_prompt(
        Prompt { blocks: vec![PromptBlock::Text { text: "hello".to_string() }] },
        Some(SendExpect { session_id: "sess-e2e".to_string() }),
    );

    let chunk = wait_for(
        &ev_rx,
        |ev| {
            matches!(
                ev,
                Ev::Stream(StreamEvent::AgentChunk { text, .. }) if text == "hello from fake goose"
            )
        },
        "agent chunk",
    );
    let Ev::Stream(StreamEvent::AgentChunk { message_id, .. }) = chunk else {
        unreachable!()
    };
    assert_eq!(message_id, "m-1");

    let ended = wait_for(
        &ev_rx,
        |ev| {
            matches!(
                ev,
                Ev::Stream(StreamEvent::RunEnded { stop_reason }) if stop_reason == "end_turn"
            )
        },
        "RunEnded",
    );
    let Ev::Stream(StreamEvent::RunEnded { stop_reason }) = ended else {
        unreachable!()
    };
    assert_eq!(stop_reason, "end_turn");

    // The accumulated transcript: user echo + one agent bubble.
    let transcript = core.transcript();
    let user = transcript
        .iter()
        .find(|message| message.role == "user")
        .expect("user bubble");
    assert_eq!(user.content, "hello");
    let agent = transcript
        .iter()
        .find(|message| message.role == "agent")
        .expect("agent bubble");
    assert_eq!(
        agent.content, "hello from fake goose and more",
        "same messageId accumulates into one bubble"
    );
    assert_eq!(agent.id, "m-1");

    // SendExpect rejection: a mismatched session is dropped, never mis-routed.
    let prompt_frames_before = server.frames_for("session/prompt").len();
    core.send_prompt(
        Prompt { blocks: vec![PromptBlock::Text { text: "stray".to_string() }] },
        Some(SendExpect { session_id: "some-other-chat".to_string() }),
    );
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        server.frames_for("session/prompt").len(),
        prompt_frames_before,
        "a send against a stale UI session must not reach the wire"
    );

    // Explicit disconnect: no reconnect, terminal Disconnected.
    core.disconnect();
    let status = wait_for(
        &ev_rx,
        |ev| matches!(ev, Ev::Status(ConnectionStatus::Disconnected)),
        "Disconnected",
    );
    assert!(matches!(status, Ev::Status(ConnectionStatus::Disconnected)));
    assert!(!core.ready());
    // No session/prompt ever went out after the first one.
    assert_eq!(server.frames_for("session/prompt").len(), 1);
}

// ---------------------------------------------------------------------------
// Stale cache: painted for the connect, then REPLACED by the replay — not
// appended to. The replay's chunks open new bubbles (append_chunk only ever
// continues the currently-open one; it never matches an existing bubble by
// message id), so a painted cache left in place duplicates the transcript.
// ---------------------------------------------------------------------------

#[test]
fn stale_cache_is_painted_then_replaced_not_appended() {
    let _cache_guard = CACHE_TEST_LOCK.lock();
    let data_dir =
        std::env::temp_dir().join(format!("grouse-paint-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    std::env::set_var("XDG_DATA_HOME", &data_dir);

    let cache = grouse_core::cache::CacheStore::new(data_dir.join("grouse"));
    let stale = vec![grouse_core::Message {
        id: "m-old".into(),
        role: "agent".into(),
        content: "STALE-MARKER".into(),
        output: String::new(),
    }];
    // An updatedAt the server will not agree with => stale => the load replays.
    assert!(cache.save_transcript("sess-r", &stale, "2026-01-01T00:00:00.000Z"));

    let (port_tx, port_rx) = mpsc::channel();
    let _server = FakeServer::spawn(port_tx);
    let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake server port");

    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(Box::new(RecordingListener::new(ev_tx)), String::new());
    core.connect(grouse_core::ServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        secret_key: "test-secret".to_string(),
        use_tls: false,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
    });
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");

    core.open_session("sess-r".to_string());
    // The paint is synchronous inside open_session, so by the time the intent
    // returns the stale rows are on screen — that is the point of painting.
    let painted: String =
        core.transcript().iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("");
    assert!(painted.contains("STALE-MARKER"), "cache must paint instantly, got: {painted}");

    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "resume ready");

    let final_text: String =
        core.transcript().iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("");
    assert!(
        final_text.contains("replayed line one and two"),
        "the replay must land: {final_text}"
    );
    // The regression: the painted rows must be GONE, not sitting above the
    // replayed copy of the same conversation.
    assert!(
        !final_text.contains("STALE-MARKER"),
        "painted cache survived the replay (transcript duplicated): {final_text}"
    );
    assert_eq!(
        final_text.matches("replayed line one").count(),
        1,
        "replayed content must appear exactly once: {final_text}"
    );

    core.disconnect();
    let _ = std::fs::remove_dir_all(&data_dir);
}

// ---------------------------------------------------------------------------
// Cold start: the painted cache belongs to the session it came from, NOT to the
// throwaway session the first connect creates on the way to resuming it.
// ---------------------------------------------------------------------------

#[test]
fn cold_start_paint_is_not_filed_under_the_transient_session() {
    let _cache_guard = CACHE_TEST_LOCK.lock();
    let data_dir =
        std::env::temp_dir().join(format!("grouse-owner-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    std::env::set_var("XDG_DATA_HOME", &data_dir);
    let cache_dir = data_dir.join("grouse");

    let cache = grouse_core::cache::CacheStore::new(cache_dir.clone());
    let seeded = vec![grouse_core::Message {
        id: "m-1".into(),
        role: "agent".into(),
        content: "OWNED-BY-SESS-R".into(),
        output: String::new(),
    }];
    assert!(cache.save_transcript("sess-r", &seeded, "2026-01-01T00:00:00.000Z"));

    let (port_tx, port_rx) = mpsc::channel();
    let _server = FakeServer::spawn(port_tx);
    let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake server port");

    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(Box::new(RecordingListener::new(ev_tx)), String::new());

    // The cold-start sequence the UI runs: paint the last chat, THEN connect.
    // connect() creates a throwaway session before the real resume.
    core.load_cached_transcript("sess-r".to_string());
    core.connect(grouse_core::ServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        secret_key: "test-secret".to_string(),
        use_tls: false,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
    });
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");
    // save_cache runs on ready, and probe_stamp_and_save follows asynchronously.
    std::thread::sleep(Duration::from_millis(600));

    // sess-e2e is the throwaway the fake server hands back from session/new.
    assert!(
        cache.load_transcript("sess-e2e").is_none(),
        "the painted rows were filed under the throwaway session"
    );

    // And nothing else picked them up either: exactly one transcript file, the
    // one they came from.
    let transcripts: Vec<String> = std::fs::read_dir(&cache_dir)
        .expect("cache dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".json") && n != "directory.json" && !n.ends_with("-tools.json"))
        .collect();
    assert_eq!(
        transcripts,
        vec!["sess-r.json".to_string()],
        "one transcript file, owned by the session it came from"
    );

    // The paint itself must survive — it is what the user is looking at.
    let painted: String =
        core.transcript().iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("");
    assert!(painted.contains("OWNED-BY-SESS-R"), "paint must stay on screen: {painted}");

    core.disconnect();
    let _ = std::fs::remove_dir_all(&data_dir);
}
