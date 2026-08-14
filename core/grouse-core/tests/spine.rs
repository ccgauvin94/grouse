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
}

impl FakeServer {
    /// Bind on 127.0.0.1:0 and spawn the accept+serve loop. Returns when the
    /// listener is bound (the port is delivered through `port_tx`).
    fn spawn(port_tx: mpsc::Sender<u16>) -> Arc<Self> {
        let server = Arc::new(Self {
            frames: Arc::new(Mutex::new(Vec::new())),
            secret_header: Arc::new(Mutex::new(None)),
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
                let (stream, _peer) = listener.accept().await.expect("fake server accept");
                serve_connection(for_thread, stream).await;
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
            "session/new" => json!({
                "sessionId": "sess-e2e",
                "configOptions": [
                    { "id": "provider", "name": "Provider", "currentValue": "openai" }
                ]
            }),
            "session/list" => json!({
                "sessions": [{
                    "sessionId": "sess-e2e",
                    "title": "E2E Chat",
                    "updatedAt": "2026-08-12T00:00:00.000Z",
                    "cwd": "/tmp",
                    "_meta": { "messageCount": 1, "lastMessageSnippet": "hi from the list" }
                }]
            }),
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
            _ => json!({}),
        };
        let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        let _ = tx.send(WsMessage::Text(reply.to_string().into())).await;
    }
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
    let core = Core::new(Box::new(RecordingListener::new(ev_tx)));

    // connect() blocks (bounded) until the handshake completes.
    core.connect(grouse_core::ServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        secret_key: "test-secret".to_string(),
        use_tls: false,
        cwd: "/tmp".to_string(),
        auto_connect: false,
        client_id: "grouse-core-test".to_string(),
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
