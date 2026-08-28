// SPDX-License-Identifier: AGPL-3.0-or-later

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
//!
//! Like the main connection, a peer whose link drops unexpectedly re-dials
//! itself: the [`RoamPeer::connect`] task is a supervisor that retries with
//! the core's backoff (500ms·2^n, cap 15s) and, once the handshake lands,
//! resumes the session that was open. Only an explicit [`RoamPeer::close`]
//! (or [`Core::roam_disconnect`]) ends a peer for good.

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
use crate::cache::CacheStore;

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
    /// Hard cap on a single frame (partial-frame buffer) in bytes.
    /// ACP messages are compact JSON well below 1 MiB; the cap exists only to
    /// keep a newline-less peer from growing the buffer without bound.
    pub const MAX_FRAME_BYTES: usize = 1 << 20; // 1 MiB

    /// A fresh codec with an empty partial-frame buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw stream bytes; returns every complete frame (newline removed).
    ///
    /// Memory-bounded (S-RC-2): the partial-frame buffer stops growing at
    /// [`MAX_FRAME_BYTES`](Self::MAX_FRAME_BYTES). ACP messages are compact
    /// JSON orders of magnitude below this, so a newline-less (or single
    /// giant-line) peer cannot grow the buffer without bound — bytes past the
    /// cap are dropped until the next newline re-syncs the codec.
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
                _ => {
                    if self.buf.len() < Self::MAX_FRAME_BYTES {
                        self.buf.push(byte);
                    }
                    // Over-cap: drop the byte and wait for the newline; the
                    // frame is malformed/oversized anyway.
                }
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
    Relist,
    Close,
}

/// Why [`RoamPeer::handshake_and_serve`] returned. The supervisor in
/// [`RoamPeer::connect`] turns these into loop/stop decisions.
enum Outcome {
    /// An explicit `Close` command: the peer is done, `connection_ended` ran.
    Closed,
    /// The dial or handshake failed: re-dial under the backoff budget.
    Retry(String),
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
    /// The receiving half, held in a shared slot so the supervisor can hand
    /// it to each attempt's command loop and reclaim it when the link dies:
    /// the SDK's `connect_with` foreground is `'static` and would otherwise
    /// own the receiver forever, and intents keep queueing commands into
    /// `cmd_tx` across reconnections (each is answered by the NEXT live link,
    /// never lost).
    cmd_rx: Arc<tokio::sync::Mutex<tokio_mpsc::UnboundedReceiver<PeerCommand>>>,
    listener: Arc<dyn CoreListener>,
    /// Gate supplied by the spine: true while this peer owns the active
    /// session. Chat-scoped events forward only under this gate.
    is_active: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Shared transcript cache. Peer transcripts are cached under their
    /// prefixed id (`roam:<label>:<raw>`) — collision-free across machines —
    /// so opening a seen chat paints instantly instead of re-replaying the
    /// whole history over the wire.
    cache: Arc<CacheStore>,
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
    /// A dropped link with a session open sets this: once the re-dial's
    /// handshake lands and `session/list` arrives, the peer re-opens the same
    /// session (the equivalent of the main connection's resume-after-
    /// reconnect). Consumed by `handshake_and_serve`.
    resume_pending: Option<String>,
    /// The live turn's run id (`_meta.goose.activeRunId`); None when no turn
    /// is running. Mirrors the spine's active_run_id.
    active_run: Option<String>,
    /// True while a `session/load` (or reconnect) replay is streaming its
    /// history back. Dedupe is scoped to this: re-delivered staged content must
    /// not double, but LIVE chunks must always append (they never dedupe — that
    /// would swallow legitimately repeated streamed text, the way serve only
    /// pushes text with no containment guard). Cleared when the first live turn
    /// of the session starts.
    replaying: bool,
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
    /// True once the CURRENT attempt's handshake reached Ready
    /// (`apply_sessions`). The supervisor uses it to decide the retry
    /// posture: a peer that was live and then dropped earns a fresh budget
    /// for the outage; one that has NEVER connected gives up on the main
    /// connection's short cold-start budget. Reset at the top of each
    /// attempt.
    ever_ready: bool,
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
            resume_pending: None,
            session_cwds: HashMap::new(),
            active_run: None,
            conn: None,
            transcript: Vec::new(),
            replaying: false,
            pending_permission: None,
            stream: None,
            closing: false,
            ever_ready: false,
            staging: HashMap::new(),
        }
    }
}

impl RoamPeer {
    /// Hard ceiling on a single post-handshake RPC (S-RC-6). A hung remote
    /// goose must not pin a UI/intent thread forever blocking on a reply.
    const RPC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    /// Re-dial budget: the main connection's shape (500ms·2^n, 15s cap — see
    /// [`crate::Core`]'s `schedule_reconnect`), but where main gives up after
    /// 6 tries, a peer keeps trying far longer. A roam host is a laptop that
    /// was simply closed when the phone slept: the drop can last hours, and
    /// the moment the path exists again the user's chats must work without a
    /// visit to the endpoints screen. ~25 attempts ≈ 6 minutes of retrying at
    /// the cap before the peer surfaces a terminal error.
    const RECONNECT_MAX: u32 = 25;

    /// Connect to a roam peer in browse mode, supervised. The dial (blocking,
    /// seconds) and the ACP handshake run on the core runtime; this returns
    /// immediately with a peer in `Connecting` state. `is_active` is the
    /// spine's routing gate (see the module docs). One peer per label: the
    /// caller replaces an existing peer with the same label before connecting
    /// again.
    ///
    /// The task loops: dial → handshake → serve commands. An unexpected
    /// transport end re-dials with exponential backoff and resumes the open
    /// session; an explicit `Close` (or `close()` mid-dial) ends the task.
    pub fn connect(
        secret: String,
        card: String,
        label: String,
        listener: Arc<dyn CoreListener>,
        is_active: Arc<dyn Fn() -> bool + Send + Sync>,
        cache: Arc<CacheStore>,
    ) -> Arc<Self> {
        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
        let peer = Arc::new(Self {
            label: label.clone(),
            inner: Mutex::new(PeerInner::new()),
            cmd_tx,
            cmd_rx: Arc::new(tokio::sync::Mutex::new(cmd_rx)),
            listener,
            is_active,
            cache,
        });
        let task = peer.clone();
        runtime().spawn(async move {
            let mut attempts: u32 = 0;
            let mut ever_connected = false;
            loop {
                // Closed while dialing or sleeping between attempts: the
                // terminal status was already emitted by close().
                if task.inner.lock().closing {
                    return;
                }
                match task.attempt(&secret, &card, &label).await {
                    Outcome::Closed => return,
                    Outcome::Retry(reason) => {
                        if task.inner.lock().closing {
                            return;
                        }
                        let ready_last = task.inner.lock().ever_ready;
                        ever_connected |= ready_last;
                        // A link that reached ready and then dropped starts a
                        // fresh outage budget (the main connection's "reset
                        // on Ready"); attempts only accumulates across
                        // consecutive failures of one outage.
                        if ready_last {
                            attempts = 0;
                        }
                        // A peer that has NEVER connected gives up on the
                        // main connection's short 6-try budget — a bad card
                        // or a gone-forever host should say so in ~90s, not
                        // after five minutes of retries.
                        let budget = if ever_connected { Self::RECONNECT_MAX } else { 6 };
                        if attempts >= budget {
                            task.fail(format!(
                                "{reason} — giving up after {attempts} reconnect attempts"
                            ));
                            return;
                        }
                        let delay = crate::spine::reconnect_delay_ms(attempts);
                        attempts += 1;
                        // Connecting again: a tap on a peer session while the
                        // re-dial is in flight must queue, not vanish (the
                        // open_session/new_session Ready gate).
                        task.inner.lock().status = ConnectionStatus::Connecting;
                        // "connecting:"-prefixed on purpose: the UI treats any
                        // connecting phase as in-flight, and the app's dial
                        // watchdog re-arms on each attempt rather than firing
                        // across the retry sleep. The line is kept short — it
                        // renders beside the peer name on one row.
                        task.emit_status(&format!("connecting: reconnect {attempts}"));
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    }
                }
            }
        });
        peer
    }

    /// One supervised cycle: dial, SDK client, handshake, command loop.
    /// Returns only when the link is done — `Closed` after an explicit
    /// Close/disconnect, `Retry` after any transport end or dial/handshake
    /// failure (the open session was recorded for resume by
    /// [`Self::connection_ended`]).
    async fn attempt(self: &Arc<Self>, secret: &str, card: &str, label: &str) -> Outcome {
        // Fresh readiness bookkeeping per link.
        self.inner.lock().ever_ready = false;
        // 1. Dial (blocking, seconds) off the runtime.
        self.emit_status("connecting: dialing");
        let dialed = tokio::task::spawn_blocking({
            let secret = secret.to_string();
            let card = card.to_string();
            let label = label.to_string();
            move || grouse_roam_core::roam_connect(&secret, &card, Some(label))
        })
        .await;
        let stream = match dialed {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => return Outcome::Retry(format!("roam connect: {error}")),
            Err(error) => {
                return Outcome::Retry(format!("roam dial task panicked: {error}"))
            }
        };
        // Closed while dialing? Hand the stream back immediately.
        if self.inner.lock().closing {
            stream.shutdown();
            stream.cancel();
            return Outcome::Closed;
        }
        self.inner.lock().stream = Some(stream.clone());

        // 2. The SDK client: notifications + server requests dispatched to
        //    the listener; requests answered via the peer.
        let notif_peer = self.clone();
        let req_peer = self.clone();
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

        // 3. Connect: initialize -> session/list (browse) -> resume-if-owed,
        //    then serve commands until close.
        let main_task = self.clone();
        let transport_stream = stream.clone();
        let result = builder
            .connect_with(
                RoamTransport::new(stream),
                async move |cx: agent_client_protocol::ConnectionTo<Agent>| {
                    main_task.handshake_and_serve(cx).await
                },
            )
            .await;
        match result {
            // The command loop broke on Close (or the peer's receiver was
            // dropped): terminal. Route through the same teardown seam as a
            // drop; `closing` was already set by close() for the explicit
            // path, so this only lands as "disconnected" for the stray case.
            Ok(()) => {
                self.connection_ended(Ok(()));
                Outcome::Closed
            }
            // Transport end or handshake failure: record the loss and let the
            // supervisor decide (retry vs give up). `connection_ended` stays
            // the single teardown seam the tests and close paths use.
            Err(error) => {
                let reason = error.to_string();
                self.connection_ended(Err(error));
                // `transport_stream` is our clone of the same dialed stream:
                // a handshake that FAILED on an otherwise-healthy byte stream
                // parked in `read()` forever — one leaked blocking-pool thread
                // plus one live QUIC connection per retry. FIN + cancel wakes
                // it; on a transport that already errored this is a no-op.
                let s = transport_stream.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    s.shutdown();
                    s.cancel();
                })
                .await;
                Outcome::Retry(reason)
            }
        }
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
                archived: false,
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
    ///
    /// The Ready gate also accepts Connecting: the supervised reconnect keeps
    /// the command channel alive, so a tap landing in the re-dial window is
    /// answered by the next live link instead of vanishing. A truly dead peer
    /// (Disconnected/Error after the supervisor gives up) must NOT accept
    /// them — nothing will ever drain the queue.
    pub fn open_session(&self, session_id: String, cwd: String) {
        match self.status() {
            ConnectionStatus::Ready | ConnectionStatus::Connecting => {}
            _ => return,
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
    /// Same Ready-or-Connecting gate: queued commands ride the reconnect.
    pub fn new_session(&self, cwd: String) {
        match self.status() {
            ConnectionStatus::Ready | ConnectionStatus::Connecting => {}
            _ => return,
        }
        let _ = self.cmd_tx.send(PeerCommand::NewSession { cwd });
    }

    /// Disconnect the peer: FIN + cancel the stream (unblocks the reader),
    /// Re-fetch this peer's session list (after a session mutation such as
    /// rename/archive/delete). The remote goose does not reliably notify us of
    /// our own mutation, so the peer re-lists explicitly to keep its drawer
    /// accurate. Fire-and-forget into the command loop.
    pub fn relist(&self) {
        let _ = self.cmd_tx.send(PeerCommand::Relist);
    }

    /// Disconnect the peer: FIN + cancel the stream (unblocks the reader),
    /// shut down the command loop, and report the terminal status.
    pub fn close(&self) {
        // Before tearing anything down: an explicit disconnect is an exit from
        // the open session just as much as a switch is.
        self.save_open_transcript();
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

    /// Persist the currently open session's transcript.
    ///
    /// Called on every exit from a session, not just a switch to another one.
    /// It used to run ONLY when `open` replaced a previous session, so a peer
    /// chat opened and then left by killing the app was never written at all —
    /// the cache only survived if you happened to open a second session on the
    /// same peer in the same run, which is why peer transcripts looked like they
    /// were not cached across restarts.
    pub fn save_open_transcript(&self) {
        let inner = self.inner.lock();
        let Some(open) = inner.open_session_id.clone() else { return };
        let key = self.cache_key(&open);
        let transcript = inner.transcript.clone();
        let updated = inner
            .sessions
            .iter()
            .find(|s| s.id.ends_with(&format!(":{open}")))
            .map(|s| s.updated_at.clone())
            .unwrap_or_default();
        drop(inner);
        self.cache.save_transcript(&key, &transcript, &updated);
    }

    /// How many trailing messages `session/load` should replay for this
    /// session, or `None` for a full replay.
    ///
    /// A tail is requested only when the cached transcript provably covers
    /// the session's current state: the `updatedAt` stamped into the cache at
    /// save time equals the session's `updatedAt` from `session/list`. On any
    /// mismatch — the session moved while we were away, no cache, no stamp —
    /// the full replay runs, because a tail with an unknown gap behind it
    /// would leave missing middle messages in the transcript forever (the
    /// merge dedupes overlap; it cannot detect absence). Servers without
    /// `replayTail` support ignore the meta and replay in full.
    fn replay_tail(&self, raw_session_id: &str) -> Option<usize> {
        let listed = {
            let inner = self.inner.lock();
            let suffix = format!(":{raw_session_id}");
            inner
                .sessions
                .iter()
                .find(|s| s.id.ends_with(&suffix))
                .map(|s| s.updated_at.clone())
        }?;
        let (_, cached_at) = self.cache.load_transcript(&self.cache_key(raw_session_id))?;
        replay_tail_decision(&cached_at, &listed)
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
        // S-RC-6: a hung remote goose must not pin the calling thread forever.
        // The request task is detached; if we time out, its eventual reply is
        // simply dropped.
        // End-of-turn fan-out, shared by the success, failure, and timeout
        // arms: the turn's completion rides the session/prompt REPLACE (the
        // reply's stopReason) — the main connection surfaces it via
        // store.run_ended; the peer has no store, so emit RunEnded straight
        // to the listener so the app clears its in-flight/turnInFlight state
        // and drains the queue.
        let end_turn = |stop: &str| {
            // Emit RunEnded UNCONDITIONALLY, not just when this peer owns the
            // screen. The turn has ended regardless of what the user is viewing;
            // gating it on `is_active` let the app keep busy=true + a stale
            // activeRunId when the turn finished off-screen — and because that
            // state is app-global, one wedged roam turn then made EVERY chat
            // steer a dead run ("no turn exists to steer"). The app matches the
            // completion to its own session/queue.
            self.emit_stream(StreamEvent::RunEnded {
                stop_reason: stop.to_string(),
            });
            // The peer's turn bookkeeping mirrors the spine: a live run id is
            // now over, whether or not the server sent an activeRunId update.
            self.inner.lock().active_run = None;
        };
        let result = match rx.recv_timeout(Self::RPC_TIMEOUT) {
            Ok(inner) => inner,
            Err(e) => {
                let detail = match e {
                    std::sync::mpsc::RecvTimeoutError::Timeout => "roam rpc timed out",
                    std::sync::mpsc::RecvTimeoutError::Disconnected => "roam rpc task failed",
                };
                let error = AcpError::internal_error().data(detail);
                // A prompt that times out (the peer vanished mid-turn without a
                // clean FIN, so send_request hangs and never errors) must still
                // end the turn — the app clears its in-flight state only on
                // RunEnded, so skipping this left it busy forever.
                if method == "session/prompt" {
                    end_turn("error");
                }
                return Err(error);
            }
        };
        let result = match result {
            Ok(reply) => reply,
            Err(error) => {
                // A prompt that fails must still end the turn so the UI can
                // clear in-flight state and drain the queue (mirrors spine's
                // run_ended("error") — the desktop wedges here).
                if method == "session/prompt" {
                    end_turn("error");
                }
                return Err(error);
            }
        };
        if method == "session/prompt" {
            let stop = result
                .get("stopReason")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            end_turn(&stop);
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

    /// The browse-mode handshake + command loop, run inside `connect_with`'s
    /// foreground: initialize → `session/list` → resume the session that was
    /// open when the link last dropped, if any. Then serve commands until
    /// `Close`. Handshake failures log and return `Err` WITHOUT surfacing an
    /// error status — during supervised reconnect the supervisor owns the
    /// status line ("reconnecting: …"); only a final give-up or an explicit
    /// close surfaces a terminal status.
    async fn handshake_and_serve(
        self: &Arc<Self>,
        cx: agent_client_protocol::ConnectionTo<Agent>,
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
        match init {
            Ok(Ok(_reply)) => {}
            Ok(Err(error)) => {
                eprintln!("grouse-core: roam peer '{}' initialize: {error}", self.label);
                return Err(AcpError::internal_error()
                    .data(format!("roam initialize: {error}")));
            }
            Err(_) => {
                eprintln!(
                    "grouse-core: roam peer '{}' handshake timed out",
                    self.label
                );
                return Err(AcpError::internal_error().data(
                    "handshake timed out waiting for initialize reply",
                ));
            }
        }

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
                eprintln!("grouse-core: roam peer '{}' session/list: {error}", self.label);
                return Err(AcpError::internal_error()
                    .data(format!("roam session/list: {error}")));
            }
            Err(_) => {
                eprintln!(
                    "grouse-core: roam peer '{}' session/list timed out",
                    self.label
                );
                return Err(AcpError::internal_error().data(
                    "handshake timed out waiting for session/list reply",
                ));
            }
        };
        self.apply_sessions(&list);

        // Resume owed from a supervised reconnect: re-open the session that
        // was live when the link died (the peer mirror of the main
        // connection's resume-after-reconnect). Consumed BEFORE loading so a
        // session that can't be re-opened can't wedge every later reconnect;
        // `load_session`'s stale-session fallback keeps the chat working.
        let resume = self.inner.lock().resume_pending.take();
        if let Some(raw) = resume {
            let cwd = self
                .inner
                .lock()
                .session_cwds
                .get(&raw)
                .cloned()
                .unwrap_or_default();
            if let Err(error) = self.load_session(&cx, raw, cwd).await {
                self.fail(format!("roam resume: {error}"));
            }
        }

        // The command queue lives on the PEER, not in this future: when the
        // transport dies the SDK drops this future mid-`recv`, and a receiver
        // owned here would die with it. Intents keep buffering in `cmd_tx`
        // across the outage; the next attempt picks the queue back up here.
        let mut cmd_rx = self.cmd_rx.lock().await;
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                PeerCommand::OpenSession { session_id, cwd } => {
                    let raw = self.strip_prefix(&session_id);
                    if let Err(error) = self.load_session(&cx, raw, cwd).await {
                        self.fail(format!("roam session/open: {error}"));
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
                PeerCommand::Relist => {
                    // A session mutation (rename/archive/delete) landed on this
                    // peer; re-fetch its list so the drawer reflects it. The
                    // remote goose does not reliably notify us of our own
                    // mutation, so we re-list explicitly.
                    let list = cx
                        .send_request(ListSessionsRequest::new().meta(session_list_meta()))
                        .block_task()
                        .await;
                    if let Ok(list) = list {
                        self.apply_sessions(&list);
                    }
                }
                PeerCommand::Close => break,
            }
        }
        Ok(())
    }

    /// `session/load` a (raw) session id on this live connection and make it
    /// the open chat. A stale/archived session can't be resumed — fall back
    /// to a fresh session rather than leaving the chat wedged on the failed
    /// load (desktop `response()` behavior). Shared by the user's tap (the
    /// OpenSession command) and the reconnect resume.
    ///
    /// Untyped sends: the raw reply `Value` is needed both to surface the
    /// peer's own {provider, model, effort} config (the app's model picker
    /// for this peer comes from here, not from the main connection) and to
    /// derive the new session id on the fallback path. mcpServers is REQUIRED
    /// by the remote deserializer (same strictness as session/new — missing
    /// field = hard error).
    /// The `Result` carries only the JSON-RPC envelope construction; every
    /// wire-level load failure is handled (or surfaced) internally.
    async fn load_session(
        &self,
        cx: &agent_client_protocol::ConnectionTo<Agent>,
        raw: String,
        cwd: String,
    ) -> Result<(), AcpError> {
        let mut params = json!({ "sessionId": raw, "cwd": cwd, "mcpServers": [] });
        // Bounded replay when the cache is current (see `replay_tail`). This
        // covers BOTH paths that reach load_session: a user tap (OpenSession)
        // and a reconnect resume — the resume dedupes the tail against the
        // transcript the peer kept across the drop, same as the tap does
        // against the cache.
        if let Some(tail) = self.replay_tail(&raw) {
            params["_meta"] = json!({ "replayTail": tail });
        }
        let load = cx
            .send_request(UntypedMessage::new("session/load", params)?)
            .block_task()
            .await;
        match load {
            Ok(reply) => {
                self.open(raw);
                self.emit_status("ready");
                self.emit_config(&reply);
            }
            Err(error) => {
                // Fallback: the tapped session is stale/archived — a fresh
                // session is better than a wedged chat. NO
                // on_peer_new_session here: that event opens the app's UI,
                // and the user asked for THIS session (the fallback silently
                // serves a proxy).
                match self.create_session(cx, &cwd, false).await {
                    Ok(()) => {}
                    Err(_) => {
                        self.fail(format!("roam session/load: {error}"));
                    }
                }
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
            // Seed each listed session's staging from the transcript cache:
            // a seen chat then paints instantly on open (its chunks were
            // staged here), no waiting on the wire replay.
            for s in &list.sessions {
                let raw = s.session_id.to_string();
                if inner.staging.contains_key(&raw) {
                    continue;
                }
                let key = self.cache_key(&raw);
                if let Some((messages, _)) = self.cache.load_transcript(&key) {
                    inner.staging.insert(raw, StagedSession { messages, has_new: false });
                }
            }
            inner.sessions = sessions;
            inner.session_cwds = cwds;
            inner.status = ConnectionStatus::Ready;
            // The supervisor's outage posture keys on this: a link that has
            // been live earns the long budget and a fresh one per outage.
            inner.ever_ready = true;
        }
        self.emit_sessions(self.sessions());
        self.emit_status("ready");
    }

    /// A session/load succeeded: the peer owns this session now, and its
    /// transcript starts from whatever we already have for it.
    ///
    /// Content comes from staging (chunks that arrived while the session was
    /// closed), else straight from the transcript cache, else empty. Reading
    /// the cache HERE rather than relying on `apply_sessions` having seeded
    /// staging means a session opened before the peer's session/list lands
    /// still paints instantly.
    ///
    /// Emits exactly ONE Clear and no Appends — the contract the main path's
    /// `TranscriptStore::replace` uses, where a Clear means "rebuild from
    /// `transcript()`". Emitting an Append per row on top of that made the UI
    /// paint the snapshot once from the rebuild and again from the events, so
    /// every cached message doubled on open.
    fn open(&self, raw_session_id: String) {
        // Persist the PREVIOUS open session's transcript before switching: the
        // next time it's opened, the cache paints it instantly.
        self.save_open_transcript();
        // Off-lock: the cache is file I/O and `inner` guards the live dispatch.
        let cached = self
            .cache
            .load_transcript(&self.cache_key(&raw_session_id))
            .map(|(messages, _)| messages);
        {
            let mut inner = self.inner.lock();
            inner.open_session_id.take();
            let staged = inner.staging.remove(&raw_session_id).map(|s| s.messages);
            inner.transcript = staged.or(cached).unwrap_or_default();
            inner.open_session_id = Some(raw_session_id);
            // The session/load replay that follows re-delivers the promoted
            // (or cached) content; dedupe against it during this replay only,
            // cleared once a live turn begins (dispatch SessionInfoUpdate).
            inner.replaying = true;
            // Promotion (or simply visiting) clears the green dot: has_new is
            // gone with the staging entry, and sessions() re-reads it live.
        }
        // Unconditional, empty session included: otherwise the previous chat's
        // rows stay on screen.
        self.emit(TranscriptEvent::Clear);
    }

    /// The link ended. `Ok` = clean (close command, or the peer was torn
    /// down). `Err` = the transport dropped: the supervisor in
    /// [`Self::connect`] decides whether to re-dial, so this records the
    /// teardown (transcript save, conn drop, resume owed, dead run cleared)
    /// and stays SILENT on a drop — the supervisor owns the status line from
    /// here ("connecting: reconnecting…" or the final give-up).
    fn connection_ended(&self, result: Result<(), AcpError>) {
        // A dropped link loses the session as surely as closing it does; if
        // close() already ran this is a cheap no-op rewrite of the same rows.
        self.save_open_transcript();
        let mut inner = self.inner.lock();
        inner.conn = None;
        if inner.closing {
            inner.status = ConnectionStatus::Disconnected;
            return;
        }
        inner.status = ConnectionStatus::Disconnected;
        if result.is_err() {
            // Resume owed: remember the open chat so the re-dial re-opens it
            // (the peer mirror of the main connection's resume-after-
            // reconnect). A run in flight died with the link: clear it and
            // emit the empty run id so the app frees busy/steer state.
            inner.resume_pending = inner.open_session_id.clone();
            let dead_run = inner.active_run.take().is_some();
            let open = inner.open_session_id.clone();
            drop(inner);
            if dead_run {
                if let Some(raw) = open {
                    self.listener
                        .on_active_run(format!("roam:{}:{raw}", self.label), String::new());
                }
            }
            return;
        }
        drop(inner);
        self.emit_status("disconnected");
    }

    /// Set an error status and surface it (used by dial/handshake failures).
    fn fail(&self, message: String) {
        // The UI elides roam status, so also log the raw failure for diagnosis.
        eprintln!("grouse-core: roam peer '{}' failed: {message}", self.label);
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

    /// Cache key for a peer session: the prefixed id. Collision-free across
    /// machines (the main cache keys on bare sessionId, which two machines
    /// could share) — peer transcripts live under `roam:<label>:<raw>`.
    fn cache_key(&self, raw: &str) -> String {
        format!("roam:{}:{raw}", self.label)
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
                            // The load/reconnect replay is done; a live turn is
                            // streaming now. From here on chunks never dedupe
                            // (that would swallow legitimately repeated text).
                            self.inner.lock().replaying = false;
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
                // Keep this peer's own session list current: session/list only
                // runs on connect and relist, so without this the stored
                // updatedAt goes stale the moment a turn runs — and
                // save_open_transcript stamps the cache from here, which is
                // what lets replay_tail prove the cache is current on the
                // next open.
                if !title.is_empty() || !updated_at.is_empty() {
                    let mut inner = self.inner.lock();
                    let suffix = format!(":{session_id}");
                    if let Some(s) = inner.sessions.iter_mut().find(|s| s.id.ends_with(&suffix)) {
                        if !title.is_empty() {
                            s.title = title.clone();
                        }
                        if !updated_at.is_empty() {
                            s.updated_at = updated_at.clone();
                        }
                    }
                }
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
        let replaying = inner.replaying;   // snapshot before the target borrow
        let mut staged_new = false;
        let target: &mut Vec<Message> = if mine {
            &mut inner.transcript
        } else {
            let st = inner.staging.entry(session_id.to_string()).or_default();
            staged_new = !st.has_new;
            st.has_new = true;
            &mut st.messages
        };
        let event = if message_id.is_empty() {
            // Live text chunks have no stable id. Serve keeps a tracked stream
            // bubble; roam mirrors that by appending to the LAST message ONLY
            // when it is still the open stream for this role — i.e. it is this
            // role and still empty-id. A tool/thought/user bubble in between (or
            // a prior turn's bubble) means the stream closed: start a fresh
            // bubble instead of merging non-contiguous text (which garbled the
            // transcript and swallowed thinking blocks).
            let msg = target.last_mut().filter(|m| m.role == role && m.id.is_empty());
            match msg {
                // Live chunks normally append unconditionally (serve parity).
                // But when a live stream stalls and the server re-delivers the
                // already-shown tail from its checkpoint, appending it again
                // restarts the paragraph ("stops mid-paragraph, starts over
                // beneath it"). Skip that by treating a full-suffix re-delivery
                // as already-on-screen — it only fires for an EXACT tail match,
                // never for a legitimate mid-paragraph repeat.
                Some(m)
                    if (replaying && m.content.contains(text))
                        || (!replaying && m.content.ends_with(text) && m.content != text) => None,
                Some(m) => {
                    m.content.push_str(text);
                    Some(TranscriptEvent::Update {
                        message: Message {
                            id: m.id.clone(),
                            role: m.role.clone(),
                            content: m.content.clone(),
                            output: String::new(),
                        },
                    })
                }
                _ => None,
            }
        } else if let Some(msg) = target
            .iter_mut()
            .find(|m| m.role == role && m.id == message_id)
        {
            // Replay dedupe: a staged message being re-sent by session/load
            // is identical — appending it again would double the text. Live
            // chunks (replaying==false) never dedupe.
            if !replaying || !msg.content.contains(text) {
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
            None
        };
        let event = event.or_else(|| {
            // No open stream bubble matched (empty id, or a new stream after a tool/
            // thought/other role, or a fresh id): start a new bubble.
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
        });
        cap_messages(target);
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
        cap_messages(target);
        drop(inner);
        if !mine && staged_new {
            self.staged_touch(session_id);
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
        let replaying = inner.replaying;   // snapshot before the target borrow
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
        // replay re-delivering already-staged output must not double it (live
        // chunks always append — same rule as accumulate).
        let result = if let Some(msg) = target
            .iter_mut()
            .find(|m| m.role == "tool" && m.id == tool_call_id)
        {
            if !replaying || !msg.output.contains(output) {
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
        cap_messages(target);
        drop(inner);
        if !mine && staged_new {
            self.staged_touch(session_id);
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
                kind: crate::spine::permission_option_kind_str(&o.kind).to_string(),
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

/// The text of a chunk's content block; non-text blocks render as a marker.
/// Delegates to the spine's SINGLE shared translator (RC-3) so roam and the
/// spine render text/image/audio/resource identically (the old local copy
/// drifted and rendered image/audio as `[unknown]`).
fn chunk_text(block: &agent_client_protocol::schema::v1::ContentBlock) -> Option<String> {
    // Preserves the original contract: every block renders to SOME text.
    Some(crate::spine::content_block_text(block))
}

/// ToolCall → `(ToolCallKind, detail)`, collapsing the desktop's mcpapp/chart
/// split into `ToolCallKind` (CONTRACT §3.4). Delegates to the spine's SINGLE
/// shared translator (RC-3) so roam and the spine produce identical kinds and
/// detail strings.
fn tool_kind(tool: &agent_client_protocol::schema::v1::ToolCall) -> (ToolCallKind, String) {
    crate::spine::tool_call_kind(tool)
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
        .map(crate::spine::tool_call_status_str)
        .unwrap_or_default()
        .to_string();
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
        archived: false,
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

/// Route a chat intent (CONTRACT §6: chat routes to the last-opened session's
/// owner). Pick the peer that owns the active session: the one whose label
/// equals `active_label`. `None` when no roam session is active (chat routes
/// to the main connection). The spine calls this from its chat/unstable intents.
pub fn active_peer<'a>(
    peers: &'a [Arc<RoamPeer>],
    active_label: &Option<String>,
) -> Option<&'a Arc<RoamPeer>> {
    let label = active_label.as_ref()?;
    peers.iter().find(|peer| &peer.label == label)
}

/// Trailing messages requested from `session/load` when the cache is current
/// (see [`RoamPeer::replay_tail`]). Counted in server-side messages (tool
/// requests and responses included), so this covers a few heavy agentic turns;
/// the server widens to the nearest turn boundary. Large enough that the
/// replayed window always overlaps the cached tail it dedupes against.
const REPLAY_TAIL: usize = 100;

/// The stamp comparison behind [`RoamPeer::replay_tail`], separated for
/// testing: a tail only when both stamps exist and agree.
fn replay_tail_decision(cached_at: &str, listed_at: &str) -> Option<usize> {
    (!cached_at.is_empty() && cached_at == listed_at).then_some(REPLAY_TAIL)
}

/// Cap a roam-side message list (S-RC-4) so a pathological session cannot grow
/// memory without bound — and, because the backward `.find()` scans for tool
/// updates are bounded by this cap, the per-update cost cannot scale with
/// unbounded history. Evicts the OLDEST messages in bulk to a watermark
/// (amortized O(1) per append).
fn cap_messages(messages: &mut Vec<Message>) {
    const MAX: usize = 2000;
    if messages.len() <= MAX {
        return;
    }
    let keep = MAX - MAX / 4;
    let drop = messages.len() - keep;
    messages.drain(0..drop);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::{
        ConfigOptionUpdate, ContentBlock, ContentChunk, SessionInfoUpdate, TextContent, ToolCall,
        ToolCallId,
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

        fn on_active_run(&self, session_id: String, run_id: String) {
            self.events
                .lock()
                .push(format!("active_run {session_id} {run_id}"));
        }

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
    static TEST_PEER_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn offline_peer(
        label: &str,
        listener: Arc<dyn CoreListener>,
        is_active: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> (Arc<RoamPeer>, tokio_mpsc::UnboundedReceiver<PeerCommand>) {
        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
        // The supervisor slot gets its own (unused) channel: these tests drive
        // the state machine directly and assert on `cmd_rx` returned below,
        // never through a live command loop.
        let (_slot_tx, slot_rx) = tokio_mpsc::unbounded_channel();
        let peer = Arc::new(RoamPeer {
            label: label.to_string(),
            inner: Mutex::new(PeerInner::new()),
            cmd_tx,
            cmd_rx: Arc::new(tokio::sync::Mutex::new(slot_rx)),
            listener,
            is_active,
            // A dir per PEER, not per label: `open` reads the transcript cache
            // now, so peers sharing a dir would read each other's rows — tests
            // that happen to use the same session id would leak into each
            // other (they did, the moment open started reading the cache).
            cache: Arc::new(CacheStore::new(std::env::temp_dir().join(format!(
                "grouse-roam-test-{label}-{}-{}",
                std::process::id(),
                TEST_PEER_SEQ.fetch_add(1, Ordering::SeqCst)
            )))),
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
    fn open_session_queues_while_connecting() {
        // Parity with the main connection: a tap during the supervised
        // reconnect window must queue, not vanish — the next live link drains
        // the command channel. Only a DEAD peer (after the supervisor gave
        // up) drops opens.
        let listener = test_listener();
        let (peer, mut cmd_rx) = offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        assert!(matches!(peer.status(), ConnectionStatus::Connecting));
        peer.open_session("s1".to_string(), "/home/user".to_string());
        match cmd_rx.try_recv() {
            Ok(PeerCommand::OpenSession { session_id, .. }) => assert_eq!(session_id, "s1"),
            other => panic!("expected OpenSession queued while connecting, got {other:?}"),
        }
        // Dead peer: nothing will drain the queue, so the intent is rejected.
        peer.connection_ended(Ok(()));
        assert!(matches!(peer.status(), ConnectionStatus::Disconnected));
        peer.open_session("s2".to_string(), "/home/user".to_string());
        assert!(cmd_rx.try_recv().is_err(), "a disconnected peer must not accept opens");
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
    fn apply_sessions_seeds_staging_from_cache() {
        use crate::Message;
        let listener = test_listener();
        let (peer, _) = offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        // Persist a transcript for s1 under the prefixed cache key, then list:
        // apply_sessions must seed it into staging so open() paints instantly.
        let m = Message {
            id: "msg1".to_string(),
            role: "agent".to_string(),
            content: "hello".to_string(),
            output: String::new(),
        };
        assert!(peer.cache.save_transcript("roam:laptop:s1", &[m], "2026-01-01T00:00:00Z"));
        peer.apply_sessions(&list_response(&[("s1", "Title", "2026-01-01T00:00:00Z")]));
        let n = peer
            .inner
            .lock()
            .staging
            .get("s1")
            .map(|st| st.messages.len())
            .unwrap_or(0);
        assert_eq!(n, 1, "s1 should be staged from the cache");
    }

    #[test]
    fn open_emits_cached_snapshot_instantly() {
        use crate::Message;
        let listener = test_listener();
        let (peer, _) = offline_peer("laptop", listener.clone(), gate(Arc::new(AtomicBool::new(true))));
        let m = Message {
            id: "msg1".to_string(),
            role: "agent".to_string(),
            content: "hello".to_string(),
            output: String::new(),
        };
        assert!(peer.cache.save_transcript("roam:laptop:s1", &[m], "2026-01-01T00:00:00Z"));
        peer.apply_sessions(&list_response(&[("s1", "Title", "2026-01-01T00:00:00Z")]));
        peer.open("s1".to_string());
        // ONE Clear and NO Appends: a Clear tells the UI to rebuild from
        // transcript(), so an Append per row on top of it painted the cached
        // snapshot twice. The rows must be in transcript() for that rebuild.
        {
            let events = listener.events.lock();
            let clears = events.iter().filter(|e| e.as_str() == "transcript Clear").count();
            let appends = events.iter().filter(|e| e.as_str() == "transcript Append").count();
            assert_eq!(clears, 1, "exactly one Clear: {events:?}");
            assert_eq!(appends, 0, "a Clear rebuilds from transcript(); no Appends: {events:?}");
        }
        let snapshot = peer.transcript();
        assert_eq!(snapshot.len(), 1, "the cached row is what the rebuild reads");
        assert_eq!(snapshot[0].content, "hello");
    }

    /// Opening a session the peer has never listed still paints: the cache is
    /// read on open, not only when apply_sessions seeds staging.
    #[test]
    fn open_paints_from_cache_before_any_session_list() {
        use crate::Message;
        let listener = test_listener();
        let (peer, _) = offline_peer("laptop", listener.clone(), gate(Arc::new(AtomicBool::new(true))));
        let m = Message {
            id: "msg1".to_string(),
            role: "agent".to_string(),
            content: "from the cache".to_string(),
            output: String::new(),
        };
        assert!(peer.cache.save_transcript("roam:laptop:s9", &[m], "2026-01-01T00:00:00Z"));
        // NO apply_sessions call: nothing has been staged for s9.
        peer.open("s9".to_string());
        let snapshot = peer.transcript();
        assert_eq!(snapshot.len(), 1, "cache must paint without a prior session/list");
        assert_eq!(snapshot[0].content, "from the cache");
    }

    /// An empty session must still clear the previous chat off screen.
    #[test]
    fn open_an_empty_session_clears_the_previous_one() {
        let listener = test_listener();
        let (peer, _) = offline_peer("laptop", listener.clone(), gate(Arc::new(AtomicBool::new(true))));
        peer.open("s-empty".to_string());
        let events = listener.events.lock();
        assert_eq!(
            events.iter().filter(|e| e.as_str() == "transcript Clear").count(),
            1,
            "an empty session still emits the Clear: {events:?}"
        );
    }

    #[test]
    fn unexpected_drop_records_resume_and_stays_silent() {
        // The supervisor owns the status line while it re-dials: a drop must
        // NOT surface "disconnected"/"error" (that was the old terminal
        // behavior that made peers look dead), only record the resume and
        // free the app's turn state.
        let listener = test_listener();
        let (peer, _cmd_rx) = offline_peer("laptop", listener.clone(), gate(Arc::new(AtomicBool::new(true))));
        peer.apply_sessions(&list_response(&[("s1", "Title", "2026-01-01T00:00:00Z")]));
        peer.open("s1".to_string());
        peer.inner.lock().active_run = Some("run-9".to_string());

        peer.connection_ended(Err(agent_client_protocol::Error::internal_error()
            .data("roam stream ended")));

        assert!(matches!(peer.status(), ConnectionStatus::Disconnected));
        assert_eq!(
            peer.inner.lock().resume_pending.as_deref(),
            Some("s1"),
            "the open session must be owed to the next live link"
        );
        let events = listener.events.lock();
        assert!(
            !events.iter().any(|e| e.starts_with("peer_status laptop disconnected")
                || e.starts_with("peer_status laptop error")),
            "a supervised drop emits no terminal status: {events:?}"
        );
        // The run died with the link: the empty run id frees the app's busy state.
        assert!(
            events
                .iter()
                .any(|e| e == "active_run roam:laptop:s1 "),
            "the dead run id must be cleared for the app: {events:?}"
        );
        assert!(peer.inner.lock().active_run.is_none());
    }

    #[test]
    fn clean_end_is_terminal_and_owes_nothing() {
        let listener = test_listener();
        let (peer, _cmd_rx) = offline_peer("laptop", listener.clone(), gate(Arc::new(AtomicBool::new(true))));
        peer.apply_sessions(&list_response(&[("s1", "Title", "2026-01-01T00:00:00Z")]));
        peer.open("s1".to_string());

        peer.connection_ended(Ok(()));

        assert_eq!(
            peer.inner.lock().resume_pending,
            None,
            "a deliberate close owes no resume"
        );
        let events = listener.events.lock();
        assert!(
            events.iter().any(|e| e == "peer_status laptop disconnected"),
            "a clean end surfaces disconnected: {events:?}"
        );
    }

    #[test]
    fn reconnect_delay_curve() {
        // The shared curve both the main connection and the peer supervisor
        // use: 500ms·2^n capped at 15s.
        assert_eq!(crate::spine::reconnect_delay_ms(0), 500);
        assert_eq!(crate::spine::reconnect_delay_ms(1), 1000);
        assert_eq!(crate::spine::reconnect_delay_ms(4), 8000);
        assert_eq!(crate::spine::reconnect_delay_ms(5), 15000);
        assert_eq!(crate::spine::reconnect_delay_ms(9), 15000);
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
    fn relist_queues_relist_command() {
        // Regression guard for the roam session-mutation fix: after a
        // rename/archive/delete lands on a peer, the core re-lists that peer's
        // sessions so its drawer reflects the change. `relist()` must queue the
        // Relist command (the command loop re-fetches + applies the list).
        let listener = test_listener();
        let (peer, mut cmd_rx) = offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        peer.relist();
        match cmd_rx.try_recv() {
            Ok(PeerCommand::Relist) => {}
            other => panic!("expected Relist command, got {other:?}"),
        }
    }

    #[test]
    fn new_session_queues_while_connecting_rejects_dead() {
        let listener = test_listener();
        let (peer, mut cmd_rx) = offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        peer.new_session("/home/user".to_string());
        match cmd_rx.try_recv() {
            Ok(PeerCommand::NewSession { cwd }) => assert_eq!(cwd, "/home/user"),
            other => panic!("expected NewSession queued while connecting, got {other:?}"),
        }
        peer.connection_ended(Ok(()));
        peer.new_session("/home/user".to_string());
        assert!(cmd_rx.try_recv().is_err(), "a disconnected peer must not accept creates");
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
    fn dispatch_live_turn_keeps_thinking_separate_and_does_not_garble() {
        let listener = test_listener();
        let (peer, _cmd_rx) = offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(false))));
        peer.open("s1".to_string());  // live-transcript path

        // Real turn shape: thinking chunks (empty id) → tool call → final answer (empty id).
        // The final answer must NOT merge into the thinking bubble (garbling), and the
        // thinking must not be swallowed by the replay `contains` dedupe.
        for t in ["think one", " think two"] {
            peer.dispatch(SessionNotification::new(
                "s1",
                SessionUpdate::AgentThoughtChunk(
                    ContentChunk::new(ContentBlock::Text(TextContent::new(t))),
                ),
            ));
        }
        peer.dispatch(SessionNotification::new(
            "s1",
            SessionUpdate::ToolCall(ToolCall::new(ToolCallId::new("tool1"), "shell")),
        ));
        let t = "answer ";
        peer.dispatch(SessionNotification::new(
            "s1",
            SessionUpdate::AgentMessageChunk(
                ContentChunk::new(ContentBlock::Text(TextContent::new(t))),
            ),
        ));

        let tr = peer.transcript();
        // thinking + tool + answer = 3 separate bubbles; nothing merged, nothing lost.
        assert_eq!(tr.len(), 3, "expected thinking+tool+answer separate, got {}", tr.len());
        assert_eq!(tr[0].role, "thought");
        assert!(!tr[0].content.is_empty(), "thinking was not lost");
        assert_eq!(tr[1].role, "tool");
        assert_eq!(tr[1].content, "shell");
        assert_eq!(tr[2].role, "agent");
        assert_eq!(tr[2].content, "answer ");
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

    // -- wire-format contract ------------------------------------------------
    //
    // The spine and the roam peer MUST emit the same snake_case status/kind
    // strings; the Android UI matches on these exact values (Screens.kt:
    // "in_progress" / "failed" / "allow_once"...). Debug-format drift here
    // silently breaks roam-peer tool progress and permission labels.

    #[test]
    fn shared_status_and_kind_strings_are_snake_case_variants() {
        use agent_client_protocol::schema::v1::{PermissionOptionKind, ToolCallStatus};
        assert_eq!(crate::spine::tool_call_status_str(&ToolCallStatus::Pending), "pending");
        assert_eq!(crate::spine::tool_call_status_str(&ToolCallStatus::InProgress), "in_progress");
        assert_eq!(crate::spine::tool_call_status_str(&ToolCallStatus::Completed), "completed");
        assert_eq!(crate::spine::tool_call_status_str(&ToolCallStatus::Failed), "failed");
        assert_eq!(crate::spine::permission_option_kind_str(&PermissionOptionKind::AllowOnce), "allow_once");
        assert_eq!(crate::spine::permission_option_kind_str(&PermissionOptionKind::AllowAlways), "allow_always");
        assert_eq!(crate::spine::permission_option_kind_str(&PermissionOptionKind::RejectOnce), "reject_once");
        assert_eq!(crate::spine::permission_option_kind_str(&PermissionOptionKind::RejectAlways), "reject_always");
    }

    #[test]
    fn prompt_timeout_emits_run_ended() {
        // Regression: a session/prompt whose reply never arrives (the peer
        // vanished mid-turn without a clean FIN, so send_request hangs and
        // never errors) must still end the turn. Before the fix, the `?` on
        // recv_timeout returned before end_turn ran, so the app never received
        // RunEnded and stayed busy forever.
        let listener = test_listener();
        let (peer, _cmd_rx) = offline_peer(
            "laptop",
            listener.clone(),
            gate(Arc::new(AtomicBool::new(true))),
        );

        // A ConnectionTo<Agent> over a Channel whose counterpart never replies:
        // send_request hangs, so the 30s RPC_TIMEOUT fires.
        let (transport, _silent) = Channel::duplex();
        let cx_holder: Arc<Mutex<Option<agent_client_protocol::ConnectionTo<Agent>>>> =
            Arc::new(Mutex::new(None));
        let (ready_tx, ready_rx) = std_mpsc::channel();
        let (_keep_alive_tx, keep_alive_rx) = tokio::sync::oneshot::channel::<()>();
        let cx_holder2 = cx_holder.clone();
        let ready_tx2 = ready_tx.clone();
        runtime().spawn(async move {
            let _ = Client.builder()
                .name("grouse")
                .connect_with(
                    transport,
                    async move |cx: agent_client_protocol::ConnectionTo<Agent>| {
                        *cx_holder2.lock() = Some(cx);
                        let _ = ready_tx2.send(());
                        // Never return: keeps the connection (and the transport)
                        // alive so send_request keeps waiting for a reply.
                        let _ = keep_alive_rx.await;
                        Ok(())
                    },
                )
                .await;
        });
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("connection not established");
        peer.inner.lock().conn = cx_holder.lock().take();

        // The prompt never gets a reply; the turn must still end.
        let result = peer.rpc("session/prompt", json!({ "sessionId": "roam:laptop:s1" }));
        assert!(result.is_err(), "expected the rpc to time out");

        let events = listener.events.lock();
        assert!(
            events.iter().any(|e| e == "stream RunEnded"),
            "expected RunEnded to be emitted, got: {events:?}"
        );
        // The peer's run bookkeeping is cleared too.
        assert!(peer.inner.lock().active_run.is_none());
    }

    #[test]
    fn replay_tail_only_when_stamps_agree() {
        assert_eq!(
            replay_tail_decision("2026-08-23T10:00:00Z", "2026-08-23T10:00:00Z"),
            Some(REPLAY_TAIL)
        );
        // The session moved while we were away — a tail could hide a gap.
        assert_eq!(
            replay_tail_decision("2026-08-23T10:00:00Z", "2026-08-23T11:00:00Z"),
            None
        );
        // No stamp on either side proves nothing.
        assert_eq!(replay_tail_decision("", ""), None);
        assert_eq!(replay_tail_decision("", "2026-08-23T10:00:00Z"), None);
    }

    #[test]
    fn session_info_update_refreshes_the_session_list() {
        let listener = test_listener();
        let (peer, _cmd_rx) = offline_peer(
            "laptop",
            listener,
            gate(Arc::new(AtomicBool::new(false))),
        );
        peer.inner.lock().sessions.push(SessionSummary {
            id: "roam:laptop:s1".to_string(),
            title: "old title".to_string(),
            updated_at: "2026-08-23T10:00:00Z".to_string(),
            last_message_snippet: None,
            project_id: None,
            message_count: 0,
            model: String::new(),
            has_recipe: false,
            has_new: false,
            archived: false,
        });

        peer.dispatch(SessionNotification::new(
            "s1",
            SessionUpdate::SessionInfoUpdate(
                SessionInfoUpdate::new()
                    .title("new title".to_string())
                    .updated_at("2026-08-23T11:00:00Z".to_string()),
            ),
        ));

        // The stored summary tracks the live update even while the session is
        // not active — save_open_transcript stamps the cache from here.
        let inner = peer.inner.lock();
        assert_eq!(inner.sessions[0].title, "new title");
        assert_eq!(inner.sessions[0].updated_at, "2026-08-23T11:00:00Z");
    }
}
