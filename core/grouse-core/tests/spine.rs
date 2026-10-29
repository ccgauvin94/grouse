// SPDX-License-Identifier: AGPL-3.0-or-later

//! End-to-end spine tests against an in-process scripted WebSocket server
//! (the only wire-level protocol coverage; the desktop is a thin client over
//! the C ABI and has no C++ wire client to test).
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
    ConfigChoice, PromptBlock, SendExpect, SessionSummary, TranscriptOp,
};
use parking_lot::Mutex;
use serde_json::{Value, json};

/// Both cache e2e tests set the process-global XDG_DATA_HOME; they must not
/// run concurrently or each reads the other's cache dir.
static CACHE_TEST_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// Recording listener
// ---------------------------------------------------------------------------

// Vec<String> images on Item outsize some other variant enough to trip
// clippy's variant-size lint; boxing over the whole recording surface is not
// worth the churn for a test-only type.
#[allow(clippy::large_enum_variant)]
#[allow(dead_code)] // full listener surface; tests match a subset
enum Ev {
    Status(ConnectionStatus),
    Sessions(Vec<SessionSummary>),
    Item(TranscriptOp),
    Usage(i64, i64, f64, String),
    RunEnded(String, String),
    Speak(String),
    Config(Vec<ConfigOption>),
    Permission(PermissionRequest),
    Touched(String, String, String),
    Projects(Vec<ProjectSummary>),
    RoamPeerStatus(String, String),
    RoamSessions(String, Vec<SessionSummary>),
    PeerNewSession(String, String),
    ActiveRun(String, String),
    Commands(Vec<String>),
    /// An open/resume failed; nothing was substituted (grouse#19).
    OpenFailed(String, String),
    Tasks(String),
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
    fn on_item(&self, op: TranscriptOp) {
        let _ = self.tx.send(Ev::Item(op));
    }
    fn on_usage(&self, used: i64, size: i64, cost: f64, currency: String) {
        let _ = self.tx.send(Ev::Usage(used, size, cost, currency));
    }
    fn on_run_ended(&self, session_id: String, stop_reason: String) {
        let _ = self.tx.send(Ev::RunEnded(session_id, stop_reason));
    }
    fn on_speak(&self, text: String) {
        let _ = self.tx.send(Ev::Speak(text));
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
    fn on_session_open_failed(&self, session_id: String, message: String) {
        let _ = self.tx.send(Ev::OpenFailed(session_id, message));
    }
    fn on_tasks(&self, tasks: String) {
        let _ = self.tx.send(Ev::Tasks(tasks));
    }
}

/// A rich cache-seed item (the cache is the item list now).
fn item(id: &str, kind: grouse_core::ItemKind, text: &str) -> grouse_core::Item {
    grouse_core::Item {
        id: id.into(),
        kind,
        text: text.into(),
        detail: String::new(),
        output: String::new(),
        status: String::new(),
        app_key: String::new(),
        calls: Vec::new(),
        thought: String::new(),
        summary: String::new(),
        steps: Vec::new(),
        seq: 0,
        attachments: 0,
        images: Vec::new(),
        message_id: String::new(),
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
    /// When set, `session/prompt` is recorded and left UNANSWERED, so a test can
    /// observe the core sending a SECOND chat's prompt while the first turn is
    /// still in flight (per-session concurrency).
    hold_prompts: Arc<std::sync::atomic::AtomicBool>,
    /// When set, `session/prompt` emits a run-end `session_info_update` (no
    /// activeRunId) before answering — the server's own post-turn touch that
    /// must not be mistaken for a remote change.
    post_turn_touch: Arc<std::sync::atomic::AtomicBool>,
    /// Fired by a test to close the live WebSocket(s), so the core's reconnect
    /// path (and its `session/load` replay) actually runs.
    drop_now: Arc<tokio::sync::Notify>,
    /// When set, the `session/load` replay stamps its rows with a DIFFERENT
    /// message id — modelling goose, which re-assigns ids on every replay, so a
    /// replay's rows do not match the live rows they restate.
    replay_alt_id: Arc<std::sync::atomic::AtomicBool>,
    /// The live connection's out-of-band push half, so a test can inject a
    /// notification (e.g. a remote `session_info_update` that makes the on-disk
    /// cache stale) without a request to hang it on.
    push: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Value>>>>,
    /// When set, `session/prompt` on the MAIN session also emits a thinking
    /// chunk and a tool call — a turn with a run for the summarizer to label.
    tool_turn: Arc<std::sync::atomic::AtomicBool>,
    /// The text of the last prompt sent. `session/load` replays it as a
    /// `user_message_chunk` — the ONLY place a prompt reaches the client, since
    /// goose never echoes it live.
    last_prompt: Arc<Mutex<String>>,
    /// When set, `session/load` answers with a JSON-RPC error (a stale/archived
    /// session) instead of replaying. Drives the "an open failure must report,
    /// never silently become a new session" coverage (grouse#19).
    fail_load: Arc<std::sync::atomic::AtomicBool>,
    /// When set, `session/load` streams its replay rows and then WAITS for
    /// `release_load` before replying — the window in which the core must
    /// ingest the replay silently (grouse#18).
    gate_load: Arc<std::sync::atomic::AtomicBool>,
    release_load: Arc<tokio::sync::Notify>,
}

impl FakeServer {
    /// Bind on 127.0.0.1:0 and spawn the accept+serve loop. Returns when the
    /// listener is bound (the port is delivered through `port_tx`).
    fn spawn(port_tx: mpsc::Sender<u16>) -> Arc<Self> {
        let server = Arc::new(Self {
            frames: Arc::new(Mutex::new(Vec::new())),
            secret_header: Arc::new(Mutex::new(None)),
            notify: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hold_prompts: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            post_turn_touch: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            drop_now: Arc::new(tokio::sync::Notify::new()),
            replay_alt_id: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            push: Arc::new(Mutex::new(None)),
            tool_turn: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_prompt: Arc::new(Mutex::new(String::new())),
            fail_load: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            gate_load: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            release_load: Arc::new(tokio::sync::Notify::new()),
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

    /// Close every live connection, so the core's reconnect path runs.
    fn drop_connections(&self) {
        self.drop_now.notify_one();
    }

    /// Push a notification to the live connection, out of band.
    fn push_notification(&self, value: Value) {
        if let Some(tx) = self.push.lock().as_ref() {
            let _ = tx.send(value);
        }
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

    let (push_tx, mut push_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    *server.push.lock() = Some(push_tx);
    let (mut tx, mut rx) = ws.split();
    loop {
        let next = tokio::select! {
            _ = server.drop_now.notified() => break,
            Some(pushed) = push_rx.recv() => {
                let _ = tx.send(WsMessage::Text(pushed.to_string().into())).await;
                continue;
            }
            msg = rx.next() => msg,
        };
        let Some(Ok(message)) = next else { break };
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
            // The core's hidden summarizer session (issue #13) asks with
            // `_meta.hidden`; give it an id of its own so its turn is
            // distinguishable from a chat's.
            "session/new" if params.pointer("/_meta/hidden").and_then(Value::as_bool) == Some(true) => {
                json!({ "sessionId": "sess-sum", "configOptions": [] })
            }
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
                        { "id": "provider", "name": "Provider", "currentValue": "openai" },
                        { "id": "model", "name": "Model", "currentValue": "m-a",
                          "options": [ { "value": "m-a", "name": "A" },
                                       { "value": "m-b", "name": "B" } ] }
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
                let mid = if server.replay_alt_id.load(std::sync::atomic::Ordering::SeqCst) {
                    "r-2"
                } else {
                    "r-1"
                };
                // A replay DOES carry the prompts that were sent: the server
                // stores them even though it never echoed them live.
                let prompt = server.last_prompt.lock().clone();
                let mut rows: Vec<(&str, String, &str)> = Vec::new();
                if !prompt.is_empty() {
                    rows.push(("user_message_chunk", prompt, "u-1"));
                }
                rows.push(("agent_message_chunk", "replayed line one".to_string(), mid));
                rows.push(("agent_message_chunk", " and two".to_string(), mid));
                for (tag, text, message_id) in rows {
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
                // Test hook: hold the reply open after the rows are on the wire,
                // so a test can assert the client saw NOTHING while the replay
                // was being ingested (grouse#18).
                if server.gate_load.load(std::sync::atomic::Ordering::SeqCst) {
                    server.release_load.notified().await;
                }
                json!({
                    "sessionId": session,
                    "configOptions": [
                        { "id": "provider", "name": "Provider", "currentValue": "openai" }
                    ]
                })
            }
            // The summarizer's own turn: answer with the numbered labels the
            // core parses (issue #13).
            "session/prompt"
                if params.get("sessionId").and_then(Value::as_str) == Some("sess-sum") =>
            {
                let frame = json!({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": "sess-sum",
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": { "type": "text", "text": "1. Look up the charging issue" },
                            "messageId": "sum-1"
                        }
                    }
                });
                let _ = tx.send(WsMessage::Text(frame.to_string().into())).await;
                json!({ "stopReason": "end_turn" })
            }
            "session/prompt" if server.hold_prompts.load(std::sync::atomic::Ordering::SeqCst) => {
                // The frame is already recorded. Leave the request unanswered:
                // the concurrency test asserts a SECOND chat's prompt goes out
                // while this turn is still in flight.
                continue;
            }
            "session/prompt" => {
                // Stream the turn BEFORE the reply: two chunks of one agent
                // message (same messageId -> one bubble), then end.
                //
                // NOTE: no `user_message_chunk`. Real goose does NOT echo the
                // prompt — a user chunk for a prompt we sent never crosses the
                // wire (verified against a live serve); it appears only in a
                // later `session/load` replay. The harness echoed it for a long
                // time, which is exactly why the missing local echo went
                // unnoticed: every test saw a user row the real server never
                // sends.
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
                *server.last_prompt.lock() = prompt_text;
                for (tag, text, message_id) in [
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
                // A turn with a tool run: the thinking that precedes it becomes
                // the run's framing, which the summarizer then rewrites (#13).
                if server.tool_turn.load(std::sync::atomic::Ordering::SeqCst) {
                    let thought = json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session,
                            "update": {
                                "sessionUpdate": "agent_thought_chunk",
                                "content": { "type": "text", "text": "Let me look up the charging issue." }
                            }
                        }
                    });
                    let _ = tx.send(WsMessage::Text(thought.to_string().into())).await;
                    let call = json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session,
                            "update": {
                                "sessionUpdate": "tool_call",
                                "toolCallId": "tc-1",
                                "title": "kagi_search_fetch",
                                "rawInput": { "query": "fairphone charging" }
                            }
                        }
                    });
                    let _ = tx.send(WsMessage::Text(call.to_string().into())).await;
                }
                if server
                    .post_turn_touch
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    // The server's run-end touch: an updatedAt with no
                    // activeRunId. Sent BEFORE the prompt reply, so its 1.5s
                    // resync debounce lands after on_prompt_done clears
                    // `prompting` — the race that used to replay our own turn.
                    let touch = json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session,
                            "update": {
                                "sessionUpdate": "session_info_update",
                                "updatedAt": "2026-08-12T14:00:00.000Z"
                            }
                        }
                    });
                    let _ = tx.send(WsMessage::Text(touch.to_string().into())).await;
                }
                json!({ "stopReason": "end_turn" })
            }
            "_goose/unstable/session/info" => json!({
                "session": {
                    "updatedAt": "2026-08-12T13:00:00.000Z",
                    "_meta": { "messageCount": 3 }
                }
            }),
            // goose's real shape for a select set: the CURRENT list with the
            // chosen value, and (as on the typed notification) no per-option
            // choices for the changed entry.
            "session/set_config_option" => json!({
                "configOptions": [
                    { "id": "model", "name": "Model", "currentValue": "m-b" }
                ]
            }),
            _ => json!({}),
        };
        let reply = if server.fail_load.load(std::sync::atomic::Ordering::SeqCst)
            && method == "session/load"
        {
            // A stale/archived session: goose answers session/load with an
            // error. The core must REPORT this, never substitute a session
            // (grouse#19).
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32602, "message": "session not found" }
            })
        } else {
            json!({ "jsonrpc": "2.0", "id": id, "result": result })
        };
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
    let stale = vec![item("m-old", grouse_core::ItemKind::User, "stale cached line")];
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
        accept_invalid_certs: false,
        ca_cert_pem: None,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
        initial_project_id: None,
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
        let mut last: Option<(Vec<grouse_core::Item>, String)> = None;
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
    let text: String = messages.iter().map(|m| m.text.clone()).collect::<Vec<_>>().join("");
    assert!(text.contains("replayed line one and two"), "cache holds the replayed content: {text}");

    core.disconnect();
    let _ = std::fs::remove_dir_all(&data_dir);
}

// ---------------------------------------------------------------------------
// grouse#19: a failed open must REPORT, never silently bind a new session
// ---------------------------------------------------------------------------

/// A live-wire resume the server rejects (stale/archived id) must not become a
/// fresh session behind the client's back. The client already moved its screen
/// to the tapped session; a silent substitute is what made the app show a blank
/// chat labelled with the selected one.
#[test]
fn rejected_resume_reports_and_binds_no_substitute_session() {
    let _cache_guard = CACHE_TEST_LOCK.lock();
    let data_dir = std::env::temp_dir().join(format!("grouse-failload-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    std::env::set_var("XDG_DATA_HOME", &data_dir);

    let (port_tx, port_rx) = mpsc::channel();
    let server = FakeServer::spawn(port_tx);
    let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake server port");
    server.fail_load.store(true, std::sync::atomic::Ordering::SeqCst);

    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(Box::new(RecordingListener::new(ev_tx)), String::new());
    core.connect(grouse_core::ServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        secret_key: "test-secret".to_string(),
        use_tls: false,
        accept_invalid_certs: false,
        ca_cert_pem: None,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
        initial_project_id: None,
    });
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");
    assert_eq!(core.active_session_id().as_deref(), Some("sess-e2e"));

    core.open_session("sess-gone".to_string());
    match wait_for(&ev_rx, |ev| matches!(ev, Ev::OpenFailed(..)), "the open failure") {
        Ev::OpenFailed(session_id, message) => {
            assert_eq!(session_id, "sess-gone");
            assert!(
                message.contains("session/load"),
                "the report names the failing call: {message}"
            );
        }
        _ => panic!("expected OpenFailed from the rejected resume"),
    }

    // No substitute: only the connect's own transient session/new ever crossed
    // the wire, and nothing is left bound — a rejected load must not leave the
    // connection claiming a session it never loaded.
    assert_eq!(
        server.frames_for("session/new").len(),
        1,
        "the rejected resume must not create a session"
    );
    assert_eq!(
        core.active_session_id(),
        None,
        "a rejected resume binds no session"
    );

    core.disconnect();
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// The same rejection on the COLD path (the handshake's resume, what a
/// cold-start tap rides): report it, leave the wire with NO session, and never
/// hand the client a chat it did not ask for.
#[test]
fn cold_resume_rejection_reports_and_binds_nothing() {
    let _cache_guard = CACHE_TEST_LOCK.lock();
    let data_dir =
        std::env::temp_dir().join(format!("grouse-failload-cold-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    std::env::set_var("XDG_DATA_HOME", &data_dir);

    let (port_tx, port_rx) = mpsc::channel();
    let server = FakeServer::spawn(port_tx);
    let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake server port");
    server.fail_load.store(true, std::sync::atomic::Ordering::SeqCst);

    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(Box::new(RecordingListener::new(ev_tx)), String::new());
    core.connect_resume(
        grouse_core::ServerConfig {
            host: "127.0.0.1".to_string(),
            port,
            secret_key: "test-secret".to_string(),
            use_tls: false,
            accept_invalid_certs: false,
            ca_cert_pem: None,
            cwd: "/tmp".to_string(),
            auto_connect: false,
            client_id: "grouse-core-test".to_string(),
            initial_recipe_id: None,
            initial_project_id: None,
        },
        "sess-gone".to_string(),
    );

    match wait_for(&ev_rx, |ev| matches!(ev, Ev::OpenFailed(..)), "the cold open failure") {
        Ev::OpenFailed(session_id, message) => {
            assert_eq!(session_id, "sess-gone");
            assert!(
                message.contains("session/load"),
                "the report names the failing call: {message}"
            );
        }
        _ => panic!("expected OpenFailed from the rejected resume"),
    }
    // The wire is still usable, so Ready follows — but with nothing bound.
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "ready after failure");
    assert!(server.frames_for("session/new").is_empty(), "no substitute session/new");
    assert_eq!(
        core.active_session_id(),
        None,
        "a rejected resume binds no session at all"
    );

    core.disconnect();
    let _ = std::fs::remove_dir_all(&data_dir);
}

// ---------------------------------------------------------------------------
// grouse#18: a replay is ingested silently and painted once
// ---------------------------------------------------------------------------

/// A reload must not walk the history from the top. While the server is still
/// streaming the replay the client sees nothing new; the load's completion paints
/// the newest window in ONE shot (Reset + rows + cursor), which is what lets a
/// refresh land on the most recent messages with scroll-back for the rest.
#[test]
fn a_replay_is_ingested_silently_and_painted_once() {
    let _cache_guard = CACHE_TEST_LOCK.lock();
    let data_dir = std::env::temp_dir().join(format!("grouse-hold-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    std::env::set_var("XDG_DATA_HOME", &data_dir);

    let (port_tx, port_rx) = mpsc::channel();
    let server = FakeServer::spawn(port_tx);
    let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake server port");
    server.gate_load.store(true, std::sync::atomic::Ordering::SeqCst);

    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(Box::new(RecordingListener::new(ev_tx)), String::new());
    core.connect(grouse_core::ServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        secret_key: "test-secret".to_string(),
        use_tls: false,
        accept_invalid_certs: false,
        ca_cert_pem: None,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
        initial_project_id: None,
    });
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");

    core.open_session("sess-r".to_string());
    // The replay's rows are on the wire and the reply is held open: nothing they
    // contain may reach the client yet. (The open's own empty paint may.)
    std::thread::sleep(Duration::from_millis(400));
    let mut streamed = 0;
    while let Ok(ev) = ev_rx.try_recv() {
        if let Ev::Item(TranscriptOp::Upsert { .. }) = ev {
            streamed += 1;
        }
    }
    assert_eq!(streamed, 0, "the replay was painted row by row instead of held");

    // Release the reply: the paint lands as one batch, Reset first.
    server.release_load.notify_one();
    match wait_for(&ev_rx, |ev| matches!(ev, Ev::Item(_)), "the paint") {
        Ev::Item(TranscriptOp::Reset { .. }) => {}
        _ => panic!("the paint must start with a Reset"),
    }
    let mut upserts = 0;
    loop {
        match wait_for(&ev_rx, |ev| matches!(ev, Ev::Item(_)), "more of the paint") {
            Ev::Item(TranscriptOp::Upsert { .. }) => upserts += 1,
            Ev::Item(TranscriptOp::Window { .. }) => break,
            _ => panic!("unexpected op in the paint"),
        }
    }
    // The paint is exactly the newest window of what the store now holds — one
    // shot, not a stream of per-row updates.
    let expected = core.rich_transcript().len().min(60);
    assert!(expected >= 1, "the replay reached the store");
    assert_eq!(upserts, expected, "the paint carries the newest window, once");

    // The held rows did reach the store (silence is not loss).
    let text: String = core
        .transcript()
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("");
    assert!(
        text.contains("replayed line one and two"),
        "the silently ingested replay is in the transcript: {text}"
    );

    core.disconnect();
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// The two reload strengths (grouse#18). QUICK lets freshness decide — a settled
/// cache means a bounded tail, not a replay of the whole session — while FULL
/// ignores the cache and always replays. Both are what the clients' "reload"
/// gesture and its hold variant call.
#[test]
fn refresh_modes_differ_on_the_wire() {
    let _cache_guard = CACHE_TEST_LOCK.lock();
    let data_dir = std::env::temp_dir().join(format!("grouse-rmode-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);

    let (port_tx, port_rx) = mpsc::channel();
    let server = FakeServer::spawn(port_tx);
    let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake server port");

    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(
        Box::new(RecordingListener::new(ev_tx)),
        data_dir.to_string_lossy().to_string(),
    );
    core.connect(grouse_core::ServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        secret_key: "test-secret".to_string(),
        use_tls: false,
        accept_invalid_certs: false,
        ca_cert_pem: None,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
        initial_project_id: None,
    });
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");

    // Open once so a settled cache exists for the reloads to reason about.
    core.open_session("sess-a".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "first open");

    core.refresh_session("sess-a".to_string(), grouse_core::RefreshMode::Quick);
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "quick reload ready");
    let quick = server
        .frames_for("session/load")
        .last()
        .cloned()
        .expect("a session/load for the quick reload");
    assert!(
        quick.pointer("/params/_meta/replayTail").is_some(),
        "a QUICK reload must let freshness pick the bounded path: {quick:?}"
    );

    core.refresh_session("sess-a".to_string(), grouse_core::RefreshMode::Full);
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "full reload ready");
    let full = server
        .frames_for("session/load")
        .last()
        .cloned()
        .expect("a session/load for the full reload");
    assert!(
        full.pointer("/params/_meta").is_none(),
        "a FULL reload must replay the session, not ask for a tail: {full:?}"
    );

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
        archived: false,
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
        archived: false,
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
        accept_invalid_certs: false,
        ca_cert_pem: None,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: Some("r-42".to_string()),
        initial_project_id: Some("proj-7".to_string()),
    });

    // The recipe rode the session/new call (cold-start recipe run, gap 4).
    let new_frames = server.frames_for("session/new");
    assert_eq!(new_frames.len(), 1, "exactly one session/new");
    assert_eq!(
        new_frames[0].pointer("/params/_meta/recipeId").and_then(Value::as_str),
        Some("r-42"),
        "recipeId must ride _meta on session/new"
    );
    assert_eq!(
        new_frames[0].pointer("/params/_meta/projectId").and_then(Value::as_str),
        Some("proj-7"),
        "projectId must ride _meta on session/new (grouse#5 P0)"
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

/// Turns are concurrent PER SESSION: with the first chat's `session/prompt`
/// still unanswered, a prompt for a second chat on the same wire must go out.
/// The old global `prompting` latch queued it until the first turn ended.
#[test]
fn a_second_chat_prompts_while_the_first_turn_is_in_flight() {
    let _guard = CACHE_TEST_LOCK.lock();
    let data_dir =
        std::env::temp_dir().join(format!("grouse-concurrent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);

    let (port_tx, port_rx) = mpsc::channel();
    let server = FakeServer::spawn(port_tx);
    let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake server port");
    // Neither prompt is answered: the test observes the wire, not run ends.
    server
        .hold_prompts
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(
        Box::new(RecordingListener::new(ev_tx)),
        data_dir.to_string_lossy().to_string(),
    );
    core.connect(grouse_core::ServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        secret_key: "test-secret".to_string(),
        use_tls: false,
        accept_invalid_certs: false,
        ca_cert_pem: None,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
        initial_project_id: None,
    });
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "Ready");
    assert_eq!(core.active_session_id().as_deref(), Some("sess-e2e"));

    core.send_prompt(
        Prompt { blocks: vec![PromptBlock::Text { text: "first".to_string() }] },
        Some(SendExpect { session_id: "sess-e2e".to_string() }),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.frames_for("session/prompt").is_empty() {
        assert!(Instant::now() < deadline, "the first prompt never reached the wire");
        std::thread::sleep(Duration::from_millis(20));
    }

    // A second chat on the same wire (the display moves; the first turn is
    // still in flight).
    core.open_session("sess-b".to_string());
    let deadline = Instant::now() + Duration::from_secs(10);
    while core.active_session_id().as_deref() != Some("sess-b") {
        assert!(Instant::now() < deadline, "sess-b never became the displayed chat");
        std::thread::sleep(Duration::from_millis(20));
    }

    core.send_prompt(
        Prompt { blocks: vec![PromptBlock::Text { text: "second".to_string() }] },
        Some(SendExpect { session_id: "sess-b".to_string() }),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.frames_for("session/prompt").len() < 2 {
        assert!(
            Instant::now() < deadline,
            "a turn in the first chat held the second chat's prompt"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let ids: Vec<String> = server
        .frames_for("session/prompt")
        .iter()
        .filter_map(|frame| {
            frame
                .pointer("/params/sessionId")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    assert!(ids.contains(&"sess-e2e".to_string()), "{ids:?}");
    assert!(ids.contains(&"sess-b".to_string()), "{ids:?}");

    core.disconnect();
    let _ = std::fs::remove_dir_all(&data_dir);
}

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
        accept_invalid_certs: false,
        ca_cert_pem: None,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
        initial_project_id: None,
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
        vec![
            ConfigOption {
                id: "provider".to_string(),
                value: "openai".to_string(),
                name: "Provider".to_string(),
                choices: vec![],
            },
            ConfigOption {
                id: "model".to_string(),
                value: "m-a".to_string(),
                name: "Model".to_string(),
                choices: vec![
                    ConfigChoice { value: "m-a".to_string(), name: "A".to_string() },
                    ConfigChoice { value: "m-b".to_string(), name: "B".to_string() },
                ],
            }
        ]
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
        |ev| matches!(ev, Ev::Item(TranscriptOp::Upsert { item }) if item.id == "m-1"),
        "agent item",
    );
    let Ev::Item(TranscriptOp::Upsert { item }) = chunk else {
        unreachable!()
    };
    assert_eq!(item.id, "m-1");
    assert!(item.text.contains("hello from fake goose"), "{:?}", item.text);

    let ended = wait_for(
        &ev_rx,
        |ev| matches!(ev, Ev::RunEnded(_, r) if r == "end_turn"),
        "RunEnded",
    );
    let Ev::RunEnded(_session, stop_reason) = ended else {
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
// A prompt's own run-end `session_info_update` must NOT trigger a resync replay
// of the turn the client just streamed. On a long chat that replay is the whole
// history reloading ("send a reply and the chat reloads from the top").
// ---------------------------------------------------------------------------

#[test]
fn prompt_does_not_resync_its_own_turn() {
    let (port_tx, port_rx) = mpsc::channel();
    let server = FakeServer::spawn(port_tx);
    server
        .post_turn_touch
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake server port");

    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(Box::new(RecordingListener::new(ev_tx)), String::new());

    core.connect(grouse_core::ServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        secret_key: "test-secret".to_string(),
        use_tls: false,
        accept_invalid_certs: false,
        ca_cert_pem: None,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
        initial_project_id: None,
    });
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "Ready");
    assert_eq!(core.active_session_id().as_deref(), Some("sess-e2e"));

    core.send_prompt(
        Prompt { blocks: vec![PromptBlock::Text { text: "hello".to_string() }] },
        Some(SendExpect { session_id: "sess-e2e".to_string() }),
    );
    wait_for(
        &ev_rx,
        |ev| matches!(ev, Ev::RunEnded(_, _)),
        "RunEnded",
    );

    // The run-end touch debounces 1.5s before it probes; give the whole cycle
    // room. A spurious resync would clear the store and issue `session/load`,
    // re-streaming the transcript.
    std::thread::sleep(Duration::from_millis(2500));
    assert!(
        server.frames_for("session/load").is_empty(),
        "the server's own run-end touch must not replay the session: {:?}",
        server.frames_for("session/load")
    );
    let agent_count = core
        .transcript()
        .iter()
        .filter(|m| m.content == "hello from fake goose and more")
        .count();
    assert_eq!(
        agent_count, 1,
        "the reply must appear once; a resync rebuild would duplicate it"
    );

    core.disconnect();
}

// ---------------------------------------------------------------------------
// set_config_option updates merge IN PLACE: the changed option takes the new
// value while its choices list survives an update that carries none (the
// typed notification shape), and untouched options keep everything. A
// wholesale replace emptied both dropdowns with no way back.
// ---------------------------------------------------------------------------

#[test]
fn set_config_update_merges_and_preserves_choices() {
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
        accept_invalid_certs: false,
        ca_cert_pem: None,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
        initial_project_id: None,
    });
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "Ready");

    core.set_config_option("model".to_string(), "m-b".to_string());
    wait_for(
        &ev_rx,
        |ev| {
            matches!(ev, Ev::Config(options)
                if options.iter().any(|o| o.id == "model" && o.value == "m-b"))
        },
        "merged config",
    );

    let config = core.config();
    assert_eq!(config.len(), 2, "both options survive");
    let provider = config.iter().find(|o| o.id == "provider").expect("provider");
    assert_eq!(provider.value, "openai", "untouched option keeps its value");
    let model = config.iter().find(|o| o.id == "model").expect("model");
    assert_eq!(model.value, "m-b", "the update lands");
    assert_eq!(
        model.choices.len(),
        2,
        "an update without choices must not erase the initial list"
    );

    core.disconnect();
}

// ---------------------------------------------------------------------------
// Stale cache: painted for the connect, then REPLACED by the replay — not
// appended to. The replay's chunks open new bubbles (append_chunk only ever
// continues the currently-open one; it never matches an existing bubble by
// message id), so a painted cache left in place duplicates the transcript.
// ---------------------------------------------------------------------------

#[test]
fn stale_cache_is_painted_then_replaced_not_appended() {
    // Own cache dir, passed to Core explicitly: XDG_DATA_HOME is process-global
    // and the spine_e2e_* tests build a Core with an empty cache dir, so they
    // resolve whatever it points at when THEY construct — steering it here made
    // their sess-e2e writes land in this test's directory.
    let data_dir =
        std::env::temp_dir().join(format!("grouse-paint-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let cache = grouse_core::cache::CacheStore::new(data_dir.clone());
    let stale = vec![item("m-old", grouse_core::ItemKind::Agent, "STALE-MARKER")];
    // An updatedAt the server will not agree with => stale => the load replays.
    assert!(cache.save_transcript("sess-r", &stale, "2026-01-01T00:00:00.000Z"));

    let (port_tx, port_rx) = mpsc::channel();
    let _server = FakeServer::spawn(port_tx);
    let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake server port");

    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(
        Box::new(RecordingListener::new(ev_tx)),
        data_dir.to_string_lossy().to_string(),
    );
    core.connect(grouse_core::ServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        secret_key: "test-secret".to_string(),
        use_tls: false,
        accept_invalid_certs: false,
        ca_cert_pem: None,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
        initial_project_id: None,
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

    // The ordering guarantee, which is what actually bit on 2026-09-16: the
    // painted rows had been NEWER than the history the replay delivered, so an
    // append left a 19-hour-old message as the LAST row and the user answered
    // it. After a replay the last row must be the server's last row.
    let rows: Vec<String> = core.transcript().iter().map(|m| m.content.clone()).collect();
    assert_eq!(
        rows.last().map(String::as_str),
        Some("replayed line one and two"),
        "the newest row on screen must be the newest the server sent: {rows:?}"
    );

    core.disconnect();
    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn reconnect_replaces_the_live_transcript_not_appends() {
    let _guard = CACHE_TEST_LOCK.lock();
    let data_dir =
        std::env::temp_dir().join(format!("grouse-reconnect-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let cache = grouse_core::cache::CacheStore::new(data_dir.clone());
    let stale = vec![item("m-old", grouse_core::ItemKind::Agent, "STALE-MARKER")];
    assert!(cache.save_transcript("sess-r", &stale, "2026-01-01T00:00:00.000Z"));

    let (port_tx, port_rx) = mpsc::channel();
    let server = FakeServer::spawn(port_tx);
    let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake server port");

    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(
        Box::new(RecordingListener::new(ev_tx)),
        data_dir.to_string_lossy().to_string(),
    );
    core.connect(grouse_core::ServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        secret_key: "test-secret".to_string(),
        use_tls: false,
        accept_invalid_certs: false,
        ca_cert_pem: None,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
        initial_project_id: None,
    });
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");

    core.open_session("sess-r".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "resume ready");
    let opened: String =
        core.transcript().iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("");
    assert!(opened.contains("replayed line one and two"), "open replay landed: {opened}");

    // A remote change bumps the session's updatedAt, so the on-disk cache
    // (stamped when the open finished) is now STALE — the reconnect plans a
    // merge. The store holds the LIVE transcript, not a painted cache, so there
    // is nothing to merge into: the replay must REPLACE it. (goose also
    // re-assigns ids on replay, so the replayed rows do not match the live ones.)
    std::thread::sleep(Duration::from_millis(300)); // let the open's stamp-save settle
    server.push_notification(json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "sess-r",
            "update": {
                "sessionUpdate": "session_info_update",
                "title": "Remote",
                "updatedAt": "2026-08-12T15:00:00.000Z"
            }
        }
    }));
    std::thread::sleep(Duration::from_millis(150)); // let the core ingest it
    server.replay_alt_id.store(true, std::sync::atomic::Ordering::SeqCst);

    // Key on a fresh `session/load` rather than a `Ready`: the open itself emits
    // a Ready, so a Ready wait could be satisfied without any reconnect.
    let loads_before = server.frames_for("session/load").len();
    server.drop_connections();
    let deadline = Instant::now() + Duration::from_secs(15);
    while server.frames_for("session/load").len() <= loads_before {
        assert!(Instant::now() < deadline, "the core never reconnected and reloaded");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(500));

    let after: String =
        core.transcript().iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("");
    assert_eq!(
        after.matches("replayed line one").count(),
        1,
        "reconnect must REPLACE the live rows, not append a copy: {after}"
    );
    assert!(!after.contains("STALE-MARKER"), "the stale paint must not survive: {after}");

    core.disconnect();
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// grouse#1: a prompt typed while the socket is down (the reconnect window)
/// must be QUEUED, not fired into the dead wire — and must reach the resumed
/// session once Ready arrives, with no substitute session created.
#[test]
fn prompt_while_reconnecting_is_queued_and_flushed_after_ready() {
    let _guard = CACHE_TEST_LOCK.lock();
    let data_dir =
        std::env::temp_dir().join(format!("grouse-reattach-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);

    let (port_tx, port_rx) = mpsc::channel();
    let server = FakeServer::spawn(port_tx);
    let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake server port");

    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(
        Box::new(RecordingListener::new(ev_tx)),
        data_dir.to_string_lossy().to_string(),
    );
    core.connect(grouse_core::ServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        secret_key: "test-secret".to_string(),
        use_tls: false,
        accept_invalid_certs: false,
        ca_cert_pem: None,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
        initial_project_id: None,
    });
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");
    core.open_session("sess-r".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "open ready");
    let sessions_before = server.frames_for("session/new").len();

    // Drop the wire and WAIT until the core has observed it: sending before the
    // drop is noticed would hit the still-live socket, which is a different
    // (and valid) path — this test is about the window after.
    server.drop_connections();
    wait_for(
        &ev_rx,
        |ev| matches!(ev, Ev::Status(ConnectionStatus::Error { .. })),
        "the drop is observed",
    );

    core.send_prompt(
        Prompt { blocks: vec![PromptBlock::Text { text: "typed-while-down".to_string() }] },
        Some(SendExpect { session_id: "sess-r".to_string() }),
    );
    // Nothing may go out on the dead wire: the prompt is queued instead.
    assert_eq!(
        server.frames_for("session/prompt").len(),
        0,
        "a prompt in the reconnect window must not be fired at the dead socket"
    );

    // Ready flushes it to the RESUMED session (no fresh session/new).
    let deadline = Instant::now() + Duration::from_secs(15);
    while server.frames_for("session/prompt").is_empty() {
        assert!(Instant::now() < deadline, "the queued prompt never reached the resumed wire");
        std::thread::sleep(Duration::from_millis(50));
    }
    let frame = server.frames_for("session/prompt")[0].clone();
    assert_eq!(frame.pointer("/params/sessionId").and_then(Value::as_str), Some("sess-r"));
    assert_eq!(
        frame.pointer("/params/prompt/0/text").and_then(Value::as_str),
        Some("typed-while-down")
    );
    assert_eq!(
        server.frames_for("session/new").len(),
        sessions_before,
        "the reconnect must resume the session, not substitute a new one"
    );

    core.disconnect();
    let _ = std::fs::remove_dir_all(&data_dir);
}

// ---------------------------------------------------------------------------
// Phase 1: the rich item transcript paints the cache, then the replay replaces
// it — with the row's KIND intact (a paint is lossless now).
// ---------------------------------------------------------------------------

#[test]
fn rich_transcript_paints_and_replaces_on_a_stale_open() {
    let data_dir =
        std::env::temp_dir().join(format!("grouse-rich-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let cache = grouse_core::cache::CacheStore::new(data_dir.clone());
    let stale = vec![item("m-old", grouse_core::ItemKind::Agent, "STALE-MARKER")];
    assert!(cache.save_transcript("sess-r", &stale, "2026-01-01T00:00:00.000Z"));

    let (port_tx, port_rx) = mpsc::channel();
    let _server = FakeServer::spawn(port_tx);
    let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake server port");

    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(
        Box::new(RecordingListener::new(ev_tx)),
        data_dir.to_string_lossy().to_string(),
    );
    core.connect(grouse_core::ServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        secret_key: "test-secret".to_string(),
        use_tls: false,
        accept_invalid_certs: false,
        ca_cert_pem: None,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
        initial_project_id: None,
    });
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");

    core.open_session("sess-r".to_string());
    // The paint is synchronous: the rich snapshot carries the item's kind, and
    // the window cursor points at it.
    let painted = core.rich_transcript();
    assert_eq!(painted.len(), 1, "one painted item");
    assert_eq!(painted[0].kind, grouse_core::ItemKind::Agent);
    assert_eq!(painted[0].text, "STALE-MARKER");
    assert_eq!(core.window().oldest_id, "m-old");
    assert!(!core.window().has_older, "the whole cache is the window for now");

    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "resume ready");

    let items = core.rich_transcript();
    let text: String = items.iter().map(|i| i.text.clone()).collect::<Vec<_>>().join("");
    assert!(text.contains("replayed line one and two"), "replay lands: {text}");
    assert!(!text.contains("STALE-MARKER"), "paint replaced, not appended: {text}");
    // item(id) resolves the row from the id index.
    let replayed = items.iter().find(|i| i.text.contains("replayed line one")).unwrap();
    assert_eq!(core.item(replayed.id.clone()).unwrap().kind, grouse_core::ItemKind::Agent);

    core.disconnect();
    let _ = std::fs::remove_dir_all(&data_dir);
}

// ---------------------------------------------------------------------------
// Phase 1: a stale cache is caught up with a BOUNDED TAIL that merges into the
// painted prefix, instead of replaying the whole session from the top.
// ---------------------------------------------------------------------------

#[test]
fn stale_cache_tail_merge_keeps_the_older_prefix() {
    // Same harness as stale_cache_is_painted_then_replaced_not_appended, but the
    // cache CONTAINS the replay's first id, so the bounded tail anchors into it:
    // the older prefix survives and the replayed row lands exactly once.
    let data_dir =
        std::env::temp_dir().join(format!("grouse-merge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let cache = grouse_core::cache::CacheStore::new(data_dir.clone());
    let stale = vec![
        item("old-1", grouse_core::ItemKind::User, "VERY-OLD-PREFIX"),
        item("r-1", grouse_core::ItemKind::Agent, "replayed line one and two"),
    ];
    assert!(cache.save_transcript("sess-r", &stale, "2026-01-01T00:00:00.000Z"));

    let (port_tx, port_rx) = mpsc::channel();
    let server = FakeServer::spawn(port_tx);
    let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake server port");

    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(
        Box::new(RecordingListener::new(ev_tx)),
        data_dir.to_string_lossy().to_string(),
    );
    core.connect(grouse_core::ServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        secret_key: "test-secret".to_string(),
        use_tls: false,
        accept_invalid_certs: false,
        ca_cert_pem: None,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
        initial_project_id: None,
    });
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");

    core.open_session("sess-r".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "resume ready");

    // The open asked for the bounded tail, not a full replay.
    let loads = server.frames_for("session/load");
    assert!(
        loads.iter().any(|f| {
            f.pointer("/params/_meta/replayTail").and_then(Value::as_u64) == Some(100)
        }),
        "a stale-cache open must request the bounded tail: {loads:?}"
    );

    let rows: Vec<String> = core.transcript().iter().map(|m| m.content.clone()).collect();
    let joined = rows.join(" | ");
    assert!(joined.contains("VERY-OLD-PREFIX"), "the older prefix must survive: {joined}");
    assert!(joined.contains("replayed line one and two"), "the tail must land: {joined}");
    assert_eq!(
        joined.matches("replayed line one").count(),
        1,
        "the replayed row must not duplicate: {joined}"
    );
    let prefix = rows.iter().position(|t| t.contains("VERY-OLD-PREFIX"));
    let tail = rows.iter().position(|t| t.contains("replayed line one"));
    assert!(prefix < tail, "the older prefix must precede the tail: {joined}");

    core.disconnect();
    let _ = std::fs::remove_dir_all(&data_dir);
}

// ---------------------------------------------------------------------------
// Session switch: opening A, then B, then back to A must reuse A's cache and
// merge a bounded tail — re-streaming the whole chat from the top is the bug.
// ---------------------------------------------------------------------------

#[test]
fn reopening_a_cached_session_merges_a_tail_instead_of_full_replay() {
    let data_dir =
        std::env::temp_dir().join(format!("grouse-switch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);

    let (port_tx, port_rx) = mpsc::channel();
    let server = FakeServer::spawn(port_tx);
    let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake server port");

    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(
        Box::new(RecordingListener::new(ev_tx)),
        data_dir.to_string_lossy().to_string(),
    );
    core.connect(grouse_core::ServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        secret_key: "test-secret".to_string(),
        use_tls: false,
        accept_invalid_certs: false,
        ca_cert_pem: None,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
        initial_project_id: None,
    });
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");

    // Open A (no cache yet → the first load may be a full replay); its transcript
    // is cached on Ready.
    core.open_session("sess-a".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "A ready");
    assert!(
        data_dir.join("sess-a.json").exists(),
        "opening a session must persist its transcript for the next open"
    );

    // Away to B, then back to A.
    core.open_session("sess-b".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "B ready");

    core.open_session("sess-a".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "A again ready");

    // The reopen asked for the bounded tail, NOT a full replay.
    let last = server
        .frames_for("session/load")
        .last()
        .cloned()
        .expect("a session/load for the reopen");
    assert_eq!(
        last.pointer("/params/_meta/replayTail").and_then(Value::as_u64),
        Some(100),
        "reopening a cached session must merge a bounded tail, not replay from the top: {last:?}"
    );
    // And it still shows the conversation.
    let text: String = core
        .rich_transcript()
        .iter()
        .map(|i| i.text.clone())
        .collect::<Vec<_>>()
        .join("");
    assert!(text.contains("replayed line one and two"), "reopen shows content: {text}");

    core.disconnect();
    let _ = std::fs::remove_dir_all(&data_dir);
}

// ---------------------------------------------------------------------------
// Cold start: the painted cache belongs to the session it came from, NOT to the
// throwaway session the first connect creates on the way to resuming it.
// ---------------------------------------------------------------------------

#[test]
fn cold_start_paint_is_not_filed_under_the_transient_session() {
    // Own cache dir, passed to Core explicitly — see the note in
    // stale_cache_is_painted_then_replaced_not_appended.
    let cache_dir =
        std::env::temp_dir().join(format!("grouse-owner-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache_dir);
    let cache = grouse_core::cache::CacheStore::new(cache_dir.clone());
    let seeded = vec![item("m-1", grouse_core::ItemKind::Agent, "OWNED-BY-SESS-R")];
    assert!(cache.save_transcript("sess-r", &seeded, "2026-01-01T00:00:00.000Z"));

    let (port_tx, port_rx) = mpsc::channel();
    let _server = FakeServer::spawn(port_tx);
    let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake server port");

    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(
        Box::new(RecordingListener::new(ev_tx)),
        cache_dir.to_string_lossy().to_string(),
    );

    // The cold-start sequence the UI runs: paint the last chat, THEN connect.
    // connect() creates a throwaway session before the real resume.
    core.load_cached_transcript("sess-r".to_string());
    core.connect(grouse_core::ServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        secret_key: "test-secret".to_string(),
        use_tls: false,
        accept_invalid_certs: false,
        ca_cert_pem: None,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
        initial_project_id: None,
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
    let _ = std::fs::remove_dir_all(&cache_dir);
}

// ---------------------------------------------------------------------------
// Run summaries (issue #13): a turn's tool run is labelled by the model
// ---------------------------------------------------------------------------

#[test]
fn a_tool_run_gets_a_model_written_label() {
    let _cache_guard = CACHE_TEST_LOCK.lock();
    let data_dir = std::env::temp_dir().join(format!("grouse-sum-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    std::env::set_var("XDG_DATA_HOME", &data_dir);

    let (port_tx, port_rx) = mpsc::channel();
    let server = FakeServer::spawn(port_tx);
    server.tool_turn.store(true, std::sync::atomic::Ordering::SeqCst);
    let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake server port");

    let (ev_tx, _ev_rx) = mpsc::channel();
    let core = Core::new(Box::new(RecordingListener::new(ev_tx)), String::new());
    core.connect(grouse_core::ServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        secret_key: "test-secret".to_string(),
        use_tls: false,
        accept_invalid_certs: false,
        ca_cert_pem: None,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
        initial_project_id: None,
    });
    core.send_prompt(
        Prompt { blocks: vec![PromptBlock::Text { text: "hi".to_string() }] },
        None,
    );

    // The run's provisional label comes from the thinking it absorbed; the
    // model's rewrite lands after the turn ends, on the hidden session.
    let mut label = String::new();
    let mut thought = String::new();
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(100));
        let run = core.rich_transcript().into_iter().find(|i| {
            matches!(i.kind, grouse_core::ItemKind::Tool | grouse_core::ItemKind::ToolGroup)
        });
        if let Some(run) = run {
            label = run.summary.clone();
            thought = run.thought.clone();
        }
        if label == "Look up the charging issue" {
            break;
        }
    }
    assert_eq!(label, "Look up the charging issue", "the model's rewrite must land");
    // The absorbed thinking is what the expanded chip still shows.
    assert_eq!(thought, "Let me look up the charging issue.");

    // The summarizer session is a HIDDEN one: asked for with `_meta.hidden`, and
    // without `_meta.client` (so it is not a user session either).
    let new_frames = server.frames_for("session/new");
    assert!(
        new_frames
            .iter()
            .any(|f| f.pointer("/params/_meta/hidden").and_then(Value::as_bool) == Some(true)),
        "the summarizer must ask for a hidden session: {new_frames:?}"
    );

    // Its context must not grow turn on turn: the core empties it after use.
    let truncations = server.frames_for("_goose/unstable/session/conversation/truncate");
    assert_eq!(truncations.len(), 1, "one truncate per summarization");
    assert_eq!(
        truncations[0].pointer("/params/truncateFrom").and_then(Value::as_i64),
        Some(0)
    );

    core.disconnect();
    let _ = std::fs::remove_dir_all(&data_dir);
}

// ---------------------------------------------------------------------------
// A prompt is NOT echoed on the wire (goose sends no `user_message_chunk` for a
// prompt we sent — it next crosses the wire in a `session/load` replay). So the
// core keeps its own copy. Without it the transcript, and the cache saved from
// it, holds no record of what was asked: a reopen that trusts the cache shows
// the replies with the questions missing.
// ---------------------------------------------------------------------------

#[test]
fn a_prompt_survives_a_reopen_because_the_core_echoes_it() {
    let data_dir = std::env::temp_dir().join(format!("grouse-prompt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);

    let (port_tx, port_rx) = mpsc::channel();
    let server = FakeServer::spawn(port_tx);
    // The server's run-end touch, so the freshness stamp moves exactly as it does
    // against a live serve.
    server.post_turn_touch.store(true, std::sync::atomic::Ordering::SeqCst);
    let port = port_rx.recv_timeout(Duration::from_secs(5)).expect("fake server port");

    let config = grouse_core::ServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        secret_key: "test-secret".to_string(),
        use_tls: false,
        accept_invalid_certs: false,
        ca_cert_pem: None,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
        initial_recipe_id: None,
        initial_project_id: None,
    };

    // --- Ask something and let the turn finish. ---
    let (ev_tx, ev_rx) = mpsc::channel();
    let core = Core::new(
        Box::new(RecordingListener::new(ev_tx)),
        data_dir.to_string_lossy().to_string(),
    );
    core.connect(config.clone());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "ready");
    core.send_prompt(
        Prompt { blocks: vec![PromptBlock::Text { text: "what did I ask?".to_string() }] },
        Some(SendExpect { session_id: "sess-e2e".to_string() }),
    );
    wait_for(&ev_rx, |ev| matches!(ev, Ev::RunEnded(_, _)), "RunEnded");

    // No user chunk ever arrived (the fake no longer invents one), so this row is
    // the CORE's echo — the only record of the question.
    let asked = core
        .rich_transcript()
        .into_iter()
        .find(|i| i.kind == grouse_core::ItemKind::User)
        .expect("the core echoes the prompt it sent");
    assert_eq!(asked.text, "what did I ask?");

    // And the cache SAVED FROM IT records the question. This is the claim that
    // matters: a reopen that trusts this file must still show what was asked.
    //
    // POLLED, not read once: the save runs after the `session/info` freshness
    // probe (see `probe_stamp_and_save`), so the write lands AFTER RunEnded and a
    // single read can lose that race — it did, on CI. Same bounded wait the other
    // cache assertions in this file use.
    let cache_path = data_dir.join("sess-e2e.json");
    let deadline = Instant::now() + Duration::from_secs(5);
    let cache_file = loop {
        if let Ok(body) = std::fs::read_to_string(&cache_path) {
            break body;
        }
        assert!(
            Instant::now() < deadline,
            "the turn's transcript was never cached at {cache_path:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        cache_file.contains("what did I ask?"),
        "the saved transcript must record the question: {cache_file}"
    );
    core.disconnect();

    // --- Reopen, as a fresh launch would. ---
    let (ev_tx2, ev_rx2) = mpsc::channel();
    let core2 = Core::new(
        Box::new(RecordingListener::new(ev_tx2)),
        data_dir.to_string_lossy().to_string(),
    );
    core2.connect_resume(config, "sess-e2e".to_string());
    wait_for(&ev_rx2, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "ready again");
    std::thread::sleep(Duration::from_millis(600)); // let the paint/replay settle

    let after: Vec<String> = core2
        .rich_transcript()
        .into_iter()
        .map(|i| format!("{:?}:{}", i.kind, i.text))
        .collect();
    assert!(
        after.iter().any(|r| r.contains("what did I ask?")),
        "the question must survive a reopen: {after:?}"
    );

    core2.disconnect();
    let _ = std::fs::remove_dir_all(&data_dir);
}
