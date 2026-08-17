//! The peer registry: parallel roam connections in browse mode (CONTRACT §6,
//! INTERNAL.md seam 4).
//!
//! Each [`RoamPeer`] is an *independent* ACP client — its own SDK `Client` and
//! its own transport — over an authenticated iroh [`RoamStream`]
//! (`grouse-roam-core`). This is what makes the peer *parallel*: it never
//! shares the spine's WebSocket, so the main connection and any number of
//! peers stay live at once.
//!
//! The roam transport is a byte stream, not a WebSocket. [`RoamTransport`]
//! adapts it to the SDK [`Channel`] with the same newline framing the SDK's
//! own `ByteStreams` component uses: one compact JSON-RPC message per
//! `\n`-terminated line ([`RoamCodec`], the desktop's `RoamFrameCodec`).
//! Inbound lines are pushed into the channel exactly like
//! [`crate::transport`]'s `drive_ws`; outbound frames are serialized and
//! newline-terminated. Because `RoamStream::read`/`write` are *blocking* (they
//! park on std mpsc receivers inside grouse-roam-core), every I/O hop runs on
//! tokio's blocking pool; [`RoamPeer::close`] unblocks a parked reader via
//! `RoamStream::cancel()`.
//!
//! Session notifications are dispatched the same way the spine dispatches
//! them (`SessionNotification` → `CoreListener` events). The peer forwards
//! chat-scoped events (`on_stream`, `on_transcript`, `on_session_touched`,
//! `on_permission_request`) only while it owns the active session — the
//! `is_active` gate supplied by the spine — and always emits the peer-scoped
//! events `on_roam_peer_status` / `on_roam_sessions` (manager.cpp
//! `wireClient`'s `active()` gate, ported).
//!
//! Peer transcripts are peer-owned (a `Vec<Message>` here) and are never
//! cached: the cache is keyed by session id, which could collide across
//! machines, so the desktop replays peer sessions every time.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, mpsc as std_mpsc};

use parking_lot::Mutex;

use agent_client_protocol::schema::v1::{
    ClientCapabilities, ElicitationCapabilities, ElicitationFormCapabilities,
    FileSystemCapabilities, Implementation, InitializeRequest, ListSessionsRequest,
    ListSessionsResponse, PermissionOptionId,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionInfo, SessionNotification, SessionUpdate,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{
    Agent, Channel, Client, ConnectTo, Error as AcpError, Responder, TransportFrame,
    UntypedMessage, on_receive_notification, on_receive_request,
};
use futures::future::{select, BoxFuture};
use futures::stream::StreamExt;
use futures::pin_mut;
use grouse_roam_core::RoamStream;
use serde_json::{Map, Value, json};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::runtime::Runtime;
use tokio::sync::mpsc as tokio_mpsc;

use crate::spine::RpcConn;
use crate::{
    ConnectionStatus, CoreListener, Message, PermissionOutcome, SessionSummary, StreamEvent,
    ToolCallKind, TranscriptEvent,
};

// ---------------------------------------------------------------------------
// Core runtime (shared with the spine)
// ---------------------------------------------------------------------------

/// The core's single tokio runtime. All peer connects, the ACP request/reply
/// dispatch, and the `CoreListener` callbacks run on it. Created on first use
/// so the crate works in tests that never touch the network.
pub(crate) fn runtime() -> &'static Runtime {
    static RT: LazyLock<Runtime> =
        LazyLock::new(|| Runtime::new().expect("grouse-core tokio runtime"));
    &RT
}

// ---------------------------------------------------------------------------
// ACP byte-stream framing (RoamCodec)
// ---------------------------------------------------------------------------

/// ACP byte-stream framing over the roam transport: one JSON-RPC message per
/// newline-terminated line — the same codec the SDK's `ByteStreams` component
/// (and goose's stdio transport) uses. JSON is serialized compactly, so a
/// message can never contain a raw newline.
///
/// Chunk-safe: partial frames stay buffered until their newline arrives; CRLF
/// is tolerated (`BufReader::lines` strips `\r`). Mirrors the desktop's
/// `RoamFrameCodec` and the Android client's codec of the same name.
#[derive(Debug, Default)]
pub struct RoamCodec {
    buf: Vec<u8>,
}

impl RoamCodec {
    /// A fresh codec with an empty partial-frame buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw stream bytes; returns every complete frame (newline removed).
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        let mut frames = Vec::new();
        for &byte in chunk {
            match byte {
                b'\n' => {
                    frames.push(
                        String::from_utf8_lossy(&std::mem::take(&mut self.buf)).into_owned(),
                    );
                }
                b'\r' => {} // CRLF tolerance (BufReader::lines strips \r)
                _ => self.buf.push(byte),
            }
        }
        frames
    }

    /// One outbound frame: the JSON message plus its terminating newline.
    pub fn encode(text: &str) -> Vec<u8> {
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(b'\n');
        bytes
    }
}

// ---------------------------------------------------------------------------
// ACP transport over a roam stream
// ---------------------------------------------------------------------------

/// ACP transport over an authenticated iroh roam stream (CONTRACT §2: "roam
/// byte stream"). The stream is pre-dialed and authorized by the caller — this
/// transport only pumps newline-framed ACP frames both ways, feeding the SDK
/// [`Channel`] exactly like `WsTransport::drive_ws` does for the WebSocket.
pub struct RoamTransport {
    stream: Arc<RoamStream>,
}

impl RoamTransport {
    /// Wrap an already-connected roam stream.
    pub fn new(stream: Arc<RoamStream>) -> Self {
        Self { stream }
    }

    /// Drive the stream against the SDK channel. `RoamStream::read`/`write`
    /// are blocking, so each I/O op hops onto tokio's blocking pool; a parked
    /// reader is unblocked by `RoamStream::cancel()` (called by close paths).
    async fn run(self, channel: Channel) -> Result<(), AcpError> {
        let Channel {
            rx: mut outgoing,
            tx: incoming,
        } = channel;
        let stream = self.stream;

        let writer = {
            let stream = stream.clone();
            async move {
                while let Some(frame) = outgoing.next().await {
                    let text = frame.to_json().map_err(|error| {
                        AcpError::internal_error().data(format!("roam serialize: {error}"))
                    })?;
                    let bytes = RoamCodec::encode(&text);
                    let s = stream.clone();
                    tokio::task::spawn_blocking(move || s.write(bytes))
                        .await
                        .map_err(|error| {
                            AcpError::internal_error().data(format!("roam write task: {error}"))
                        })?
                        .map_err(|error| {
                            AcpError::internal_error().data(format!("roam write: {error}"))
                        })?;
                }
                // Outbound channel closed: FIN to the peer.
                let s = stream.clone();
                let _ = tokio::task::spawn_blocking(move || s.shutdown()).await;
                Ok::<(), AcpError>(())
            }
        };

        let reader = async move {
            let mut codec = RoamCodec::new();
            let mut discard_incoming = false;
            loop {
                let s = stream.clone();
                let chunk = tokio::task::spawn_blocking(move || s.read(16 * 1024))
                    .await
                    .map_err(|error| {
                        AcpError::internal_error().data(format!("roam read task: {error}"))
                    })?
                    .map_err(|error| {
                        AcpError::internal_error().data(format!("roam read: {error}"))
                    })?;
                if chunk.is_empty() {
                    // EOF from the peer.
                    return Err(AcpError::internal_error().data("roam stream ended"));
                }
                if discard_incoming {
                    continue;
                }
                for line in codec.feed(&chunk) {
                    let frame = TransportFrame::parse_json(&line);
                    if incoming.unbounded_send(frame).is_err() {
                        // The client channel closed; drain the stream until it
                        // ends so graceful shutdown still works.
                        discard_incoming = true;
                    }
                }
            }
        };

        pin_mut!(writer, reader);
        match select(writer, reader).await {
            futures::future::Either::Left((result, _))
            | futures::future::Either::Right((result, _)) => result,
        }
    }
}

impl ConnectTo<Client> for RoamTransport {
    async fn connect_to(self, client: impl ConnectTo<Agent>) -> Result<(), AcpError> {
        let (channel, transport) = ConnectTo::<Client>::into_channel_and_future(self);
        let shutdown_tx = channel.tx.clone();
        match select(
            std::pin::pin!(client.connect_to(channel)),
            std::pin::pin!(transport),
        )
        .await
        {
            futures::future::Either::Left((result, transport)) => {
                result?;
                // Reject sends from escaped client handles while preserving
                // messages already accepted into the channel, then let the
                // physical transport finish those messages.
                shutdown_tx.close_channel();
                transport.await
            }
            futures::future::Either::Right((result, _)) => result,
        }
    }

    fn into_channel_and_future(self) -> (Channel, BoxFuture<'static, Result<(), AcpError>>) {
        let (caller, transport) = Channel::duplex();
        (caller, Box::pin(self.run(transport)))
    }
}

// ---------------------------------------------------------------------------
// The peer registry entry
// ---------------------------------------------------------------------------

/// Commands the spine's intents enqueue for the peer's connection task.
#[derive(Debug)]
enum PeerCommand {
    OpenSession { session_id: String, cwd: String },
    NewSession { cwd: String },
    Close,
}

/// One active roam endpoint: a direct iroh peer running its own goose (the
/// desktop's `m_roamPeers` entry). The peer's client stays connected in
/// browse mode — after `initialize` only `session/list` runs; sessions open
/// explicitly via [`RoamPeer::open_session`].
pub struct RoamPeer {
    /// Stable peer name; the routing key and the `roam:<label>:` id prefix.
    label: String,
    inner: Mutex<PeerInner>,
    /// Command queue into the peer's `connect_with` foreground task.
    cmd_tx: tokio_mpsc::UnboundedSender<PeerCommand>,
    listener: Arc<dyn CoreListener>,
    /// Gate supplied by the spine: true while this peer owns the active
    /// session. Chat-scoped events forward only under this gate.
    is_active: Arc<dyn Fn() -> bool + Send + Sync>,
}

struct PeerInner {
    status: ConnectionStatus,
    /// Last `session/list` result, prefixed `roam:<label>:<id>` (CONTRACT §6
    /// namespace).
    sessions: Vec<SessionSummary>,
    /// The raw (unprefixed) id of the session opened on this peer, if any.
    open_session_id: Option<String>,
    /// Each listed session's working directory, keyed by RAW session id
    /// (populated from `session/list`; `session/load` must use the session's
    /// own cwd — the main connection's `last_config` cwd is empty on a
    /// roam-only client and `session/load` hard-fails on a blank cwd).
    session_cwds: HashMap<String, String>,
    /// The live turn's run id (`_meta.goose.activeRunId`); None when no turn
    /// is running. Mirrors the spine's active_run_id.
    active_run: Option<String>,
    /// The SDK connection handle, set once `initialize` succeeds.
    conn: Option<agent_client_protocol::ConnectionTo<Agent>>,
    /// Peer-owned transcript of the open session (never cached).
    transcript: Vec<Message>,
    /// In-flight `session/request_permission` responder, answered via
    /// [`RoamPeer::respond_permission`] (single slot, like the desktop).
    pending_permission: Option<Responder<RequestPermissionResponse>>,
    /// The dialed stream; `None` until the blocking dial completes.
    stream: Option<Arc<RoamStream>>,
    /// Set by [`RoamPeer::close`] so a teardown that races the dial or a
    /// transport error is not reported as a failure.
    closing: bool,
    /// Backgrounded content for sessions that are NOT currently open, keyed by
    /// raw session id (dispatch routes by `notif.session_id`). Chunks keep
    /// accumulating here while the user sits in another chat; opening the
    /// session promotes them (no flash) and clears its green dot.
    staging: HashMap<String, StagedSession>,
}

/// One backgrounded session's staged transcript + dot state.
#[derive(Default)]
struct StagedSession {
    messages: Vec<Message>,
    has_new: bool,
}

impl PeerInner {
    fn new() -> Self {
        Self {
            status: ConnectionStatus::Connecting,
            sessions: Vec::new(),
            open_session_id: None,
            session_cwds: HashMap::new(),
            active_run: None,
            conn: None,
            transcript: Vec::new(),
            pending_permission: None,
            stream: None,
            closing: false,
            staging: HashMap::new(),
        }
    }
}

impl RoamPeer {
    /// Connect to a roam peer in browse mode. The dial (blocking, seconds) and
    /// the ACP handshake run on the core runtime; this returns immediately
    /// with a peer in `Connecting` state. `is_active` is the spine's routing
    /// gate (see the module docs). One peer per label: the caller replaces an
    /// existing peer with the same label before connecting again.
    pub fn connect(
        secret: String,
        card: String,
        label: String,
        listener: Arc<dyn CoreListener>,
        is_active: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Arc<Self> {
        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
        let peer = Arc::new(Self {
            label: label.clone(),
            inner: Mutex::new(PeerInner::new()),
            cmd_tx,
            listener,
            is_active,
        });
        let task = peer.clone();
        runtime().spawn(async move {
            // 1. Dial (blocking, seconds) off the runtime.
            task.emit_status("connecting: dialing");
            let dialed = tokio::task::spawn_blocking({
                let secret = secret.clone();
                let card = card.clone();
                let label = label.clone();
                move || grouse_roam_core::roam_connect(&secret, &card, Some(label))
            })
            .await;
            let stream = match dialed {
                Ok(Ok(stream)) => stream,
                Ok(Err(error)) => {
                    task.fail(format!("roam connect: {error}"));
                    return;
                }
                Err(error) => {
                    task.fail(format!("roam dial task panicked: {error}"));
                    return;
                }
            };
            // Closed while dialing? Hand the stream back immediately.
            if task.inner.lock().closing {
                stream.shutdown();
                stream.cancel();
                return;
            }
            task.inner.lock().stream = Some(stream.clone());

            // 2. The SDK client: notifications + server requests dispatched to
            //    the listener; requests answered via the peer.
            let notif_peer = task.clone();
            let req_peer = task.clone();
            let builder = Client.builder()
                .name("grouse")
                .on_receive_notification(
                    async move |notif: SessionNotification, _cx| {
                        notif_peer.dispatch(notif);
                        Ok(())
                    },
                    on_receive_notification!(),
                )
                .on_receive_request(
                    async move |req: RequestPermissionRequest, responder, _cx| {
                        req_peer.on_permission_request(req, responder);
                        Ok(())
                    },
                    on_receive_request!(),
                );

            // 3. Connect: initialize -> session/list (browse), then serve
            //    commands until close.
            let main_task = task.clone();
            let result = builder
                .connect_with(
                    RoamTransport::new(stream),
                    async move |cx: agent_client_protocol::ConnectionTo<Agent>| {
                        main_task.connected(cx, cmd_rx).await
                    },
                )
                .await;
            task.connection_ended(result);
        });
        peer
    }

    /// The peer's stable label (the routing key).
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Current peer status snapshot.
    pub fn status(&self) -> ConnectionStatus {
        match &self.inner.lock().status {
            ConnectionStatus::Disconnected => ConnectionStatus::Disconnected,
            ConnectionStatus::Connecting => ConnectionStatus::Connecting,
            ConnectionStatus::Ready => ConnectionStatus::Ready,
            ConnectionStatus::Syncing => ConnectionStatus::Syncing,
            ConnectionStatus::Error { message } => ConnectionStatus::Error {
                message: message.clone(),
            },
        }
    }

    /// The last `session/list` result, ids prefixed `roam:<label>:<id>`.
    pub fn sessions(&self) -> Vec<SessionSummary> {
        // Rebuild from fields: the uniffi records are not Clone (yet). Read
        // has_new from the staging map under the SAME lock — staging_has_new
        // locks again and parking_lot is not re-entrant (deadlock).
        let inner = self.inner.lock();
        inner
            .sessions
            .iter()
            .map(|s| SessionSummary {
                id: s.id.clone(),
                title: s.title.clone(),
                updated_at: s.updated_at.clone(),
                last_message_snippet: s.last_message_snippet.clone(),
                project_id: None,
                message_count: 0,
                model: String::new(),
                has_recipe: false,
                has_new: inner.staging.get(&s.id).map(|st| st.has_new).unwrap_or(false),
            })
            .collect()
    }

    /// True while the given raw session id has backgrounded content staged.
    fn staging_has_new(&self, raw_id: &str) -> bool {
        self.inner
            .lock()
            .staging
            .get(raw_id)
            .map(|s| s.has_new)
            .unwrap_or(false)
    }

    /// The open session's id (`roam:<label>:<raw>`), if one is open.
    pub fn active_session_id(&self) -> Option<String> {
        let inner = self.inner.lock();
        inner
            .open_session_id
            .as_ref()
            .map(|id| format!("roam:{}:{id}", self.label))
    }

    /// The accumulated transcript of the open session (peer-owned, never
    /// cached — cache keys would collide across machines).
    pub fn transcript(&self) -> Vec<Message> {
        // Rebuild from fields: the uniffi records are not Clone (yet).
        self.inner
            .lock()
            .transcript
            .iter()
            .map(|m| Message {
                id: m.id.clone(),
                role: m.role.clone(),
                content: m.content.clone(),
                output: m.output.clone(),
            })
            .collect()
    }

    /// Open a session on this peer: `session/load` with the given cwd (the
    /// cwd is authoritative — `session/load` silently rewrites `working_dir`
    /// when it differs). Fire-and-forget into the peer's command loop, like
    /// the desktop's `openRoamSession`. The raw id is derived by stripping the
    /// `roam:<label>:` prefix when the caller passed a prefixed id.
    pub fn open_session(&self, session_id: String, cwd: String) {
        if !matches!(self.status(), ConnectionStatus::Ready) {
            return; // browse peers only open sessions once connected
        }
        // The raw id is derived by stripping the prefix when the caller passed
        // a prefixed id.
        let raw = self.strip_prefix(&session_id);
        // session/load hard-fails on a blank cwd. The passed cwd can be empty
        // on a roam-only client (no main connection to seed last_config), so
        // fall back to the session's own cwd from session/list — the remote
        // goose rewrote it there, so it's the guaranteed-valid one.
        let cwd = if cwd.is_empty() {
            self.inner.lock().session_cwds.get(&raw).cloned().unwrap_or_default()
        } else {
            cwd
        };
        let _ = self.cmd_tx.send(PeerCommand::OpenSession {
            session_id: raw,
            cwd,
        });
    }

    /// Create a fresh session on this peer. The peer's goose has no default
    /// cwd, so one is required (like `session/new` on the main connection);
    /// the caller resolves it the same way `open_session` does. Fire-and-forget
    /// into the peer's command loop, like the desktop's `openRoamSession`.
    pub fn new_session(&self, cwd: String) {
        if !matches!(self.status(), ConnectionStatus::Ready) {
            return; // browse peers only open sessions once connected
        }
        let _ = self.cmd_tx.send(PeerCommand::NewSession { cwd });
    }

    /// Disconnect the peer: FIN + cancel the stream (unblocks the reader),
    /// shut down the command loop, and report the terminal status.
    pub fn close(&self) {
        {
            let mut inner = self.inner.lock();
            if inner.closing {
                return;
            }
            inner.closing = true;
            inner.status = ConnectionStatus::Disconnected;
        }
        let _ = self.cmd_tx.send(PeerCommand::Close);
        if let Some(stream) = self.inner.lock().stream.clone() {
            stream.shutdown();
            stream.cancel();
        }
        self.emit_status("disconnected");
    }

    /// Send a raw JSON-RPC request over this peer's connection — the
    /// peer-side mirror of the spine's `Conn::rpc`. Synchronous: the request
    /// is spawned onto the core runtime and this call blocks on the reply.
    /// Chat/unstable intents route here while this peer owns the active
    /// session. Call from a UI/intent thread, not from inside the dispatch
    /// loop.
    pub fn rpc(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        let conn = self
            .inner
            .lock()
            .conn
            .clone()
            .ok_or_else(|| AcpError::internal_error().data("roam peer not connected"))?;
        let msg = UntypedMessage::new(method, self.wire_params(params))?;
        let (tx, rx) = std_mpsc::channel();
        runtime().spawn(async move {
            let _ = tx.send(conn.send_request(msg).block_task().await);
        });
        let result = rx
            .recv()
            .map_err(|_| AcpError::internal_error().data("roam rpc task failed"))??;
        // The turn's completion rides the session/prompt REPLACE (the reply's
        // stopReason) — the main connection surfaces it via store.run_ended;
        // the peer has no store, so emit RunEnded straight to the listener so
        // the app clears its in-flight/turnInFlight state and drains the queue.
        if method == "session/prompt" {
            let stop = result
                .get("stopReason")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if (self.is_active)() {
                self.emit_stream(StreamEvent::RunEnded {
                    stop_reason: stop,
                });
            }
            // The peer's turn bookkeeping mirrors the spine: a live run id is
            // now over, whether or not the server sent an activeRunId update.
            self.inner.lock().active_run = None;
        }
        Ok(result)
    }

    /// Preprocess outbound `sessionId` params at the peer wire boundary.
    ///
    /// The app and the spine always address this peer's sessions by their
    /// prefixed id (`roam:<label>:<raw>`, see `open_session`). The remote
    /// goose knows only the raw id — `open_session` strips the prefix for the
    /// same reason — so every request/notification that carries a
    /// `sessionId` must be stripped to the raw id before it leaves this peer's
    /// connection, or the remote cannot resolve the session. This is the
    /// single rewrite point for `rpc` and `notify`; a `sessionId` that does
    /// not carry *this* peer's exact prefix passes through untouched (a raw id
    /// is already correct, and another peer's prefix can't reach this
    /// connection because `route()` resolves by label).
    fn wire_params(&self, params: Value) -> Value {
        let Value::Object(mut object) = params else {
            return params;
        };
        let stripped = match object.get("sessionId") {
            Some(Value::String(id)) => self.strip_prefix(id),
            _ => return Value::Object(object),
        };
        if stripped == object["sessionId"].as_str().unwrap_or_default() {
            return Value::Object(object);
        }
        object.insert("sessionId".into(), Value::String(stripped));
        Value::Object(object)
    }

    /// Send a raw JSON-RPC notification over this peer's connection (e.g.
    /// `session/cancel`) — the peer-side mirror of the spine's `Conn::notify`.
    /// Fully synchronous and non-blocking: the notification is handed to the
    /// SDK's outgoing channel; there is no reply to wait for.
    pub fn notify(&self, method: &str, params: Value) -> Result<(), AcpError> {
        let conn = self
            .inner
            .lock()
            .conn
            .clone()
            .ok_or_else(|| AcpError::internal_error().data("roam peer not connected"))?;
        let msg = UntypedMessage::new(method, self.wire_params(params))?;
        conn.send_notification(msg)
    }

    /// Answer the pending `session/request_permission` from this peer
    /// (CONTRACT §5; the spine routes `respond_permission` here when the peer
    /// owns the active session).
    pub fn respond_permission(&self, outcome: PermissionOutcome) -> Result<(), AcpError> {
        let responder = self
            .inner
            .lock()
            .pending_permission
            .take()
            .ok_or_else(|| AcpError::internal_error().data("no pending roam permission request"))?;
        let outcome = match outcome {
            PermissionOutcome::Selected { option_id } => {
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                    PermissionOptionId::new(option_id),
                ))
            }
            PermissionOutcome::Cancelled => RequestPermissionOutcome::Cancelled,
        };
        responder
            .respond(RequestPermissionResponse::new(outcome))
            .map_err(|error| {
                AcpError::internal_error().data(format!("roam permission respond: {error}"))
            })
    }

    // -- internals (all run on the core runtime) ------------------------------

    /// The browse-mode handshake, run inside `connect_with`'s foreground:
    /// initialize → `session/list`, NO auto-open. Then serve commands until
    /// close. This is the desktop's `connectRoam` + `setBrowseOnly(true)`
    /// flow, minus the reconnect (peers never auto-reconnect).
    async fn connected(
        &self,
        cx: agent_client_protocol::ConnectionTo<Agent>,
        mut cmd_rx: tokio_mpsc::UnboundedReceiver<PeerCommand>,
    ) -> Result<(), AcpError> {
        self.inner.lock().conn = Some(cx.clone());

        // Phase-tagged + bounded handshake: a stall in any phase must surface
        // as a per-phase status the UI can show, not a silent forever-hang.
        // The host logs "connected" once its accept-ack is SENT — the phone
        // can still be waiting here in `initialize`/`session/list` after that.
        const PHASE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

        self.emit_status("connecting: handshake");
        let init = tokio::time::timeout(
            PHASE_TIMEOUT,
            cx.send_request(initialize_request()).block_task(),
        )
        .await;
        let _init = match init {
            Ok(Ok(reply)) => reply,
            Ok(Err(error)) => {
                self.fail(format!("roam initialize: {error}"));
                return Err(AcpError::internal_error()
                    .data(format!("roam initialize: {error}")));
            }
            Err(_) => {
                self.fail("handshake timed out waiting for initialize reply".into());
                return Err(AcpError::internal_error().data(
                    "handshake timed out waiting for initialize reply",
                ));
            }
        };

        self.emit_status("connecting: listing sessions");
        let list = tokio::time::timeout(
            PHASE_TIMEOUT,
            cx.send_request(ListSessionsRequest::new().meta(session_list_meta()))
                .block_task(),
        )
        .await;
        let list: ListSessionsResponse = match list {
            Ok(Ok(reply)) => reply,
            Ok(Err(error)) => {
                self.fail(format!("roam session/list: {error}"));
                return Err(AcpError::internal_error()
                    .data(format!("roam session/list: {error}")));
            }
            Err(_) => {
                self.fail("handshake timed out waiting for session/list reply".into());
                return Err(AcpError::internal_error().data(
                    "handshake timed out waiting for session/list reply",
                ));
            }
        };
        self.apply_sessions(&list);

        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                PeerCommand::OpenSession { session_id, cwd } => {
                    let raw = self.strip_prefix(&session_id);
                    // A stale/archived session can't be resumed — fall back to
                    // a fresh session rather than leaving the chat wedged on
                    // the failed load (desktop `response()` behavior).
                    //
                    // Untyped sends: the raw reply `Value` is needed both to
                    // surface the peer's own {provider, model, effort} config
                    // (the app's model picker for this peer comes from here,
                    // not from the main connection) and to derive the new
                    // session id on the fallback path.
                    // mcpServers is REQUIRED by the remote deserializer (same
                    // strictness as session/new — missing field = hard error).
                    let load = cx
                        .send_request(UntypedMessage::new(
                            "session/load",
                            json!({ "sessionId": raw, "cwd": cwd, "mcpServers": [] }),
                        )?)
                        .block_task()
                        .await;
                    match load {
                        Ok(reply) => {
                            self.open(raw);
                            self.emit_status("ready");
                            self.emit_config(&reply);
                        }
                        Err(error) => {
                            // Fallback: the tapped session is stale/archived —
                            // a fresh session is better than a wedged chat.
                            // NO on_peer_new_session here: that event opens the
                            // app's UI, and the user asked for THIS session
                            // (the fallback silently serves a proxy).
                            match self.create_session(&cx, &cwd, false).await {
                                Ok(()) => {}
                                Err(_) => {
                                    self.fail(format!("roam session/load: {error}"));
                                }
                            }
                        }
                    }
                }
                PeerCommand::NewSession { cwd } => {
                    match self.create_session(&cx, &cwd, true).await {
                        Ok(()) => {}
                        Err(error) => {
                            self.fail(format!("roam session/new: {error}"));
                        }
                    }
                }
                PeerCommand::Close => break,
            }
        }
        Ok(())
    }

    /// `session/new` on this peer: untyped so the reply `Value` surfaces the
    /// peer's own {provider, model, effort} config and the fresh session id
    /// (the app's model picker for this peer comes from `emit_config`). Opens
    /// the created session and reports ready.
    async fn create_session(
        &self,
        cx: &agent_client_protocol::ConnectionTo<agent_client_protocol::Agent>,
        cwd: &str,
        notify_new: bool,
    ) -> Result<(), AcpError> {
        // The remote goose's session/new deserializer REQUIRES mcpServers
        // (a missing field is a hard protocol error, not a default) — the
        // main connection sends it and so must this peer.
        let mut params = json!({ "cwd": cwd, "mcpServers": [] });
        params["_meta"] = Value::Object(new_session_meta());
        let reply = cx
            .send_request(UntypedMessage::new("session/new", params)?)
            .block_task()
            .await
            .map_err(|error| AcpError::internal_error().data(format!("roam session/new: {error}")))?;
        let sid = reply
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        self.open(sid.clone());
        self.emit_status("ready");
        self.emit_config(&reply);
        // The app opens a freshly created chat on this id — but ONLY when the
        // user initiated the create. The load-fallback create must not yank
        // the UI to a proxy session the user didn't ask for.
        if notify_new {
            self.listener.on_peer_new_session(self.label.clone(), sid);
        }
        // The created session must enter the peer's list or the app's drawer
        // never learns it exists (and cannot route a chat to it). Re-list.
        let relist = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            cx.send_request(ListSessionsRequest::new().meta(session_list_meta()))
                .block_task(),
        )
        .await;
        if let Ok(Ok(list)) = relist {
            self.apply_sessions(&list);
        }
        Ok(())
    }

    /// Surface the session config from a `session/load`|`session/new` reply as
    /// a `CoreListener::on_config` event. The remote goose populates the
    /// `model` option's `options` list with its featured models, so this is
    /// where a peer's model picker gets its choices. A reply without
    /// `configOptions` yields an empty vec — the app tolerates that.
    fn emit_config(&self, reply: &Value) {
        let options = crate::spine::parse_config_options(reply);
        self.listener.on_config(options);
    }

    /// `session/list` arrived: store it, flip to Ready, emit. Browse mode — no
    /// session is auto-opened here (open only via `open_session`).
    fn apply_sessions(&self, list: &ListSessionsResponse) {
        let mut cwds = HashMap::new();
        let sessions: Vec<SessionSummary> = list
            .sessions
            .iter()
            .map(|s| {
                let id = s.session_id.to_string();
                cwds.insert(id.clone(), s.cwd.to_string_lossy().to_string());
                let mut summary = to_summary(&self.label, s);
                summary.has_new = self.staging_has_new(&id);
                summary
            })
            .collect();
        {
            let mut inner = self.inner.lock();
            inner.sessions = sessions;
            inner.session_cwds = cwds;
            inner.status = ConnectionStatus::Ready;
        }
        self.emit_sessions(self.sessions());
        self.emit_status("ready");
    }

    /// A session/load succeeded: the peer now owns an open session and its
    /// transcript starts fresh (the server replays history as chunks).
    fn open(&self, raw_session_id: String) {
        let mut inner = self.inner.lock();
        // Promote backgrounded content if any: the staged chunks ARE the
        // session's transcript since the last visit (chunks only accumulate
        // here while the session is closed), so show them instantly — the
        // session/load replay that follows merges/dedupes against them.
        if let Some(staged) = inner.staging.remove(&raw_session_id) {
            inner.transcript = staged.messages;
        } else {
            inner.transcript.clear();
        }
        inner.open_session_id = Some(raw_session_id);
        // Promotion (or simply visiting) clears the green dot: has_new is
        // gone with the staging entry, and sessions() re-reads it live.
    }

    /// The connection task ended. `Ok` = clean (close command). `Err` = the
    /// transport dropped — only report it as a failure if the peer wasn't
    /// closed deliberately.
    fn connection_ended(&self, result: Result<(), AcpError>) {
        let mut inner = self.inner.lock();
        inner.conn = None;
        if inner.closing {
            inner.status = ConnectionStatus::Disconnected;
            return;
        }
        inner.status = ConnectionStatus::Disconnected;
        drop(inner);
        match result {
            Ok(()) => self.emit_status("disconnected"),
            Err(error) => self.emit_status(&format!("error: {error}")),
        }
    }

    /// Set an error status and surface it (used by dial/handshake failures).
    fn fail(&self, message: String) {
        let mut inner = self.inner.lock();
        if inner.closing {
            return;
        }
        inner.status = ConnectionStatus::Error {
            message: message.clone(),
        };
        drop(inner);
        self.emit_status(&format!("error: {message}"));
    }

    fn strip_prefix(&self, id: &str) -> String {
        id.strip_prefix(&format!("roam:{}:", self.label))
            .unwrap_or(id)
            .to_string()
    }

    // -- SessionNotification dispatch -----------------------------------------

    /// The peer's `session/update` dispatch — the same `SessionNotification`
    /// translation the spine uses, but for this peer's connection. Chat-scoped
    /// events (stream/transcript/touched/permission) forward only under the
    /// `is_active` gate; peer-scoped events are emitted unconditionally by
    /// their callers.
    fn dispatch(&self, notif: SessionNotification) {
        let active = (self.is_active)();
        let session_id = notif.session_id.to_string();
        // Session-membership gate: this notification's chunks belong to the
        // OPEN session (live view) or to a backgrounded one (staging + dot).
        // Nothing from another session ever renders in the visible chat.
        let mine = self
            .inner
            .lock()
            .open_session_id
            .as_deref()
            == Some(session_id.as_str());
        match notif.update {
            SessionUpdate::UserMessageChunk(chunk) => {
                if let Some(text) = chunk_text(&chunk.content) {
                    let message_id = chunk.message_id.map(|m| m.to_string()).unwrap_or_default();
                    let event = self.accumulate(&session_id, "user", &text, &message_id);
                    if active && mine {
                        if let Some(event) = event {
                            self.emit(event);
                        }
                        self.emit_stream(StreamEvent::UserChunk { text, message_id });
                    }
                }
            }
            SessionUpdate::AgentMessageChunk(chunk) => {
                if let Some(text) = chunk_text(&chunk.content) {
                    let message_id = chunk.message_id.map(|m| m.to_string()).unwrap_or_default();
                    let event = self.accumulate(&session_id, "agent", &text, &message_id);
                    if active && mine {
                        if let Some(event) = event {
                            self.emit(event);
                        }
                        self.emit_stream(StreamEvent::AgentChunk { text, message_id });
                    }
                }
            }
            SessionUpdate::AgentThoughtChunk(chunk) => {
                if let Some(text) = chunk_text(&chunk.content) {
                    let message_id = chunk.message_id.map(|m| m.to_string()).unwrap_or_default();
                    let event = self.accumulate(&session_id, "thought", &text, &message_id);
                    if active && mine {
                        if let Some(event) = event {
                            self.emit(event);
                        }
                        self.emit_stream(StreamEvent::ThoughtChunk { text });
                    }
                }
            }
            SessionUpdate::ToolCall(tool) => {
                let tool_call_id = tool.tool_call_id.to_string();
                let (kind, detail) = tool_kind(&tool);
                let event = self.accumulate_tool(&session_id, &tool_call_id, &tool.title, None);
                if active && mine {
                    if let Some(event) = event {
                        self.emit(event);
                    }
                    self.emit_stream(StreamEvent::ToolCall {
                        title: tool.title.clone(),
                        detail,
                        tool_call_id: tool_call_id.clone(),
                        kind,
                    });
                }
            }
            SessionUpdate::ToolCallUpdate(update) => {
                let id = update.tool_call_id.to_string();
                let (status, output, live) = tool_update(&update);
                let event = if live {
                    self.accumulate_tool_append(&session_id, &id, &output)
                } else {
                    self.accumulate_tool(&session_id, &id, &status, Some(&output))
                };
                if active && mine {
                    if let Some(event) = event {
                        self.emit(event);
                    }
                    self.emit_stream(StreamEvent::ToolCallUpdate {
                        id: id.clone(),
                        status: status.clone(),
                        output: output.clone(),
                        live,
                    });
                }
            }
            SessionUpdate::UsageUpdate(usage) => {
                let (cost, currency) = usage
                    .cost
                    .as_ref()
                    .map(|c| (c.amount, c.currency.clone()))
                    .unwrap_or((0.0, String::new()));
                if active && mine {
                    self.emit_stream(StreamEvent::Usage {
                        used: usage.used as i64,
                        size: usage.size as i64,
                        cost,
                        currency,
                    });
                }
            }
            SessionUpdate::SessionInfoUpdate(info) => {
                // The active-run lifecycle rides _meta.goose.activeRunId (same
                // translation as the spine's dispatch_session_info_update):
                // a present id starts a turn, an absent one ends it. The
                // prefixed session id is emitted so the app's
                // onCoreActiveRun matches currentSession and clears the
                // prompting state.
                if active {
                    if let Some(goose) = info.meta.as_ref().and_then(|meta| meta.get("goose")) {
                        let run = goose.get("activeRunId").and_then(Value::as_str);
                        let started = run.map(|r| r.to_string());
                        let ended = started.is_none()
                            && self.inner.lock().active_run.take().is_some();
                        if let Some(run_id) = started {
                            self.inner.lock().active_run = Some(run_id.clone());
                            self.listener.on_active_run(
                                format!("roam:{}:{session_id}", self.label),
                                run_id,
                            );
                        } else if ended {
                            self.listener.on_active_run(
                                format!("roam:{}:{session_id}", self.label),
                                String::new(),
                            );
                        }
                    }
                }
                let title = info.title.value().map(|t| t.to_string()).unwrap_or_default();
                let updated_at = info
                    .updated_at
                    .value()
                    .map(|t| t.to_string())
                    .unwrap_or_default();
                if active && (!title.is_empty() || !updated_at.is_empty()) {
                    self.listener
                        .on_session_touched(session_id, title, updated_at);
                }
            }
            SessionUpdate::ConfigOptionUpdate(config) if active => {
                // Peer-scoped config change (e.g. the user picked a different
                // model on this peer): forward it to the listener so the app
                // shows the peer's current provider/model/effort. Mirrors the
                // spine's translation; gated on `active` like the neighboring
                // chat-scoped arms.
                let options = config
                    .config_options
                    .iter()
                    .map(|option| crate::ConfigOption {
                        id: option.id.to_string(),
                        value: crate::spine::config_option_value(option),
                        name: option.name.clone(),
                        // The typed schema has no choices list — the initial
                        // config (session/load|new reply) carries them.
                        choices: Vec::new(),
                    })
                    .collect::<Vec<_>>();
                self.listener.on_config(options);
            }
            _ => {} // other updates are not peer-scoped chat events
        }
    }

    /// Append a text chunk to the bubble for `(role, message_id)`; a new
    /// message_id starts a new bubble (CONTRACT §4 accumulation rule). Chunks
    /// for a session that is NOT currently open accumulate into its staging
    /// buffer instead (green dot) and never touch the visible transcript.
    /// Returns the transcript mutation for the caller to emit under the
    /// active gate (`emit_if_mine` handles the session-membership check too,
    /// so a staged event is never emitted to the live view).
    fn accumulate(&self, session_id: &str, role: &str, text: &str, message_id: &str) -> Option<TranscriptEvent> {
        let mut inner = self.inner.lock();
        let mine = inner.open_session_id.as_deref() == Some(session_id);
        let mut staged_new = false;
        let target: &mut Vec<Message> = if mine {
            &mut inner.transcript
        } else {
            let st = inner.staging.entry(session_id.to_string()).or_default();
            staged_new = !st.has_new;
            st.has_new = true;
            &mut st.messages
        };
        let event = if let Some(msg) = target
            .iter_mut()
            .find(|m| m.role == role && m.id == message_id)
        {
            // Replay dedupe: a staged message being re-sent by session/load
            // is identical — appending it again would double the text.
            if !msg.content.contains(text) {
                msg.content.push_str(text);
                Some(TranscriptEvent::Update {
                    message: Message {
                        id: msg.id.clone(),
                        role: msg.role.clone(),
                        content: msg.content.clone(),
                        output: String::new(),
                    },
                })
            } else {
                None
            }
        } else {
            let msg = Message {
                id: message_id.to_string(),
                role: role.to_string(),
                content: text.to_string(),
                output: String::new(),
            };
            target.push(Message {
                id: msg.id.clone(),
                role: msg.role.clone(),
                content: msg.content.clone(),
                output: String::new(),
            });
            Some(TranscriptEvent::Append { message: msg })
        };
        drop(inner);
        if !mine && staged_new {
            // First backgrounded content for this session: tell the UI once
            // (prefixed id, so it can paint the row). It re-reads sessions()
            // for the dot state; no per-chunk traffic.
            self.listener.on_session_touched(
                format!("roam:{}:{}", self.label, session_id),
                String::new(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis().to_string())
                    .unwrap_or_default(),
            );
        }
        event
    }

    /// Create (or replace) the bubble for a tool call id.
    fn accumulate_tool(
        &self,
        session_id: &str,
        tool_call_id: &str,
        title: &str,
        output: Option<&str>,
    ) -> Option<TranscriptEvent> {
        let mut inner = self.inner.lock();
        let mine = inner.open_session_id.as_deref() == Some(session_id);
        let mut staged_new = false;
        let target: &mut Vec<Message> = if mine {
            &mut inner.transcript
        } else {
            let st = inner.staging.entry(session_id.to_string()).or_default();
            staged_new = !st.has_new;
            st.has_new = true;
            &mut st.messages
        };
        // Serve-shape: content is the TITLE ONLY; the result rides Message.output
        // so the UI chip never renders the tool output in its header.
        let (message, is_new) = if let Some(msg) = target
            .iter_mut()
            .find(|m| m.role == "tool" && m.id == tool_call_id)
        {
            msg.content = title.to_string();
            msg.output = output.unwrap_or("").to_string();
            (
                Message {
                    id: msg.id.clone(),
                    role: msg.role.clone(),
                    content: msg.content.clone(),
                    output: msg.output.clone(),
                },
                false,
            )
        } else {
            let msg = Message {
                id: tool_call_id.to_string(),
                role: "tool".to_string(),
                content: title.to_string(),
                output: output.unwrap_or("").to_string(),
            };
            target.push(Message {
                id: msg.id.clone(),
                role: msg.role.clone(),
                content: msg.content.clone(),
                output: msg.output.clone(),
            });
            (msg, true)
        };
        drop(inner);
        if !mine && staged_new {
            let _ = self.staged_touch(session_id);
        }
        Some(if is_new {
            TranscriptEvent::Append { message }
        } else {
            TranscriptEvent::Update { message }
        })
    }

    /// Live shell output appends to the tool bubble instead of replacing it
    /// (CONTRACT §4: "live shell output appends, the completion update
    /// replaces").
    fn accumulate_tool_append(&self, session_id: &str, tool_call_id: &str, output: &str) -> Option<TranscriptEvent> {
        let mut inner = self.inner.lock();
        let mine = inner.open_session_id.as_deref() == Some(session_id);
        let mut staged_new = false;
        let target: &mut Vec<Message> = if mine {
            &mut inner.transcript
        } else {
            let st = inner.staging.entry(session_id.to_string()).or_default();
            staged_new = !st.has_new;
            st.has_new = true;
            &mut st.messages
        };
        // Appends go to Message.output (title stays in content). Dedupe: a
        // replay re-delivering already-staged output must not double it.
        let result = if let Some(msg) = target
            .iter_mut()
            .find(|m| m.role == "tool" && m.id == tool_call_id)
        {
            if !msg.output.contains(output) {
                msg.output.push_str(output);
                Some(TranscriptEvent::Update {
                    message: Message {
                        id: msg.id.clone(),
                        role: msg.role.clone(),
                        content: msg.content.clone(),
                        output: msg.output.clone(),
                    },
                })
            } else {
                None
            }
        } else {
            None
        };
        drop(inner);
        if !mine && staged_new {
            let _ = self.staged_touch(session_id);
        }
        result
    }

    /// Fire the UI notification for a session's FIRST backgrounded content
    /// (prefixed id, so the client can paint the dot on the peer row).
    fn staged_touch(&self, session_id: &str) {
        self.listener.on_session_touched(
            format!("roam:{}:{}", self.label, session_id),
            String::new(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis().to_string())
                .unwrap_or_default(),
        );
    }

    /// A `session/request_permission` arrived: stash the responder and prompt
    /// the UI (gated on the peer owning the active session, like the desktop).
    fn on_permission_request(
        &self,
        req: RequestPermissionRequest,
        responder: Responder<RequestPermissionResponse>,
    ) {
        let tool_call_id = req.tool_call.tool_call_id.to_string();
        let options = req
            .options
            .iter()
            .map(|o| crate::PermissionOption {
                option_id: o.option_id.to_string(),
                name: o.name.clone(),
                kind: format!("{:?}", o.kind),
            })
            .collect();
        self.inner.lock().pending_permission = Some(responder);
        if (self.is_active)() {
            self.listener.on_permission_request(crate::PermissionRequest {
                tool_call_id,
                title: req.tool_call.fields.title.clone().unwrap_or_default(),
                detail: String::new(),
                options,
            });
        }
    }

    // -- emits ----------------------------------------------------------------

    fn emit(&self, event: TranscriptEvent) {
        self.listener.on_transcript(event);
    }

    fn emit_stream(&self, event: StreamEvent) {
        self.listener.on_stream(event);
    }

    fn emit_sessions(&self, sessions: Vec<SessionSummary>) {
        self.listener.on_roam_sessions(self.label.clone(), sessions);
    }

    fn emit_status(&self, status: &str) {
        self.listener
            .on_roam_peer_status(self.label.clone(), status.to_string());
    }
}

/// The unstable shim routes session-bound RPCs through this when the session
/// belongs to a peer (`roam:<label>:` prefix) — the peer's connection answers
/// tool/extension/config calls for its own sessions.
impl RpcConn for RoamPeer {
    fn rpc(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        RoamPeer::rpc(self, method, params)
    }

    fn active_session_id(&self) -> Option<String> {
        let raw = self.inner.lock().open_session_id.clone()?;
        Some(format!("roam:{}:{}", self.label, raw))
    }
}

// ---------------------------------------------------------------------------
// SessionNotification translation helpers (the "same dispatch the spine uses")
// ---------------------------------------------------------------------------

/// The text of a chunk's content block; non-text blocks render as a marker
/// (desktop `standardUpdate`'s `text()` lambda).
fn chunk_text(block: &agent_client_protocol::schema::v1::ContentBlock) -> Option<String> {
    use agent_client_protocol::schema::v1::ContentBlock;
    match block {
        ContentBlock::Text(t) => Some(t.text.clone()),
        ContentBlock::ResourceLink(_) => Some("[resource]".to_string()),
        other => Some(format!("[{}]", block_type_name(other))),
    }
}

fn block_type_name(block: &agent_client_protocol::schema::v1::ContentBlock) -> &'static str {
    use agent_client_protocol::schema::v1::ContentBlock;
    match block {
        ContentBlock::ResourceLink(_) => "resource_link",
        ContentBlock::Resource(_) => "resource",
        _ => "unknown",
    }
}

/// ToolCall → `StreamEvent::ToolCall`, collapsing the desktop's mcpapp/chart
/// split into `ToolCallKind` (CONTRACT §3.4). MCP-App resources come from
/// `_meta.goose.mcpApp`; the legacy chart fallback keys on the goose tool
/// name + `data` input.
fn tool_kind(tool: &agent_client_protocol::schema::v1::ToolCall) -> (ToolCallKind, String) {
    let detail = tool
        .raw_input
        .as_ref()
        .and_then(|input| input.get("command"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let goose = tool
        .meta
        .as_ref()
        .and_then(|meta| meta.get("goose"))
        .and_then(|v| v.as_object());
    let mcp_app = goose
        .and_then(|g| g.get("mcpApp"))
        .and_then(|v| v.as_object());
    if let (Some(uri), Some(extension)) = (
        mcp_app
            .and_then(|m| m.get("resourceUri"))
            .and_then(|v| v.as_str()),
        mcp_app
            .and_then(|m| m.get("extensionName"))
            .and_then(|v| v.as_str()),
    ) {
        let input = tool
            .raw_input
            .as_ref()
            .map(|v| v.to_string())
            .unwrap_or_default();
        return (
            ToolCallKind::McpApp {
                app_key: format!("{extension}|{uri}"),
                uri: uri.to_string(),
                extension: extension.to_string(),
                input,
            },
            detail,
        );
    }
    let tool_name = goose
        .and_then(|g| g.get("toolCall"))
        .and_then(|v| v.as_object())
        .and_then(|t| t.get("toolName"))
        .and_then(|v| v.as_str());
    let chart_data = tool
        .raw_input
        .as_ref()
        .and_then(|input| input.get("data"))
        .and_then(|d| match d {
            Value::String(s) => Some(s.clone()),
            Value::Object(_) => Some(d.to_string()),
            _ => None,
        });
    if tool_name == Some("autovisualiser__show_chart") {
        if let Some(spec) = chart_data {
            return (ToolCallKind::Chart { spec }, detail);
        }
    }
    (ToolCallKind::Plain, detail)
}

/// ToolCallUpdate → status/output/live. Live shell output rides
/// `_meta.toolNotification.type == "live_output"` and appends; otherwise the
/// content texts join as the final output (desktop `standardUpdate`).
fn tool_update(
    update: &agent_client_protocol::schema::v1::ToolCallUpdate,
) -> (String, String, bool) {
    let status = update
        .fields
        .status
        .as_ref()
        .map(|s| format!("{:?}", s))
        .unwrap_or_default();
    let notif = update
        .meta
        .as_ref()
        .and_then(|meta| meta.get("toolNotification"))
        .and_then(|v| v.as_object());
    if notif.and_then(|n| n.get("type")).and_then(|v| v.as_str()) == Some("live_output") {
        let mut chunk = String::new();
        if let Some(params) = notif.and_then(|n| n.get("params")).and_then(|v| v.as_object()) {
            if let Some(chunks) = params.get("chunks").and_then(|v| v.as_array()) {
                for c in chunks {
                    if let Some(text) = c
                        .get("output")
                        .and_then(|o| o.as_object())
                        .and_then(|o| o.get("text"))
                        .and_then(|t| t.as_str())
                    {
                        chunk.push_str(text);
                    }
                }
            }
        }
        return (status, chunk, true);
    }
    let mut outputs = Vec::new();
    use agent_client_protocol::schema::v1::{ContentBlock, ToolCallContent};
    for content in &update.fields.content.clone().unwrap_or_default() {
        if let ToolCallContent::Content(content) = content {
            if let ContentBlock::Text(text) = &content.content {
                outputs.push(text.text.clone());
            }
        }
    }
    (status, outputs.join("\n"), false)
}

/// One `session/list` entry → summary, in the peer's id namespace
/// (`roam:<label>:<id>`, CONTRACT §6).
fn to_summary(label: &str, info: &SessionInfo) -> SessionSummary {
    SessionSummary {
        id: format!("roam:{label}:{}", info.session_id),
        title: info.title.clone().unwrap_or_default(),
        updated_at: info.updated_at.clone().unwrap_or_default(),
        last_message_snippet: None,
        project_id: None,
        message_count: 0,
        model: String::new(),
        has_recipe: false,
        has_new: false,
    }
}

/// `initialize` request mirroring the desktop's `onOpen`: protocol v1, no fs
/// access, form elicitation, and the goose `_meta` capabilities without which
/// `session/new` hard-fails for recipe-parameter sessions.
fn initialize_request() -> InitializeRequest {
    let mut fs = FileSystemCapabilities::default();
    fs.read_text_file = false;
    fs.write_text_file = false;
    let mut caps = ClientCapabilities::new().fs(fs).elicitation(
        ElicitationCapabilities::new().form(ElicitationFormCapabilities::default()),
    );
    caps.meta = Some(goose_meta());
    InitializeRequest::new(ProtocolVersion::V1)
        .client_capabilities(caps)
        .client_info(Implementation::new("grouse", env!("CARGO_PKG_VERSION")))
}

/// The `_meta.goose` capability block (customNotifications etc.).
fn goose_meta() -> Map<String, Value> {
    let mut goose = Map::new();
    goose.insert("customNotifications".into(), Value::Bool(true));
    goose.insert("recipeParameterRequests".into(), Value::Bool(true));
    goose.insert("toolCallLabelEnrichment".into(), Value::Bool(true));
    let mut meta = Map::new();
    meta.insert("goose".into(), Value::Object(goose));
    meta
}

/// `session/list` filter: user + acp sessions only (desktop `listSessions`).
fn session_list_meta() -> Map<String, Value> {
    let mut meta = Map::new();
    meta.insert(
        "types".into(),
        Value::Array(vec![
            Value::String("user".into()),
            Value::String("acp".into()),
        ]),
    );
    meta
}

/// `session/new` `_meta`: `client` present ⇒ `SessionType::User`, so the
/// Desktop/CLI can see these chats (desktop `startNewSession`).
fn new_session_meta() -> Map<String, Value> {
    let mut meta = Map::new();
    meta.insert("client".into(), Value::String("grouse".into()));
    meta
}

// ---------------------------------------------------------------------------
// Routing (CONTRACT §6: chat routes to the last-opened session's owner)
// ---------------------------------------------------------------------------

/// Pick the peer that owns the active session: the one whose label equals
/// `active_label`. `None` when no roam session is active (chat routes to the
/// main connection). The spine calls this from its chat/unstable intents.
pub fn active_peer<'a>(
    peers: &'a [Arc<RoamPeer>],
    active_label: &Option<String>,
) -> Option<&'a Arc<RoamPeer>> {
    let label = active_label.as_ref()?;
    peers.iter().find(|peer| &peer.label == label)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::{
        ConfigOptionUpdate, ContentBlock, ContentChunk, TextContent, ToolCall, ToolCallId,
    };
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Records every CoreListener call as a line, for asserting dispatch.
    /// The uniffi records/enums are not Debug yet, so events stringify via
    /// explicit matches.
    struct RecordingListener {
        events: Mutex<Vec<String>>,
    }

    fn status_name(status: &ConnectionStatus) -> &'static str {
        match status {
            ConnectionStatus::Disconnected => "Disconnected",
            ConnectionStatus::Connecting => "Connecting",
            ConnectionStatus::Ready => "Ready",
            ConnectionStatus::Syncing => "Syncing",
            ConnectionStatus::Error { .. } => "Error",
        }
    }

    fn transcript_name(event: &TranscriptEvent) -> &'static str {
        match event {
            TranscriptEvent::Append { .. } => "Append",
            TranscriptEvent::Update { .. } => "Update",
            TranscriptEvent::Clear => "Clear",
        }
    }

    fn stream_name(event: &StreamEvent) -> &'static str {
        match event {
            StreamEvent::AgentChunk { .. } => "AgentChunk",
            StreamEvent::UserChunk { .. } => "UserChunk",
            StreamEvent::ThoughtChunk { .. } => "ThoughtChunk",
            StreamEvent::ToolCall { .. } => "ToolCall",
            StreamEvent::ToolCallUpdate { .. } => "ToolCallUpdate",
            StreamEvent::Usage { .. } => "Usage",
            StreamEvent::RunEnded { .. } => "RunEnded",
        }
    }

    fn tool_kind_name(kind: &ToolCallKind) -> &'static str {
        match kind {
            ToolCallKind::Plain => "Plain",
            ToolCallKind::Chart { .. } => "Chart",
            ToolCallKind::McpApp { .. } => "McpApp",
        }
    }

    impl CoreListener for RecordingListener {
        fn on_status(&self, status: ConnectionStatus) {
            self.events
                .lock()
                .push(format!("status {}", status_name(&status)));
        }
        fn on_sessions(&self, sessions: Vec<SessionSummary>) {
            self.events.lock().push(format!("sessions {}", sessions.len()));
        }
        fn on_transcript(&self, event: TranscriptEvent) {
            self.events
                .lock()
                .push(format!("transcript {}", transcript_name(&event)));
        }
        fn on_stream(&self, event: StreamEvent) {
            let line = match &event {
                StreamEvent::ToolCall {
                    title,
                    detail,
                    tool_call_id,
                    kind,
                } => format!(
                    "stream ToolCall kind={} detail={detail} title={title} id={tool_call_id}",
                    tool_kind_name(kind)
                ),
                other => format!("stream {}", stream_name(other)),
            };
            self.events.lock().push(line);
        }
        fn on_config(&self, options: Vec<crate::ConfigOption>) {
            self.events
                .lock()
                .push(format!("config {}", options.len()));
        }
        fn on_permission_request(&self, request: crate::PermissionRequest) {
            self.events
                .lock()
                .push(format!("permission {}", request.tool_call_id));
        }
        fn on_session_touched(&self, session_id: String, title: String, updated_at: String) {
            self.events
                .lock()
                .push(format!("touched {session_id} {title} {updated_at}"));
        }
        fn on_projects(&self, projects: Vec<crate::ProjectSummary>) {
            self.events
                .lock()
                .push(format!("projects {}", projects.len()));
        }
        fn on_roam_peer_status(&self, label: String, status: String) {
            self.events
                .lock()
                .push(format!("peer_status {label} {status}"));
        }
        fn on_roam_sessions(&self, label: String, sessions: Vec<SessionSummary>) {
            self.events
                .lock()
                .push(format!("peer_sessions {label} {}", sessions.len()));
        }
        fn on_peer_new_session(&self, label: String, session_id: String) {
            self.events
                .lock()
                .push(format!("peer_new_session {label} {session_id}"));
        }

        fn on_active_run(&self, _session_id: String, _run_id: String) {}

        fn on_commands(&self, _commands: Vec<String>) {}
    }

    fn test_listener() -> Arc<RecordingListener> {
        Arc::new(RecordingListener {
            events: Mutex::new(Vec::new()),
        })
    }

    fn gate(value: Arc<AtomicBool>) -> Arc<dyn Fn() -> bool + Send + Sync> {
        Arc::new(move || value.load(Ordering::SeqCst))
    }

    /// A peer without a connection, for state-machine tests.
    fn offline_peer(
        label: &str,
        listener: Arc<dyn CoreListener>,
        is_active: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> (Arc<RoamPeer>, tokio_mpsc::UnboundedReceiver<PeerCommand>) {
        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
        let peer = Arc::new(RoamPeer {
            label: label.to_string(),
            inner: Mutex::new(PeerInner::new()),
            cmd_tx,
            listener,
            is_active,
        });
        (peer, cmd_rx)
    }

    fn list_response(ids: &[(&str, &str, &str)]) -> ListSessionsResponse {
        ListSessionsResponse::new(
            ids.iter()
                .map(|(id, title, updated)| {
                    SessionInfo::new(id.to_string(), "/home/user")
                        .title(title.to_string())
                        .updated_at(updated.to_string())
                })
                .collect(),
        )
    }

    // -- codec ----------------------------------------------------------------

    #[test]
    fn codec_splits_newline_frames() {
        let mut codec = RoamCodec::new();
        let frames = codec.feed(b"{\"a\":1}\n{\"b\":2}\n");
        assert_eq!(frames, vec!["{\"a\":1}", "{\"b\":2}"]);
        assert!(codec.feed(b"").is_empty());
    }

    #[test]
    fn codec_buffers_partial_frames_across_chunks() {
        let mut codec = RoamCodec::new();
        assert!(codec.feed(b"{\"a\":1").is_empty());
        assert!(codec.feed(b",\"b\"").is_empty());
        assert_eq!(codec.feed(b":2}\n"), vec!["{\"a\":1,\"b\":2}"]);
        // No trailing newline: the tail stays buffered until it arrives.
        assert!(codec.feed(b"tail").is_empty());
        assert_eq!(codec.feed(b"\n"), vec!["tail"]);
    }

    #[test]
    fn codec_tolerates_crlf() {
        let mut codec = RoamCodec::new();
        assert_eq!(codec.feed(b"{\"a\":1}\r\n"), vec!["{\"a\":1}"]);
    }

    #[test]
    fn codec_preserves_utf8() {
        let mut codec = RoamCodec::new();
        let bytes = "héllo 🦆".as_bytes();
        let mut chunk = bytes.to_vec();
        chunk.push(b'\n');
        assert_eq!(codec.feed(&chunk), vec!["héllo 🦆"]);
    }

    #[test]
    fn codec_encode_terminates_with_newline() {
        assert_eq!(RoamCodec::encode("{\"a\":1}"), b"{\"a\":1}\n");
        assert_eq!(RoamCodec::encode(""), b"\n");
    }

    // -- browse-mode state transitions ----------------------------------------

    #[test]
    fn browse_list_arrives_no_session_auto_opens() {
        let listener = test_listener();
        let (peer, mut cmd_rx) = offline_peer("laptop", listener.clone(), gate(Arc::new(AtomicBool::new(false))));

        peer.apply_sessions(&list_response(&[("s1", "Title", "2026-01-01T00:00:00Z")]));

        // Ready, sessions stored with the roam:<label>: prefix, no open id.
        assert!(matches!(peer.status(), ConnectionStatus::Ready));
        let sessions = peer.sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "roam:laptop:s1");
        assert_eq!(sessions[0].title, "Title");
        assert_eq!(sessions[0].updated_at, "2026-01-01T00:00:00Z");
        assert!(peer.active_session_id().is_none());
        // Browse mode: the command loop must NOT have queued an open.
        assert!(cmd_rx.try_recv().is_err());
        // Peer-scoped events always emitted.
        let events = listener.events.lock();
        assert!(events.iter().any(|e| e == "peer_status laptop ready"));
        assert!(events.iter().any(|e| e == "peer_sessions laptop 1"));
    }

    #[test]
    fn open_session_queues_load_with_raw_id() {
        let listener = test_listener();
        let (peer, mut cmd_rx) = offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        peer.apply_sessions(&list_response(&[("s1", "Title", "2026-01-01T00:00:00Z")]));

        // Prefixed id (what the UI holds) is stripped back to the raw id.
        peer.open_session("roam:laptop:s1".to_string(), "/home/user".to_string());
        match cmd_rx.try_recv() {
            Ok(PeerCommand::OpenSession { session_id, cwd }) => {
                assert_eq!(session_id, "s1");
                assert_eq!(cwd, "/home/user");
            }
            other => panic!("expected OpenSession command, got {other:?}"),
        }
    }

    #[test]
    fn open_session_ignored_before_ready() {
        let listener = test_listener();
        let (peer, mut cmd_rx) = offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        assert!(matches!(peer.status(), ConnectionStatus::Connecting));
        peer.open_session("s1".to_string(), "/home/user".to_string());
        assert!(cmd_rx.try_recv().is_err());
    }

    #[test]
    fn open_session_falls_back_to_session_cwd_when_blank() {
        let listener = test_listener();
        let (peer, mut cmd_rx) = offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        peer.apply_sessions(&list_response(&[("s1", "Title", "2026-01-01T00:00:00Z")]));

        // A blank cwd (roam-only client, no main last_config) must resolve to
        // the session's own cwd from session/list, not fail the load.
        peer.open_session("s1".to_string(), String::new());
        match cmd_rx.try_recv() {
            Ok(PeerCommand::OpenSession { session_id, cwd }) => {
                assert_eq!(session_id, "s1");
                assert!(!cwd.is_empty());
            }
            other => panic!("expected OpenSession command, got {other:?}"),
        }
    }

    #[test]
    fn new_session_queues_create_with_cwd() {
        let listener = test_listener();
        let (peer, mut cmd_rx) = offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        peer.apply_sessions(&list_response(&[("s1", "Title", "2026-01-01T00:00:00Z")]));
        peer.new_session("/home/user".to_string());
        match cmd_rx.try_recv() {
            Ok(PeerCommand::NewSession { cwd }) => assert_eq!(cwd, "/home/user"),
            other => panic!("expected NewSession command, got {other:?}"),
        }
    }

    #[test]
    fn new_session_ignored_before_ready() {
        let listener = test_listener();
        let (peer, _) = offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        peer.new_session("/home/user".to_string());
        // No Ready, no command queued (peer still Connecting).
    }

    #[test]
    fn applying_a_new_list_replaces_sessions() {
        let listener = test_listener();
        let (peer, _cmd_rx) = offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        peer.apply_sessions(&list_response(&[("s1", "Old", "2026-01-01T00:00:00Z")]));
        peer.apply_sessions(&list_response(&[("s2", "New", "2026-02-01T00:00:00Z")]));
        let sessions = peer.sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "roam:laptop:s2");
    }

    #[test]
    fn wire_params_strips_own_roam_prefix_from_session_id() {
        let listener = test_listener();
        let (peer, _cmd_rx) = offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        // The app/spine address this peer's sessions by the prefixed id; the
        // wire must receive the raw id or the remote cannot resolve it.
        let rewritten = peer.wire_params(json!({
            "sessionId": "roam:laptop:s1",
            "foo": "bar",
        }));
        assert_eq!(rewritten["sessionId"], json!("s1"));
        // Unrelated params pass through untouched.
        assert_eq!(rewritten["foo"], json!("bar"));
    }

    #[test]
    fn wire_params_leaves_foreign_or_raw_ids_alone() {
        let listener = test_listener();
        let (peer, _cmd_rx) = offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        // A raw id (already correct) and another peer's prefix (cannot reach
        // this connection) are not rewritten.
        assert_eq!(peer.wire_params(json!({"sessionId": "s1"}))["sessionId"], json!("s1"));
        assert_eq!(
            peer.wire_params(json!({"sessionId": "roam:other:s2"}))["sessionId"],
            json!("roam:other:s2")
        );
        // A params object without sessionId is returned as-is (same value).
        let params = json!({"cwd": "/home/user"});
        assert_eq!(peer.wire_params(params.clone()), params);
    }

    #[test]
    fn open_session_emits_config_from_load_reply() {
        // The peer's load reply carries `configOptions` (provider/model/effort
        // with the featured model choices); it must reach the listener as
        // on_config — that is the model picker's only source for a peer.
        let reply = json!({
            "sessionId": "s1",
            "configOptions": [{
                "id": "model",
                "name": "Model",
                "currentValue": "deepseek",
                "options": [ { "value": "deepseek", "name": "DeepSeek" } ]
            }]
        });
        let options = crate::spine::parse_config_options(&reply);
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].id, "model");
        assert_eq!(options[0].value, "deepseek");
        assert_eq!(options[0].choices.len(), 1);
        assert_eq!(options[0].choices[0].value, "deepseek");
        assert_eq!(options[0].choices[0].name, "DeepSeek");
    }

    #[test]
    fn dispatch_gates_chat_events_on_active() {
        let listener = test_listener();
        let active = Arc::new(AtomicBool::new(false));
        let (peer, _cmd_rx) = offline_peer("laptop", listener.clone(), gate(active.clone()));
        peer.open("s1".to_string());  // live-transcript path: session must be OPEN

        let notif = SessionNotification::new(
            "s1",
            SessionUpdate::AgentMessageChunk(
                ContentChunk::new(ContentBlock::Text(TextContent::new("hello")))
                    .message_id("m1"),
            ),
        );
        peer.dispatch(notif);

        // Inactive: the bubble accumulates, but no chat events forward.
        assert_eq!(peer.transcript().len(), 1);
        assert_eq!(peer.transcript()[0].content, "hello");
        assert!(!listener.events.lock().iter().any(|e| e.starts_with("stream ")));

        active.store(true, Ordering::SeqCst);
        peer.dispatch(SessionNotification::new(
            "s1",
            SessionUpdate::AgentMessageChunk(
                ContentChunk::new(ContentBlock::Text(TextContent::new(" world")))
                    .message_id("m1"),
            ),
        ));
        let events = listener.events.lock();
        assert!(events.iter().any(|e| e.contains("AgentChunk")));
    }

    #[test]
    fn dispatch_appends_to_existing_bubble() {
        let listener = test_listener();
        let (peer, _cmd_rx) = offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(false))));
        peer.open("s1".to_string());  // live-transcript path
        for text in ["a", "b", "c"] {
            peer.dispatch(SessionNotification::new(
                "s1",
                SessionUpdate::AgentMessageChunk(
                    ContentChunk::new(ContentBlock::Text(TextContent::new(text))).message_id("m1"),
                ),
            ));
        }
        assert_eq!(peer.transcript().len(), 1);
        assert_eq!(peer.transcript()[0].content, "abc");
        // A new message id starts a new bubble.
        peer.dispatch(SessionNotification::new(
            "s1",
            SessionUpdate::AgentMessageChunk(
                ContentChunk::new(ContentBlock::Text(TextContent::new("x"))).message_id("m2"),
            ),
        ));
        assert_eq!(peer.transcript().len(), 2);
    }

    #[test]
    fn dispatch_config_option_update_forwards_when_active() {
        let listener = test_listener();
        let active = Arc::new(AtomicBool::new(false));
        let (peer, _cmd_rx) = offline_peer("laptop", listener.clone(), gate(active.clone()));

        let notif = SessionNotification::new(
            "s1",
            SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(vec![
                agent_client_protocol::schema::v1::SessionConfigOption::select(
                    "model", "Model", "deepseek",
                    Vec::<agent_client_protocol::schema::v1::SessionConfigSelectOption>::new(),
                ),
            ])),
        );
        // Inactive: config change is not forwarded (chat-scoped gate).
        peer.dispatch(notif.clone());
        assert!(!listener.events.lock().iter().any(|e| e.starts_with("config ")));

        // Active: forwarded to on_config.
        active.store(true, Ordering::SeqCst);
        peer.dispatch(notif);
        let events = listener.events.lock();
        assert!(events.iter().any(|e| e == "config 1"));
    }

    #[test]
    fn dispatch_tool_call_kinds() {
        let listener = test_listener();
        let (peer, _cmd_rx) = offline_peer("laptop", listener.clone(), gate(Arc::new(AtomicBool::new(true))));
        peer.open("s1".to_string());  // live-transcript path

        let mut meta = Map::new();
        let mut goose = Map::new();
        goose.insert("toolCall".into(), Value::String("nope".into()));
        meta.insert("goose".into(), Value::Object(goose));
        peer.dispatch(SessionNotification::new(
            "s1",
            SessionUpdate::ToolCall(
                ToolCall::new(ToolCallId::new("t1"), "Run a command")
                    .raw_input(serde_json::json!({"command": "ls"})),
            ),
        ));
        let events = listener.events.lock();
        assert!(events.iter().any(|e| e.contains("ToolCall") && e.contains("Plain") && e.contains("ls")));
    }

    // -- routing --------------------------------------------------------------

    #[test]
    fn routing_picks_the_active_peer_by_label() {
        let listener = test_listener();
        let gate_off = gate(Arc::new(AtomicBool::new(false)));
        let a = offline_peer("alice", listener.clone(), gate_off.clone()).0;
        let b = offline_peer("bob", listener, gate_off).0;
        let peers = vec![a, b];

        assert!(active_peer(&peers, &None).is_none());
        assert!(active_peer(&peers, &Some("unknown".to_string())).is_none());
        let picked = active_peer(&peers, &Some("bob".to_string())).unwrap();
        assert_eq!(picked.label(), "bob");
        // The "last-opened" peer is whoever the spine's slot names; labels are unique.
        let picked = active_peer(&peers, &Some("alice".to_string())).unwrap();
        assert_eq!(picked.label(), "alice");
    }

    #[test]
    fn routing_empty_registry_never_picks() {
        assert!(active_peer(&[], &Some("x".to_string())).is_none());
    }
}
