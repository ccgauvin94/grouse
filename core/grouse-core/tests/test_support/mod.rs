// SPDX-License-Identifier: AGPL-3.0-or-later

//! Shared fixtures for the wire suite: the event recorder, a unique cache dir
//! per test, the strict fake-goose protocol oracle, and the small macros that
//! make a new regression a handful of lines.
//!
//! Only `spine.rs` includes this module. Nothing here mutates process-global
//! state: `Core::new` is always handed the harness's own absolute cache dir.
//! A listener addition changes `Ev`/`RecordingListener` here, not every test.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use async_tungstenite::tungstenite::Message as WsMessage;
use futures::stream::StreamExt;
use grouse_core::{
    ConfigOption, ConnectionStatus, Core, CoreListener, PermissionRequest, ProjectSummary,
    SessionSummary, TranscriptOp,
};
pub(crate) use parking_lot::Mutex;
use serde_json::{Value, json};

// Vec<String> images on Item outsize some other variant enough to trip
// clippy's variant-size lint; boxing over the whole recording surface is not
// worth the churn for a test-only type.
#[allow(clippy::large_enum_variant)]
#[allow(dead_code)] // full listener surface; tests match a subset
pub(crate) enum Ev {
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

pub(crate) struct RecordingListener {
    tx: Sender<Ev>,
}

impl RecordingListener {
    pub(crate) fn new(tx: Sender<Ev>) -> Self {
        Self { tx }
    }
}

impl CoreListener for RecordingListener {
    fn on_session_opened(&self, _session_id: String) {}
    fn on_advisor_report(&self, _: String, _: String) {}

    fn on_sensitivity(&self, _data: String) {}
    fn on_thread_timeline(&self, _label: String, _rows: Vec<grouse_core::TimelineRow>) {}
    fn on_thread_ready(&self, _: String, _: String) {}
    fn on_file(&self, _: grouse_core::FileEvent) {}
    fn on_replay_progress(&self, _: String, _: i64, _: i64, _: bool) {}

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
pub(crate) fn item(id: &str, kind: grouse_core::ItemKind, text: &str) -> grouse_core::Item {
    grouse_core::Item {
        created: 0,
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
        files: Vec::new(),
        images: Vec::new(),
        message_id: String::new(),
    }
}

/// Wait for an event matching `pred`, skipping unrelated traffic.
pub(crate) fn wait_for<F: Fn(&Ev) -> bool>(rx: &Receiver<Ev>, pred: F, what: &str) -> Ev {
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

/// Poll the recorded wire frames for `count` requests of `method`.
/// This is an EVENT wait (a frame arrived), never a sleep past a timer.
pub(crate) fn wait_for_frames(server: &FakeServer, method: &str, count: usize, what: &str) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let frames = server.frames_for(method);
        if frames.len() >= count {
            return frames;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {count} {method} frames ({what})");
}

// ---------------------------------------------------------------------------
// Per-test harness: a private cache dir, a recorder, and a strict fake server
// ---------------------------------------------------------------------------

static HARNESS_SEQ: AtomicU64 = AtomicU64::new(0);

/// RAII cache directory. Removed on drop even when a test panics, so a failed
/// wire test leaves no state behind for the next one.
struct UniqueDir(PathBuf);

impl Drop for UniqueDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) struct SpineHarness {
    _dir: UniqueDir,
    /// Protocol-oracle findings from the fake server. Any entry fails the test
    /// at harness drop: a misspelled method/param can no longer be answered
    /// with a harmless `{}` and pass unnoticed.
    violations: Arc<Mutex<Vec<String>>>,
}

impl SpineHarness {
    /// The directory handed to `Core::new` (and to cache-seeding tests).
    pub(crate) fn cache_dir(&self) -> PathBuf {
        self._dir.0.join("grouse")
    }

    pub(crate) fn cache_store(&self) -> grouse_core::cache::CacheStore {
        grouse_core::cache::CacheStore::new(self.cache_dir())
    }

    /// A fresh event recorder. Call once per Core; the receiver is returned to
    /// the test.
    pub(crate) fn listener(&self) -> (Sender<Ev>, Receiver<Ev>) {
        mpsc::channel()
    }

    /// Every Core in the suite is built with this harness's private dir —
    /// never with `String::new()` (the process-global XDG default).
    pub(crate) fn core(&self, tx: Sender<Ev>) -> Arc<Core> {
        Core::new(
            Box::new(RecordingListener::new(tx)),
            self.cache_dir().to_string_lossy().into_owned(),
        )
    }

    pub(crate) fn server(&self) -> (Arc<FakeServer>, u16) {
        let (port_tx, port_rx) = mpsc::channel();
        let server = FakeServer::spawn(port_tx, self.violations.clone());
        let port = port_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("fake server port");
        (server, port)
    }

    pub(crate) fn violations(&self) -> Vec<String> {
        self.violations.lock().clone()
    }
}

impl Drop for SpineHarness {
    fn drop(&mut self) {
        // Don't double-panic while another assertion is unwinding.
        if !std::thread::panicking() {
            let found = self.violations();
            assert!(
                found.is_empty(),
                "the fake protocol oracle saw unsupported/typo RPCs: {found:#?}"
            );
        }
    }
}

pub(crate) fn spine_harness() -> SpineHarness {
    let seq = HARNESS_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("grouse-spine-{}-{seq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create private harness dir");
    SpineHarness {
        _dir: UniqueDir(dir),
        violations: Arc::new(Mutex::new(Vec::new())),
    }
}

/// The default ServerConfig every wire test starts from. Override the two
/// recipe fields with struct update syntax when a case needs them.
pub(crate) fn server_config(port: u16) -> grouse_core::ServerConfig {
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
    }
}

/// One line to stand up a wire: recorder → Core with its own cache dir →
/// fake server and port. Returns `(core, server, port, event_rx)`.
macro_rules! wire {
    ($h:expr) => {{
        let (ev_tx, ev_rx) = $h.listener();
        let core = $h.core(ev_tx);
        let (server, port) = $h.server();
        (core, server, port, ev_rx)
    }};
}
pub(crate) use wire;

// ---------------------------------------------------------------------------
// Fake goosed server (in-process, scripted per method)
// ---------------------------------------------------------------------------

/// The scripted ACP server. Lives on its own tokio runtime thread; the test
/// drives it only through the port + the recorded frames.
pub(crate) struct FakeServer {
    /// Unsupported/typo requests seen by the strict protocol oracle. Any
    /// entry fails the owning SpineHarness at drop, even if the core happened
    /// to tolerate the unanswered request.
    violations: Arc<Mutex<Vec<String>>>,
    /// The stamp returned by `_goose/unstable/session/info`; a case may set it
    /// equal to its listed stamp to exercise the fresh-cache fast path.
    pub(crate) info_stamp: Arc<Mutex<String>>,
    /// Every JSON-RPC frame the client sent, in arrival order.
    pub(crate) frames: Arc<Mutex<Vec<Value>>>,
    /// The `X-Secret-Key` upgrade header, if the handshake carried one.
    pub(crate) secret_header: Arc<Mutex<Option<String>>>,
    /// When set, the fake pushes `session_info_update` (with an activeRunId)
    /// + `available_commands_update` notifications right after session/new.
    pub(crate) notify: Arc<std::sync::atomic::AtomicBool>,
    /// When set, `session/prompt` is recorded and left UNANSWERED, so a test can
    /// observe the core sending a SECOND chat's prompt while the first turn is
    /// still in flight (per-session concurrency).
    pub(crate) hold_prompts: Arc<std::sync::atomic::AtomicBool>,
    /// When set, `session/prompt` emits a run-end `session_info_update` (no
    /// activeRunId) before answering — the server's own post-turn touch that
    /// must not be mistaken for a remote change.
    pub(crate) post_turn_touch: Arc<std::sync::atomic::AtomicBool>,
    /// Fired by a test to close the live WebSocket(s), so the core's reconnect
    /// path (and its `session/load` replay) actually runs.
    pub(crate) drop_now: Arc<tokio::sync::Notify>,
    /// When set, the `session/load` replay stamps its rows with a DIFFERENT
    /// message id — modelling goose, which re-assigns ids on every replay, so a
    /// replay's rows do not match the live rows they restate.
    pub(crate) replay_alt_id: Arc<std::sync::atomic::AtomicBool>,
    /// The live connection's out-of-band push half, so a test can inject a
    /// notification (e.g. a remote `session_info_update` that makes the on-disk
    /// cache stale) without a request to hang it on.
    pub(crate) push: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Value>>>>,
    /// When set, `session/prompt` on the MAIN session also emits a thinking
    /// chunk and a tool call — a turn with a run for the summarizer to label.
    pub(crate) tool_turn: Arc<std::sync::atomic::AtomicBool>,
    /// The text of the last prompt sent. `session/load` replays it as a
    /// `user_message_chunk` — the ONLY place a prompt reaches the client, since
    /// goose never echoes it live.
    pub(crate) last_prompt: Arc<Mutex<String>>,
    /// When set, `session/load` answers with a JSON-RPC error (a stale/archived
    /// session) instead of replaying. Drives the "an open failure must report,
    /// never silently become a new session" coverage (grouse#19).
    pub(crate) fail_load: Arc<std::sync::atomic::AtomicBool>,
    /// When set, `session/load` streams its replay rows and then WAITS for
    /// `release_load` before replying — the window in which the core must
    /// ingest the replay silently (grouse#18).
    pub(crate) gate_load: Arc<std::sync::atomic::AtomicBool>,
    /// A synthetic conversation of this many messages, served by `session/load`
    /// with `_meta.replayTail` honoured and `_meta.replaySkipped` reported —
    /// the two things reverse loading is built on (docs/TRANSCRIPT_MODEL.md).
    /// `0` = off (the small canned replay below).
    pub(crate) history: Arc<Mutex<usize>>,
    pub(crate) release_load: Arc<tokio::sync::Notify>,
    /// When set, `session/list` answers in TWO pages with a `nextCursor` between
    /// them, the way goose does past 50 sessions — the shape a client that never
    /// followed the cursor silently truncated (2026-11-08: the Media project
    /// showed one chat where it used to show five).
    pub(crate) page_lists: Arc<std::sync::atomic::AtomicBool>,
    /// Hold the FIRST `initialize` until `release_initialize` fires, so a test
    /// can start a handshake, supersede it with a newer connect, and then let
    /// the stale handshake continue.
    pub(crate) hold_first_initialize: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) release_initialize: Arc<tokio::sync::Notify>,
    /// Add a session whose listed message time matches a cached stamp — the
    /// fresh-cache baseline a verification probe must be able to refute.
    pub(crate) fresh_session_row: Arc<std::sync::atomic::AtomicBool>,
}

impl FakeServer {
    /// Bind on 127.0.0.1:0 and spawn the accept+serve loop. Returns when the
    /// listener is bound (the port is delivered through `port_tx`).
    pub(crate) fn spawn(
        port_tx: mpsc::Sender<u16>,
        violations: Arc<Mutex<Vec<String>>>,
    ) -> Arc<Self> {
        let server = Arc::new(Self {
            violations,
            frames: Arc::new(Mutex::new(Vec::new())),
            secret_header: Arc::new(Mutex::new(None)),
            notify: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hold_prompts: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            post_turn_touch: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            drop_now: Arc::new(tokio::sync::Notify::new()),
            replay_alt_id: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            info_stamp: Arc::new(Mutex::new("2026-08-12T13:00:00.000Z".to_string())),
            push: Arc::new(Mutex::new(None)),
            tool_turn: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_prompt: Arc::new(Mutex::new(String::new())),
            fail_load: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            gate_load: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            history: Arc::new(Mutex::new(0)),
            release_load: Arc::new(tokio::sync::Notify::new()),
            page_lists: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hold_first_initialize: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            release_initialize: Arc::new(tokio::sync::Notify::new()),
            fresh_session_row: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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

    pub(crate) fn frames_for(&self, method: &str) -> Vec<Value> {
        self.frames
            .lock()
            .iter()
            .filter(|frame| frame.get("method").and_then(Value::as_str) == Some(method))
            .cloned()
            .collect()
    }

    /// Close every live connection, so the core's reconnect path runs.
    pub(crate) fn drop_connections(&self) {
        self.drop_now.notify_one();
    }

    /// Push a notification to the live connection, out of band.
    pub(crate) fn push_notification(&self, value: Value) {
        if let Some(tx) = self.push.lock().as_ref() {
            let _ = tx.send(value);
        }
    }
}

/// The strict method/param table. Anything else is a PROTOCOL FAILURE, not
/// an innocuous empty reply: a misspelled method used to get `{}` and pass.
pub(crate) fn check_request(method: &str, params: &Value) -> Result<(), String> {
    let required: &[&str] = match method {
        "initialize" => &["protocolVersion", "clientCapabilities"],
        "session/list" => &["_meta"],
        "session/new" => &["cwd", "mcpServers", "_meta"],
        "session/load" => &["sessionId", "cwd", "mcpServers"],
        "session/prompt" => &["sessionId", "prompt"],
        "session/set_config_option" => &["sessionId", "configId", "value"],
        "session/set_mode" => &["sessionId", "modeId"],
        "_goose/unstable/session/info" => &["sessionId"],
        "_goose/unstable/session/conversation/truncate" => &["sessionId", "truncateFrom"],
        other => return Err(format!("unknown RPC method {other:?} — script it in the fake")),
    };
    for key in required {
        if params.get(*key).is_none() {
            return Err(format!(
                "{method}: missing required param {key:?} (a misspelling is silently dropped by goose); params={params}"
            ));
        }
    }
    Ok(())
}

pub(crate) fn record_violation(
    violations: &Mutex<Vec<String>>,
    method: &str,
    params: &Value,
    reason: &str,
) {
    violations
        .lock()
        .push(format!("{method} {params}: {reason}"));
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

        // Strict oracle: record the finding and leave the request UNANSWERED.
        // A detached server task cannot fail the test by panicking, so the
        // harness asserts the violation list at drop.
        if let Err(reason) = check_request(&method, &params) {
            record_violation(&server.violations, &method, &params, &reason);
            continue;
        }

        let result = match method.as_str() {
            "initialize" => {
                if server
                    .hold_first_initialize
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
                {
                    server.release_initialize.notified().await;
                }
                json!({ "protocolVersion": 1 })
            }
            "session/list" if server.fresh_session_row.load(std::sync::atomic::Ordering::SeqCst) => {
                json!({
                    "sessions": [{
                        "sessionId": "sess-fresh",
                        "title": "Fresh baseline",
                        "cwd": "/tmp",
                        // Cache stamp below matches this value byte-for-byte.
                        "_meta": {
                            "lastMessageAt": "2026-08-12T06:00:00Z",
                            "messageCount": 2
                        }
                    }]
                })
            }
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
            "session/list" => {
                if server.page_lists.load(std::sync::atomic::Ordering::SeqCst) {
                    // Page 2 is asked for with the cursor page 1 handed out; a client
                    // that ignores `nextCursor` never sends that request at all.
                    let cursor = params.get("cursor").and_then(Value::as_str).unwrap_or("");
                    if cursor == "page-2" {
                        json!({
                            "sessions": [{
                                "sessionId": "sess-old",
                                "title": "Movies",
                                "updatedAt": "2026-01-01T00:00:00.000Z",
                                "cwd": "/tmp",
                                "_meta": { "messageCount": 4 }
                            }]
                        })
                    } else {
                        json!({
                            "sessions": [{
                                "sessionId": "sess-e2e",
                                "title": "E2E Chat",
                                "updatedAt": "2026-08-12T00:00:00.000Z",
                                "cwd": "/tmp",
                                "_meta": { "messageCount": 1, "lastMessageSnippet": "hi from the list" }
                            }],
                            "nextCursor": "page-2"
                        })
                    }
                } else {
                    json!({
                        "sessions": [{
                            "sessionId": "sess-e2e",
                            "title": "E2E Chat",
                            "updatedAt": "2026-08-12T00:00:00.000Z",
                            "cwd": "/tmp",
                            "_meta": { "messageCount": 1, "lastMessageSnippet": "hi from the list" }
                        }]
                    })
                }
            }
            "session/load" => {
                let session = params
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let history = *server.history.lock();
                if history > 0 {
                    // A synthetic conversation served the way goose serves one:
                    // only the trailing `_meta.replayTail` messages are replayed,
                    // and the count it withheld comes back as
                    // `_meta.replaySkipped` (verified against a live serve).
                    let tail = params
                        .get("_meta")
                        .and_then(|meta| meta.get("replayTail"))
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as usize;
                    let skip = history.saturating_sub(tail);
                    for i in skip..history {
                        let frame = json!({
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": {
                                "sessionId": session,
                                "update": {
                                    "sessionUpdate": "agent_message_chunk",
                                    "content": { "type": "text", "text": format!("history row {i}") },
                                    "messageId": format!("h{i}")
                                }
                            }
                        });
                        let _ = tx.send(WsMessage::Text(frame.to_string().into())).await;
                    }
                    let mut reply = json!({
                        "sessionId": session,
                        "configOptions": [
                            { "id": "provider", "name": "Provider", "currentValue": "openai" }
                        ]
                    });
                    if skip > 0 {
                        reply["_meta"] = json!({ "replaySkipped": skip });
                    }
                    reply
                } else {
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
                *server.last_prompt.lock() = prompt_text.clone();
                for (tag, text, message_id) in [
                    // The reply is derived from the prompt on the wire: a
                    // misspelled `prompt` param fails the oracle above rather
                    // than producing a canned "hello" answer.
                    (
                        "agent_message_chunk",
                        format!("{prompt_text} from fake goose"),
                        "m-1",
                    ),
                    ("agent_message_chunk", " and more".to_string(), "m-1"),
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
                "sessionId": params.get("sessionId").cloned().unwrap_or(Value::Null),
                "session": {
                    "updatedAt": server.info_stamp.lock().clone(),
                    "_meta": { "messageCount": 3 }
                }
            }),
            // goose's real shape for a select set: the CURRENT list with the
            // chosen value, and (as on the typed notification) no per-option
            // choices for the changed entry. Derived from the REQUEST, so a
            // typo in configId/value cannot hide behind a canned answer.
            "session/set_config_option" => {
                let id = params.get("configId").and_then(Value::as_str).unwrap_or("");
                let value = params.get("value").and_then(Value::as_str).unwrap_or("");
                json!({
                    "configOptions": [
                        { "id": id, "name": id, "currentValue": value }
                    ]
                })
            }
            "session/set_mode" => json!({
                "modeId": params.get("modeId").cloned().unwrap_or(Value::Null)
            }),
            "_goose/unstable/session/conversation/truncate" => json!({}),
            // The oracle above already rejected this; keep the arm explicit so
            // a future refactor cannot quietly reinstate `{}` for typos.
            other => {
                record_violation(
                    &server.violations,
                    other,
                    &params,
                    "unhandled method (the fake must script it)",
                );
                continue;
            }
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

