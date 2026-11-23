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
//! The card a peer first connects with is pinned by label in the core cache
//! (`roam-card-pins.json`, 0600): every dial — including a supervisor redial —
//! compares the card fingerprint and refuses with a terminal status on change.
//! Clearing that pin is the only re-enrollment path.
//!
//! Session notifications go through the SAME `session::dispatch` as the spine
//! (`SessionNotification` → one `SessionHub` store per display id). Painting is
//! gated by the hub's displayed key, never by the peer; the peer itself only
//! emits the peer-scoped `on_roam_peer_status` / `on_roam_sessions`.
//!
//! Peer transcripts live in the shared hub under `roam:<label>:<raw>` (also
//! their cache key), saved by the core's one freshness rule.
//!
//! Like the main connection, a peer whose link drops unexpectedly re-dials
//! itself: the [`RoamPeer::connect`] task is a supervisor that retries with
//! the core's backoff (500ms·2^n, cap 15s) and, once the handshake lands,
//! resumes the session that was open. Only an explicit [`RoamPeer::close`]
//! (or [`Core::roam_disconnect`]) ends a peer for good.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc as std_mpsc, Arc, LazyLock};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

use agent_client_protocol::schema::v1::{
    ListSessionsRequest, ListSessionsResponse, RequestPermissionRequest, RequestPermissionResponse,
    SessionInfo, SessionNotification, SessionUpdate,
};
use agent_client_protocol::{
    Agent, Channel, Client, ConnectTo, Error as AcpError, Responder, TransportFrame, UntypedMessage,
};
use futures::future::{select, BoxFuture};
use futures::pin_mut;
use futures::stream::StreamExt;
use grouse_roam_core::{RoamSecret, RoamStream};
use serde_json::Value;
use tokio::runtime::Runtime;
use tokio::sync::mpsc as tokio_mpsc;

use crate::cache::CacheStore;
use crate::session::ChatEndpoint;
use crate::spine::{PendingRequest, RpcConn};
use crate::{ConnectionStatus, CoreListener, PermissionOutcome, SessionSummary};

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
// Connection-card pinning
// ---------------------------------------------------------------------------

/// The card a peer first connected with is the identity of its `roam:<label>:`
/// namespace. Displaying a fingerprint without enforcing it let a changed
/// card (a re-shared host, a MITM'd pairing paste) silently become the trust
/// anchor for every chat under that label. The map lives in the core cache,
/// not in a client, so all three clients enforce the same pin.
const CARD_PIN_FILE: &str = "roam-card-pins.json";

/// Serializes read-modify-write of the pin file. Reads are cheap (one small
/// JSON file per dial); re-reading on every dial means a pin changed by
/// another process is never masked by an in-memory copy.
static CARD_PIN_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Debug)]
pub(crate) enum CardPinError {
    /// A malformed card: the old retry budget still applies (it is a bad
    /// paste, not a changed trust anchor).
    InvalidCard(String),
    /// The card's fingerprint differs from the enrolled one. Terminal: the
    /// user must explicitly clear the pin to re-enroll.
    Mismatch {
        label: String,
        pinned: String,
        got: String,
    },
    /// The pin file itself is unreadable/corrupt. Fail closed; clear repairs.
    Store(String),
}

impl fmt::Display for CardPinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCard(e) => write!(f, "invalid roam card: {e}"),
            Self::Mismatch {
                label,
                pinned,
                got,
            } => write!(
                f,
                "roam card for '{label}' changed: pinned {pinned}, got {got}. \
                 Refusing to dial — clear the pin to re-enroll this peer"
            ),
            Self::Store(e) => write!(
                f,
                "roam card pin store is unusable: {e}. Clear the pin to repair it"
            ),
        }
    }
}

fn pins_path(cache: &CacheStore) -> PathBuf {
    cache.dir().join(CARD_PIN_FILE)
}

fn read_pins(path: &Path) -> Result<BTreeMap<String, String>, CardPinError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => {
            return Err(CardPinError::Store(format!(
                "cannot read {}: {e}",
                path.display()
            )))
        }
    };
    serde_json::from_slice(&bytes).map_err(|e| {
        CardPinError::Store(format!("cannot parse {}: {e}", path.display()))
    })
}

/// 0600 from creation, unguessable temp name, same-directory rename. The
/// fingerprint is public, but the pin file is a trust decision: a world-
/// writable pin (or a predictable temp that another user can plant) would let
/// anyone choose which host this client accepts for a label.
fn write_pins(path: &Path, pins: &BTreeMap<String, String>) -> Result<(), CardPinError> {
    let bytes = serde_json::to_vec(pins)
        .map_err(|e| CardPinError::Store(format!("cannot encode pins: {e}")))?;
    write_private_atomic(path, &bytes)
        .map_err(|e| CardPinError::Store(format!("cannot write {}: {e}", path.display())))
}

fn write_private_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let leaf = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("roam-card-pins.json");
    for attempt in 0..8 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = path.with_file_name(format!(
            ".{leaf}.tmp-{}-{nanos}-{seq:08x}-{attempt}",
            std::process::id()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = match options.open(&tmp) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let written = (|| {
            file.write_all(data)?;
            file.flush()?;
            file.sync_all()
        })();
        drop(file);
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        if let Some(parent) = path.parent() {
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique pin temp file",
    ))
}

/// The pinned fingerprint for a label, if any (for a client's pin viewer).
pub(crate) fn roam_card_pin(cache: &CacheStore, label: &str) -> Option<String> {
    let _guard = CARD_PIN_LOCK.lock();
    read_pins(&pins_path(cache)).ok()?.remove(label)
}

/// Every `{label: fingerprint}` pin, for a settings screen that lists them.
pub(crate) fn roam_card_pins(cache: &CacheStore) -> BTreeMap<String, String> {
    let _guard = CARD_PIN_LOCK.lock();
    read_pins(&pins_path(cache)).unwrap_or_default()
}

/// Explicitly enroll/replace a pin (e.g. after a user confirms a re-shared
/// card out of band).
pub(crate) fn roam_card_pin_enroll(
    cache: &CacheStore,
    label: &str,
    fingerprint: &str,
) -> Result<(), String> {
    let _guard = CARD_PIN_LOCK.lock();
    let path = pins_path(cache);
    let mut pins = read_pins(&path).map_err(|e| e.to_string())?;
    pins.insert(label.to_string(), fingerprint.to_string());
    write_pins(&path, &pins).map_err(|e| e.to_string())
}

/// Forget one label's pin so the next dial re-enrolls. A corrupt file is
/// repaired by writing the remaining pins (or an empty map) back.
pub(crate) fn roam_card_pin_clear(cache: &CacheStore, label: &str) -> Result<(), String> {
    let _guard = CARD_PIN_LOCK.lock();
    let path = pins_path(cache);
    let mut pins = read_pins(&path).unwrap_or_default();
    pins.remove(label);
    write_pins(&path, &pins).map_err(|e| e.to_string())
}

/// Enroll on first use, compare on every later dial. Called by the supervisor
/// on EVERY attempt, including reconnect redials.
pub(crate) fn verify_card_fingerprint(
    cache: &CacheStore,
    label: &str,
    fingerprint: &str,
) -> Result<(), CardPinError> {
    let _guard = CARD_PIN_LOCK.lock();
    let path = pins_path(cache);
    let mut pins = read_pins(&path)?;
    if let Some(pinned) = pins.get(label) {
        if pinned != fingerprint {
            return Err(CardPinError::Mismatch {
                label: label.to_string(),
                pinned: pinned.clone(),
                got: fingerprint.to_string(),
            });
        }
        return Ok(());
    }
    pins.insert(label.to_string(), fingerprint.to_string());
    write_pins(&path, &pins)
}

fn verify_card_pin(
    cache: &CacheStore,
    label: &str,
    card: &str,
) -> Result<(), CardPinError> {
    let fingerprint = grouse_roam_core::card_fingerprint(card)
        .map_err(|e| CardPinError::InvalidCard(e.to_string()))?;
    verify_card_fingerprint(cache, label, &fingerprint)
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
pub(crate) struct RoamCodec {
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
                    frames
                        .push(String::from_utf8_lossy(&std::mem::take(&mut self.buf)).into_owned());
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
pub(crate) struct RoamTransport {
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
    OpenSession {
        session_id: String,
        cwd: String,
        force: bool,
    },
    NewSession {
        cwd: String,
        /// `grouse.mode` written at creation on THIS peer (dev, or None for chat).
        mode: Option<String>,
    },
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
    /// The card no longer matches its pin (or the pin store is unusable).
    /// Terminal — retrying cannot fix a changed trust anchor; the user must
    /// clear the pin. `fail()` already emitted the reason.
    Refused,
}

/// One active roam endpoint: a direct iroh peer running its own goose (the
/// desktop's `m_roamPeers` entry). The peer's client stays connected in
/// browse mode — after `initialize` only `session/list` runs; sessions open
/// explicitly via [`RoamPeer::open_session`].
pub(crate) struct RoamPeer {
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
    /// The one dispatch pipeline, prefixed with this peer's label.
    host: Arc<dyn crate::session::DispatchHost>,
    /// Every session's store, shared with the main connection.
    hub: Arc<crate::session::SessionHub>,
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
    /// The mode the OPEN session was created in, when this client created it.
    ///
    /// A mode is a tag, and a tag is write-only: the list cannot tell us, and a chat
    /// this client just made is EMPTY — goose's `session/list` omits empty sessions, so
    /// the per-mode walk below cannot see it either. Local knowledge covers the gap
    /// until the session has a message and the walk catches up. Cleared when a
    /// different session is opened, so the synthetic row cannot inherit it.
    open_session_mode: Option<String>,
    /// Whether the open session appeared in a SERVER list (as opposed to only
    /// in our synthetic empty-chat row). A session that was listed and then
    /// disappears on the next relist was archived/deleted: unioning it back in
    /// would resurrect a ghost row, and keeping it as `resume_pending` would
    /// re-open a dead chat after a link drop.
    open_session_listed: bool,
    /// Each listed session's working directory, keyed by RAW session id
    /// (populated from `session/list`; `session/load` must use the session's
    /// own cwd — the main connection's `last_config` cwd is empty on a
    /// roam-only client and `session/load` hard-fails on a blank cwd).
    ///
    /// Entries are MERGED on relist: goose omits empty sessions, so replacing
    /// this map with the fresh list wipes the cwd of the just-created chat (and
    /// of hidden sessions the drawer never lists). Later loads then send `""`
    /// and goose rewrites `working_dir`. Only pathological growth is pruned.
    session_cwds: HashMap<String, String>,
    /// A dropped link with a session open sets this: once the re-dial's
    /// handshake lands and `session/list` arrives, the peer re-opens the same
    /// session (the equivalent of the main connection's resume-after-
    /// reconnect). Consumed by `handshake_and_serve`.
    resume_pending: Option<String>,
    /// The live turn's run id (`_meta.goose.activeRunId`); None when no turn
    /// is running. Mirror of the core's per-session run map, kept here only so
    /// a dropped link can clear it.
    active_run: Option<String>,
    /// The SDK connection handle, set once `initialize` succeeds.
    conn: Option<agent_client_protocol::ConnectionTo<Agent>>,
    /// Optional override for the open-success notification (the core host's
    /// central emitter). `None` emits `CoreListener::on_session_opened`
    /// directly from the bind, like every other peer-scoped event here.
    on_session_opened: Option<std::sync::Arc<dyn Fn(String) + Send + Sync>>,
    /// In-flight `session/request_permission` responders, keyed by tool call
    /// id so parallel calls can each be answered (the spine's map, not the old
    /// single slot that dropped the first responder).
    pending_permission: HashMap<String, Responder<RequestPermissionResponse>>,
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
}

impl PeerInner {
    fn new() -> Self {
        Self {
            status: ConnectionStatus::Connecting,
            sessions: Vec::new(),
            open_session_id: None,
            open_session_mode: None,
            open_session_listed: false,
            resume_pending: None,
            session_cwds: HashMap::new(),
            active_run: None,
            conn: None,
            on_session_opened: None,
            pending_permission: HashMap::new(),
            stream: None,
            closing: false,
            ever_ready: false,
        }
    }
}

impl RoamPeer {
    /// Re-dial budget: the main connection's shape (500ms·2^n, 15s cap — see
    /// [`crate::Core`]'s `schedule_reconnect`), but where main gives up after
    /// 6 tries, a peer keeps trying far longer. A roam host is a laptop that
    /// was simply closed when the phone slept: the drop can last hours, and
    /// the moment the path exists again the user's chats must work without a
    /// visit to the endpoints screen. ~25 attempts ≈ 6 minutes of retrying at
    /// the cap before the peer surfaces a terminal error.
    const RECONNECT_MAX: u32 = 25;

    /// Upper bound on remembered working directories. A cwd entry is a couple
    /// of strings; this is only a guard against unbounded growth if a peer's
    /// session churns through thousands of ids. Entries not in the latest list
    /// (and not the open session) are pruned first, so real sessions and the
    /// hidden ones survive; only beyond the cap do we drop any other key.
    const MAX_SESSION_CWDS: usize = 4096;

    /// Connect to a roam peer in browse mode, supervised. The dial (blocking,
    /// seconds) and the ACP handshake run on the core runtime; this returns
    /// immediately with a peer in `Connecting` state. `host` is the shared
    /// dispatch pipeline prefixed with this peer's label. One peer per label:
    /// the caller replaces an existing peer with the same label before
    /// connecting again.
    ///
    /// The task loops: dial → handshake → serve commands. An unexpected
    /// transport end re-dials with exponential backoff and resumes the open
    /// session; an explicit `Close` (or `close()` mid-dial) ends the task.
    pub(crate) fn connect(
        secret: String,
        card: String,
        label: String,
        listener: Arc<dyn CoreListener>,
        host: Arc<dyn crate::session::DispatchHost>,
        hub: Arc<crate::session::SessionHub>,
        cache: Arc<CacheStore>,
    ) -> Arc<Self> {
        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
        let peer = Arc::new(Self {
            label: label.clone(),
            inner: Mutex::new(PeerInner::new()),
            cmd_tx,
            cmd_rx: Arc::new(tokio::sync::Mutex::new(cmd_rx)),
            listener,
            host,
            hub,
            cache,
        });
        // Own the identity bytes exactly once. Every retry shares this Arc;
        // the old code cloned the base64 String per attempt, leaving copies
        // the process could neither track nor scrub. `RoamSecret` overwrites
        // its bytes when the last handle (including this one) drops.
        let secret = Arc::new(RoamSecret::from_owned(secret));
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
                    Outcome::Closed | Outcome::Refused => return,
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
                        let budget = if ever_connected {
                            Self::RECONNECT_MAX
                        } else {
                            6
                        };
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
    async fn attempt(
        self: &Arc<Self>,
        secret: &Arc<RoamSecret>,
        card: &str,
        label: &str,
    ) -> Outcome {
        // Fresh readiness bookkeeping per link.
        self.inner.lock().ever_ready = false;
        // 0. Pin check BEFORE every dial, including supervisor redials. First
        // use enrolls; a changed card is terminal with an explicit status.
        if let Err(error) = verify_card_pin(&self.cache, &self.label, card) {
            return match error {
                CardPinError::InvalidCard(reason) => {
                    Outcome::Retry(format!("invalid roam card: {reason}"))
                }
                other => {
                    self.fail(format!("roam card pin refused: {other}"));
                    Outcome::Refused
                }
            };
        }
        // 1. Dial (blocking, seconds) off the runtime.
        self.emit_status("connecting: dialing");
        let dialed = tokio::task::spawn_blocking({
            let secret = secret.clone();
            let card = card.to_string();
            let label = label.to_string();
            move || grouse_roam_core::roam_connect_with_secret(&secret, &card, Some(label))
        })
        .await;
        let stream = match dialed {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => return Outcome::Retry(format!("roam connect: {error}")),
            Err(error) => return Outcome::Retry(format!("roam dial task panicked: {error}")),
        };
        // Closed while dialing? Hand the stream back immediately.
        if self.inner.lock().closing {
            stream.shutdown();
            stream.cancel();
            return Outcome::Closed;
        }
        self.inner.lock().stream = Some(stream.clone());

        // 2. The SDK client: the SAME handler table as the spine, so custom
        //    notifications (status/message_usage) and recipe/elicitation
        //    requests behave identically on a peer.
        let notif_peer = self.clone();
        let permission_peer = self.clone();
        let request_peer = self.clone();
        let notification_peer = self.clone();
        let handlers = crate::session::ClientHandlers {
            notification: std::sync::Arc::new(move |notif| notif_peer.dispatch(notif)),
            custom_notification: std::sync::Arc::new(move |method, params| {
                // The ABI-4 session gate in `dispatch_notification` compares
                // `sessionId` against the DISPLAYED id, and a peer chat's
                // displayed id is `roam:<label>:<raw>`. The server keys
                // notifications by the NAKED id, so forwarding them untouched
                // made the gate eat every custom notification (message_usage
                // stats, the compaction status, the roam context bar) for the
                // chat that was actually open. Key them with this peer's
                // namespace so the gate sees what it is comparing against.
                crate::spine::dispatch_custom_notification(
                    method,
                    RoamPeer::prefix_notification_key(&params, notification_peer.label()),
                )
            }),
            permission: std::sync::Arc::new(move |req, responder| {
                // Register the OWNER by tool_call_id before parking the responder,
                // so Core::respond_permission can answer a main-wire prompt while
                // this peer owns the screen (and this one while a main chat does).
                let tool_call_id = crate::session::permission_request(&req).tool_call_id;
                let peer = permission_peer.clone();
                peer.on_permission_request(req, responder);
                crate::spine::register_permission_answer(
                    tool_call_id,
                    std::sync::Arc::new(move |id, outcome| peer.respond_permission(id, outcome)),
                );
            }),
            request: std::sync::Arc::new(move |method, params, responder| {
                request_peer.on_server_request(method, params, responder)
            }),
        };

        // 3. Connect: initialize -> session/list (browse) -> resume-if-owed,
        //    then serve commands until close.
        let main_task = self.clone();
        let transport_stream = stream.clone();
        let foreground = move |cx: agent_client_protocol::ConnectionTo<Agent>| {
            let task = main_task.clone();
            Box::pin(async move { task.handshake_and_serve(cx).await })
                as futures::future::BoxFuture<'static, Result<(), AcpError>>
        };
        let result =
            crate::session::connect_client(RoamTransport::new(stream), handlers, foreground).await;
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

    /// Take over open-success emission from the default `CoreListener` path.
    /// The callback receives the DISPLAY session id (`roam:<label>:<raw>`).
    /// Only a core that emits the event centrally should register one — doing
    /// both emits the event twice.
    #[allow(dead_code)] // the core host wires this only when it emits centrally
    pub(crate) fn set_on_session_opened(&self, f: std::sync::Arc<dyn Fn(String) + Send + Sync>) {
        self.inner.lock().on_session_opened = Some(f);
    }

    /// The successful-bind notification for this peer. Both peer binds —
    /// `load_session` (a tap, a forced reload, or the supervised reconnect
    /// resume) and `create_session` (`session/new`) — end here with the
    /// prefixed id the UI routes by. No notification on a failed load.
    fn report_session_opened(&self, display_id: &str) {
        let (closing, hook) = {
            let inner = self.inner.lock();
            (inner.closing, inner.on_session_opened.clone())
        };
        if closing {
            return;
        }
        if let Some(hook) = hook {
            hook(display_id.to_string());
        } else {
            self.listener.on_session_opened(display_id.to_string());
        }
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
            // Advisory statuses never live in a peer's stored state (only
            // emit_status paths write lifecycle variants), so this arm is a
            // defensive pass-through.
            ConnectionStatus::Alert { message } => ConnectionStatus::Error {
                message: message.clone(),
            },
        }
    }

    /// The last `session/list` result, ids prefixed `roam:<label>:<id>`.
    pub fn sessions(&self) -> Vec<SessionSummary> {
        // Rebuild from fields: the uniffi records are not Clone (yet). The
        // green dot comes from the shared hub (has_new), so every other field
        // is preserved verbatim.
        let inner = self.inner.lock();
        inner
            .sessions
            .iter()
            .map(|s| SessionSummary {
                // The effective mode was resolved by the per-mode walk when the
                // list arrived. Dropping it here cost a peer's dev chats their
                // badge in every client that reads `on_roam_sessions`.
                mode: s.mode.clone(),
                id: s.id.clone(),
                title: s.title.clone(),
                updated_at: s.updated_at.clone(),
                last_message_snippet: s.last_message_snippet.clone(),
                project_id: s.project_id.clone(),
                message_count: s.message_count,
                model: s.model.clone(),
                has_recipe: s.has_recipe,
                has_new: self.hub.has_new(&s.id),
                archived: s.archived,
            })
            .collect()
    }

    /// The open session's id (`roam:<label>:<raw>`), if one is open.
    pub fn active_session_id(&self) -> Option<String> {
        let inner = self.inner.lock();
        inner
            .open_session_id
            .as_ref()
            .map(|id| format!("roam:{}:{id}", self.label))
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
        self.open_session_with(session_id, cwd, false);
    }

    /// Open/reload a peer session. `force` ignores cache freshness and takes a
    /// full replay (the desktop's "reload from the start").
    pub fn open_session_with(&self, session_id: String, cwd: String, force: bool) {
        match self.status() {
            // Syncing is accepted for the same reason Connecting is: the command queue
            // lives on the PEER, so a command sent now is buffered and runs when the
            // loop picks it up. Dropping it silently meant a Reload press during a
            // resync (or a reconnect's sync) did nothing at all — no request, no
            // status, no error, on the peer only (the main wire has no such gate).
            ConnectionStatus::Ready
            | ConnectionStatus::Connecting
            | ConnectionStatus::Syncing => {}
            _ => return,
        }
        // The wire id is raw; the display id carries this peer's namespace.
        let raw = self.strip_prefix(&session_id);
        // The peer's OWN cwd beats the caller's: `open_on_peer` in lib.rs only
        // has the main connection's `last_config`, which is empty on a
        // roam-only client and wrong when the remote goose rewrote
        // `working_dir`. `session/load` silently rewrites it from what we send,
        // so using the main cwd would move the peer's session. The caller's
        // value is still the fallback for a session we know nothing about.
        let cwd = <Self as ChatEndpoint>::session_cwd(self, &session_id)
            .filter(|known| !known.is_empty())
            .unwrap_or(cwd);
        let _ = self.cmd_tx.send(PeerCommand::OpenSession {
            session_id: raw,
            cwd,
            force,
        });
    }

    /// Create a fresh session on this peer. The peer's goose has no default
    /// cwd, so one is required (like `session/new` on the main connection);
    /// the caller resolves it the same way `open_session` does. Fire-and-forget
    /// into the peer's command loop, like the desktop's `openRoamSession`.
    /// Same Ready-or-Connecting gate: queued commands ride the reconnect.
    pub fn new_session(&self, cwd: String) {
        self.new_session_in_mode(cwd, None);
    }

    /// The same, in a mode (`grouse.mode`: dev). The mode is a tag written on
    /// `session/new` — the peer's goose stores it and filters on it, so a roam chat
    /// can be a dev chat exactly like a local one.
    pub fn new_session_in_mode(&self, cwd: String, mode: Option<String>) {
        match self.status() {
            ConnectionStatus::Ready | ConnectionStatus::Connecting => {}
            _ => return,
        }
        let _ = self.cmd_tx.send(PeerCommand::NewSession { cwd, mode });
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
        let Some(open) = self.inner.lock().open_session_id.clone() else {
            return;
        };
        let key = self.cache_key(&open);
        // A store that is MID-REPLAY is a partial ingest (emissions held, rows
        // still arriving). Saving it here cached whatever prefix had landed, and
        // the next open then painted that prefix and planned a merge from it —
        // which is why repeated manual refreshes of one chat showed a different
        // "checkpoint" each time.
        if self.host.replaying(&key) {
            return;
        }
        // The core owns the freshness rule (a mid-turn store is written with an
        // empty stamp); duplicating it here would let a dropped peer turn look
        // current.
        self.host.cache_session(&key);
    }

    /// Send a raw JSON-RPC request over this peer's connection — the
    /// peer-side mirror of the spine's `Conn::rpc`. Synchronous: the request
    /// is spawned onto the core runtime and this call blocks on the reply.
    /// Chat/unstable intents route here while this peer owns the active
    /// session. Call from a UI/intent thread, not from inside the dispatch
    /// loop.
    pub fn rpc(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        let conn = { self.inner.lock().conn.clone() }
            .ok_or_else(|| AcpError::internal_error().data("roam peer not connected"))?;
        let msg = UntypedMessage::new(method, self.wire_params(params))?;
        let (tx, rx) = std_mpsc::channel();
        runtime().spawn(async move {
            let _ = tx.send(conn.send_request(msg).block_task().await);
        });
        // Control calls are bounded (S-RC-6); prompts never travel through
        // this path anymore (Core's one transmit path owns them).
        let limit =
            crate::session::rpc_timeout(method).unwrap_or(std::time::Duration::from_secs(30));
        match rx.recv_timeout(limit) {
            Ok(inner) => inner,
            Err(_) => Err(AcpError::internal_error().data("roam rpc timed out")),
        }
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
    pub fn respond_permission(
        &self,
        tool_call_id: &str,
        outcome: PermissionOutcome,
    ) -> Result<(), AcpError> {
        let responder = self
            .inner
            .lock()
            .pending_permission
            .remove(tool_call_id)
            .ok_or_else(|| AcpError::internal_error().data("no pending roam permission request"))?;
        responder
            .respond(crate::session::permission_response(outcome))
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
            cx.send_request(crate::session::initialize_request())
                .block_task(),
        )
        .await;
        match init {
            Ok(Ok(_reply)) => {}
            Ok(Err(error)) => {
                eprintln!(
                    "grouse-core: roam peer '{}' initialize: {error}",
                    self.label
                );
                return Err(AcpError::internal_error().data(format!("roam initialize: {error}")));
            }
            Err(_) => {
                eprintln!(
                    "grouse-core: roam peer '{}' handshake timed out",
                    self.label
                );
                return Err(AcpError::internal_error()
                    .data("handshake timed out waiting for initialize reply"));
            }
        }

        self.emit_status("connecting: listing sessions");
        let list: ListSessionsResponse =
            match tokio::time::timeout(PHASE_TIMEOUT, list_all_peer_sessions(&cx, &[])).await {
                Ok(Ok(reply)) => reply,
                Ok(Err(error)) => {
                    eprintln!(
                        "grouse-core: roam peer '{}' session/list: {error}",
                        self.label
                    );
                    return Err(error);
                }
                Err(_) => {
                    eprintln!(
                        "grouse-core: roam peer '{}' session/list timed out",
                        self.label
                    );
                    return Err(AcpError::internal_error()
                        .data("handshake timed out waiting for session/list reply"));
                }
            };
        // A tag is write-only, so a peer's modes cost one filtered list per known
        // non-default value. Best-effort: a failed walk leaves rows reading as chats.
        let modes = peer_session_modes(&cx).await;
        self.apply_sessions_with_modes(&list, &modes);

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
            if let Err(error) = self.load_session(raw, cwd, false).await {
                self.command_failed(format!("roam resume: {error}"));
            }
        }

        // The command queue lives on the PEER, not in this future: when the
        // transport dies the SDK drops this future mid-`recv`, and a receiver
        // owned here would die with it. Intents keep buffering in `cmd_tx`
        // across the outage; the next attempt picks the queue back up here.
        let mut cmd_rx = self.cmd_rx.lock().await;
        // Commands pulled forward while coalescing a run of opens. They run
        // AFTER the open that superseded them, in their original order.
        let mut deferred: std::collections::VecDeque<PeerCommand> = Default::default();
        loop {
            let cmd = match deferred.pop_front() {
                Some(cmd) => cmd,
                None => match cmd_rx.recv().await {
                    Some(cmd) => cmd,
                    None => break,
                },
            };
            match cmd {
                PeerCommand::OpenSession {
                    mut session_id,
                    mut cwd,
                    mut force,
                } => {
                    // A tap that supersedes three earlier taps owes ONE load.
                    // Every open is a whole `session/load` awaited right here,
                    // so a queue of them re-fetched a tail per step and the
                    // chat the user finally tapped sat behind all of them —
                    // the "switching fast makes it hang" report. Keep only the
                    // newest open; anything queued behind it runs next.
                    while let Ok(next) = cmd_rx.try_recv() {
                        match next {
                            PeerCommand::OpenSession {
                                session_id: newer,
                                cwd: newer_cwd,
                                force: newer_force,
                            } => {
                                session_id = newer;
                                cwd = newer_cwd;
                                force = newer_force;
                            }
                            other => deferred.push_back(other),
                        }
                    }
                    let raw = self.strip_prefix(&session_id);
                    if let Err(error) = self.load_session(raw, cwd, force).await {
                        self.command_failed(format!("roam session/open: {error}"));
                    }
                }
                PeerCommand::NewSession { cwd, mode } => {
                    match self.create_session(&cx, &cwd, mode.as_deref(), true).await {
                        Ok(()) => {}
                        Err(error) => {
                            self.command_failed(format!("roam session/new: {error}"));
                        }
                    }
                }
                PeerCommand::Relist => {
                    // A session mutation (rename/archive/delete) landed on this
                    // peer; re-fetch its list so the drawer reflects it. The
                    // remote goose does not reliably notify us of our own
                    // mutation, so we re-list explicitly.
                    if let Ok(list) = list_all_peer_sessions(&cx, &[]).await {
                        let modes = peer_session_modes(&cx).await;
                        self.apply_sessions_with_modes(&list, &modes);
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
        raw: String,
        cwd: String,
        force: bool,
    ) -> Result<(), AcpError> {
        // Paint the cache and make this session the displayed one BEFORE the
        // load: replay chunks may arrive ahead of the reply, and they must land
        // in the store the UI is watching. This is the same paint policy the
        // main connection applies (one implementation, `session::plan_paint`).
        self.save_open_transcript();
        let key = self.cache_key(&raw);
        let listed = {
            let list = self.inner.lock().sessions.clone();
            freshness_stamp(self.host.as_ref(), &list, &key)
        };
        // A TURN IN FLIGHT makes the peer's copy incomplete by construction: goose
        // persists a message when it finishes, so no replay can contain the reply
        // being streamed — and a replay that replaces the store deletes it (and
        // clears the stream id, so the rest of the reply restarts mid-sentence).
        // The store we are already writing into is the newest copy; paint it and
        // bind only.
        let live_turn = self.host.turn_in_flight(&key);
        let plan = if live_turn {
            crate::session::PaintPlan::bind_only()
        } else {
            let incomplete = self.host.is_incomplete(&key);
            crate::session::plan_paint(&self.cache, &key, &listed, force, incomplete)
        };
        {
            let mut inner = self.inner.lock();
            inner.open_session_id = Some(raw.clone());
            // This is a session that already exists on the peer, so its mode is the
            // walk's business, not a mode this client wrote.
            inner.open_session_mode = None;
            // It is (or was) in the server's list: if a later relist drops it,
            // that is an archive/delete, not an empty chat to union back in.
            inner.open_session_listed = inner.sessions.iter().any(|s| s.id == key);
            if !cwd.is_empty() {
                // Remember the resolved cwd for the hidden summarizer session.
                inner.session_cwds.insert(raw.clone(), cwd.clone());
            }
        }
        let store = self.hub.display(&key);
        if live_turn {
            // The client cleared its rows when it asked for this session, so the
            // store has to be painted back to it — the load below paints nothing.
            store.repaint_window();
        }
        // Paint AND replay through the SHARED lifecycle (`session::load_with_plan`):
        // it claims the session's load slot for both halves, so a resync (spawned
        // onto the runtime, not the peer's command loop) can never append its rows
        // beside this one's mid-flight replay — the interleaving that put the first
        // turn's tool runs at the bottom of a reopened chat. It also ingests
        // silently and paints once, and discards the tail a current cache does not
        // need. This file used to hand-write the lifecycle and lost steps (no held
        // emissions, no `end_merge`), which is what made a reopened chat show its
        // recent prompts twice, the copy at the bottom.
        let reply = crate::session::load_with_plan(
            self,
            self.host.as_ref(),
            &store,
            &key,
            &cwd,
            &plan,
        )
        .await;
        match reply {
            Ok(reply) => {
                self.emit_status("ready");
                self.emit_config(&reply, &key);
                // Re-baseline from the server after the replay, like the main
                // wire does (spine's post-load `stamp_probe`). Without it a peer
                // chat's cache keeps the stamp it had BEFORE the replay, so the
                // next open cannot tell that the rows it just fetched are current
                // — the second half of "it reloads every time".
                self.host.stamp_probe(&key, true);
                // Bind complete: one shared open-success event, under the
                // prefixed id the UI routes by. The reconnect resume goes
                // through this same function, so it notifies too.
                self.report_session_opened(&key);
            }
            Err(error) => {
                // The tapped session is stale/archived — do NOT quietly proxy
                // it with a fresh session on the peer: the user asked for THIS
                // session, and a blank replacement is the defect (grouse#19).
                // Keep the painted cache and report against the prefixed id.
                {
                    let mut inner = self.inner.lock();
                    inner.open_session_id = None;
                    inner.open_session_mode = None;
                    inner.open_session_listed = false;
                }
                self.listener.on_session_open_failed(
                    format!("roam:{}:{}", self.label, raw),
                    format!("session/load failed: {error}"),
                );
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
        mode: Option<&str>,
        notify_new: bool,
    ) -> Result<(), AcpError> {
        // The remote goose's session/new deserializer REQUIRES mcpServers
        // (a missing field is a hard protocol error, not a default) — the
        // shared builder sends it for every transport.
        let params = crate::session::session_new_params("grouse", cwd, None, None, mode);
        let reply = cx
            .send_request(UntypedMessage::new("session/new", params)?)
            .block_task()
            .await
            .map_err(|error| {
                AcpError::internal_error().data(format!("roam session/new: {error}"))
            })?;
        let sid = reply
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        self.save_open_transcript();
        let key = format!("roam:{}:{sid}", self.label);
        {
            let mut inner = self.inner.lock();
            inner.open_session_id = Some(sid.clone());
            // What the tag was written with. The synthetic row reads this because a
            // chat with no messages is in neither the peer's list nor the mode walk.
            inner.open_session_mode =
                mode.map(str::to_string).filter(|m| !m.is_empty());
            // A just-created chat is EMPTY: goose will not list it until its
            // first message, so it lives only in the synthetic row for now.
            inner.open_session_listed = false;
            // The hidden summarizer needs this session's own cwd on the peer.
            inner.session_cwds.insert(sid.clone(), cwd.to_string());
        }
        self.hub.display(&key).clear();
        self.emit_status("ready");
        self.emit_config(&reply, &key);
        // The app opens a freshly created chat on this id — but ONLY when the
        // user initiated the create. The load-fallback create must not yank
        // the UI to a proxy session the user didn't ask for.
        if notify_new {
            self.listener.on_peer_new_session(self.label.clone(), sid);
        }
        // The new chat is bound (hub.display + clear above): report the same
        // open-success event a peer load reports, after the app has been told
        // which id to switch to.
        self.report_session_opened(&key);
        // The created session must enter the peer's list or the app's drawer
        // never learns it exists (and cannot route a chat to it). Re-list.
        let relist = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            list_all_peer_sessions(cx, &[]),
        )
        .await;
        if let Ok(Ok(list)) = relist {
            // The new chat is empty, so the mode walk cannot see it yet —
            // `open_session_mode` (set below) is what the synthetic row reads.
            let modes = peer_session_modes(cx).await;
            self.apply_sessions_with_modes(&list, &modes);
        }
        Ok(())
    }

    /// Surface the session config from a `session/load`|`session/new` reply as
    /// a `CoreListener::on_config` event. The remote goose populates the
    /// `model` option's `options` list with its featured models, so this is
    /// where a peer's model picker gets its choices. A reply without
    /// `configOptions` yields an empty vec — the app tolerates that.
    fn emit_config(&self, reply: &Value, key: &str) {
        let options = crate::session::parse_config_options(reply);
        // Through the CORE's merge (the host), never straight to the UI: a
        // partial update keeps each option's choices, and the host gates it on
        // this session being the displayed one.
        if !options.is_empty() {
            self.host.config(key, options);
        }
    }

    /// Fold a fresh `session/list`'s cwds into what we already know, instead
    /// of replacing it: goose omits empty and hidden sessions, so a replace
    /// drops the cwd of the chat just created on this peer and every later
    /// `session/load` sends `""` (which goose treats as permission to rewrite
    /// `working_dir`). A changed cwd from the server wins; an absent one is
    /// left alone. Only a pathological map is pruned, oldest-looking
    /// non-listed keys first (HashMap has no order, so any stale key will do),
    /// and the open session's key is never dropped.
    fn merge_session_cwds(
        stored: &mut HashMap<String, String>,
        fresh: HashMap<String, String>,
        open: Option<&str>,
    ) {
        for (id, cwd) in &fresh {
            // A row with no cwd is not authority to forget one we already
            // learned (the remote can omit it on some list shapes).
            if cwd.is_empty() && stored.get(id).is_some_and(|old| !old.is_empty()) {
                continue;
            }
            stored.insert(id.clone(), cwd.clone());
        }
        if stored.len() <= Self::MAX_SESSION_CWDS {
            return;
        }
        let mut stale: Vec<String> = stored
            .keys()
            .filter(|id| !fresh.contains_key(id.as_str()) && Some(id.as_str()) != open)
            .cloned()
            .collect();
        for id in stale.drain(..) {
            stored.remove(&id);
            if stored.len() <= Self::MAX_SESSION_CWDS {
                return;
            }
        }
        // The fresh list itself can exceed the cap; drop arbitrary non-open
        // entries rather than growing the map without bound.
        while stored.len() > Self::MAX_SESSION_CWDS {
            let victim = stored
                .keys()
                .find(|id| Some(id.as_str()) != open)
                .cloned();
            match victim {
                Some(id) => {
                    stored.remove(&id);
                }
                None => break,
            }
        }
    }

    /// `session/list` arrived: store it, flip to Ready, emit. Browse mode — no
    /// session is auto-opened here (open only via `open_session`).
    ///
    /// [`Self::apply_sessions_with_modes`] for the paths that have not walked modes —
    /// the tests, and any caller with nothing to say about them.
    #[cfg(test)]
    fn apply_sessions(&self, list: &ListSessionsResponse) {
        self.apply_sessions_with_modes(list, &HashMap::new());
    }

    /// [`Self::apply_sessions`] plus the per-mode walk's answer: raw session id ->
    /// mode, for the sessions that are NOT plain chats.
    fn apply_sessions_with_modes(
        &self,
        list: &ListSessionsResponse,
        modes: &HashMap<String, String>,
    ) {
        let mut fresh_cwds = HashMap::new();
        let mut sessions: Vec<SessionSummary> = list
            .sessions
            .iter()
            .filter_map(|s| {
                let id = s.session_id.to_string();
                fresh_cwds.insert(id.clone(), crate::session::info_cwd(s));
                let mut summary = crate::session::summary_from_info(Some(&self.label), s)?;
                summary.has_new = self.hub.has_new(&summary.id);
                // The effective mode, never absent: an untagged chat IS a chat, the same
                // rule the main connection applies, so a client does not have to
                // re-derive the default for a peer's rows.
                summary.mode = Some(
                    modes
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| crate::session::DEFAULT_MODE.to_string()),
                );
                Some(summary)
            })
            .collect();
        {
            let mut inner = self.inner.lock();
            // A session the server USED to list but no longer does was
            // archived/deleted (lib.rs re-lists after those mutations). Without
            // this check the synthetic union below resurrects it as a ghost
            // row, and a later link drop queues it as `resume_pending` and
            // re-opens a chat the user deleted. An empty chat that was never
            // listed is NOT deleted — it has just not posted a message yet.
            if let Some(raw) = inner.open_session_id.clone() {
                let still_listed = list
                    .sessions
                    .iter()
                    .any(|s| s.session_id.to_string() == raw);
                if inner.open_session_listed && !still_listed {
                    inner.open_session_id = None;
                    inner.open_session_mode = None;
                    inner.open_session_listed = false;
                    inner.resume_pending = None;
                } else {
                    inner.open_session_listed = still_listed;
                }
            }
            // The peer's list only contains sessions that HAVE messages (goose's
            // session/list filters empty ones). A chat this client just created is
            // open but empty — invisible to the relist. Union it in: once its first
            // message lands, the server's own entry appears and the next list
            // replaces this synthetic row with server truth.
            if let Some(raw) = inner.open_session_id.clone() {
                let app_id = format!("roam:{}:{}", self.label, raw);
                if !sessions.iter().any(|s| s.id == app_id) {
                    let count = self.hub.store(&app_id).rich_transcript().len();
                    sessions.insert(
                        0,
                        SessionSummary {
                            // Local knowledge: this client created the session, so it
                            // knows the mode the tag was written with. An empty chat is
                            // in neither the peer's list nor the per-mode walk.
                            mode: inner.open_session_mode.clone(),
                            id: app_id,
                            title: String::new(),
                            updated_at: String::new(),
                            last_message_snippet: None,
                            project_id: None,
                            message_count: count as i64,
                            model: String::new(),
                            has_recipe: false,
                            has_new: false,
                            archived: false,
                        },
                    );
                }
            }
            // The cache-freshness baseline lives in the core, shared with the
            // main list. A peer's own list is the only place these stamps
            // arrive, so publish them without emitting a touch event.
            for summary in &sessions {
                self.host.session_stamp(&summary.id, &summary.updated_at);
                // The mode too: a peer chat's turn ends like any other, and the
                // advisor decides whether to review it from this.
                if let Some(mode) = &summary.mode {
                    self.host.session_mode(&summary.id, mode);
                }
            }
            inner.sessions = sessions;
            let open_key = inner.open_session_id.clone();
            Self::merge_session_cwds(
                &mut inner.session_cwds,
                fresh_cwds,
                open_key.as_deref(),
            );
            inner.status = ConnectionStatus::Ready;
            // The supervisor's outage posture keys on this: a link that has
            // been live earns the long budget and a fresh one per outage.
            inner.ever_ready = true;
        }
        self.emit_sessions(self.sessions());
        self.emit_status("ready");
        // Peer prompts queued while the link was down now have a wire.
        self.host.endpoint_ready(Some(&self.label));
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
        // A permission prompt answered after the link is gone has no wire to
        // reply on, and its responder used to sit in this map across every
        // re-dial. Drop them all with the connection.
        for id in inner.pending_permission.keys() {
            crate::spine::unregister_permission_answer(id);
        }
        inner.pending_permission.clear();
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
            inner.active_run = None;
            let open = inner.open_session_id.clone();
            drop(inner);
            if let Some(raw) = open {
                let key = format!("roam:{}:{raw}", self.label);
                // The host's run map is authoritative (it only emits the end
                // when a run was live) and the replay is over with the link.
                self.host.active_run(key.clone(), String::new());
                // The prompt reply died with the link, so this is the only thing
                // that can end the turn now: whoever owns the store must release
                // it, or the app keeps drawing "thinking" and queues every later
                // send behind a turn that can never finish.
                self.host.turn_lost(&key);
                self.host.end_replay(&key);
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

    /// A command (open/create/resume) failed on a link that is otherwise up.
    /// `fail()` made this terminal, but the command loop kept running: the
    /// status said Error, every later intent was rejected by the Ready gate,
    /// and the supervisor saw a healthy link and never redialed — one
    /// transient `session/new` error stranded a live peer. Keep the link and
    /// its queue, restore Ready so the next tap is accepted, and surface the
    /// message as an advisory (the UI treats `error:` as a dead peer).
    fn command_failed(&self, message: String) {
        eprintln!("grouse-core: roam peer '{}' command failed: {message}", self.label);
        {
            let mut inner = self.inner.lock();
            if inner.closing {
                return;
            }
            inner.status = ConnectionStatus::Ready;
        }
        // Ready first (settles any replay/watchdog state in the clients), then
        // the text. The raw cause is logged above for diagnosis.
        self.emit_status("ready");
        self.emit_status(&format!("alert: {message}"));
    }

    fn strip_prefix(&self, id: &str) -> String {
        id.strip_prefix(&format!("roam:{}:", self.label))
            .unwrap_or(id)
            .to_string()
    }

    /// Display id for a session on this peer: `roam:<label>:<raw>`.
    ///
    /// Idempotent for this peer's ids, so a FileEvent emitter holding either
    /// the raw wire id from a tool result or the already-prefixed id from the
    /// UI ends up with the one form the clients key on. A foreign id is left
    /// alone rather than double-prefixed.
    #[allow(dead_code)] // available to the core's FileEvent emitters
    pub(crate) fn display_id(&self, id: &str) -> String {
        if id.starts_with(&format!("roam:{}:", self.label))
            || id.starts_with(crate::session::ROAM_PREFIX)
        {
            return id.to_string();
        }
        crate::session::display_id(Some(&self.label), id)
    }

    /// Rewrite a goose-custom notification's `sessionId` into this peer's
    /// display namespace (`roam:<label>:<raw>`), the id form the dispatch
    /// gate compares against. NO-OP when the payload carries no session id
    /// (sessionless payloads pass the gate by design).
    fn prefix_notification_key(params: &Value, label: &str) -> Value {
        let Some(sid) = params.get("sessionId").and_then(Value::as_str) else {
            return params.clone();
        };
        if sid.is_empty() {
            return params.clone();
        }
        let mut out = params.clone();
        if let Some(obj) = out.as_object_mut() {
            obj["sessionId"] = Value::String(format!("roam:{label}:{sid}"));
        }
        out
    }

    /// Cache key for a peer session: the prefixed id. Collision-free across
    /// machines (the main cache keys on bare sessionId, which two machines
    /// could share) — peer transcripts live under `roam:<label>:<raw>`.
    fn cache_key(&self, raw: &str) -> String {
        format!("roam:{}:{raw}", self.label)
    }

    // -- SessionNotification dispatch -----------------------------------------

    /// One peer-specific pre-pass (its own session-list metadata), then the
    /// shared `session::dispatch`. The hub's display gate decides what paints.
    fn dispatch(&self, notif: SessionNotification) {
        let wire = notif.session_id.to_string();
        let key = format!("roam:{}:{wire}", self.label);
        // The peer's own session-list metadata is the one piece of state the
        // shared pipeline does not own (the main list lives in Core).
        if let SessionUpdate::SessionInfoUpdate(info) = &notif.update {
            let title = info
                .title
                .value()
                .map(|t| t.to_string())
                .unwrap_or_default();
            let updated_at = info
                .updated_at
                .value()
                .map(|t| t.to_string())
                .unwrap_or_default();
            let mut inner = self.inner.lock();
            if !title.is_empty() || !updated_at.is_empty() {
                if let Some(session) = inner.sessions.iter_mut().find(|s| s.id == key) {
                    if !title.is_empty() {
                        session.title = title;
                    }
                    if !updated_at.is_empty() {
                        session.updated_at = updated_at;
                    }
                }
            }
            if let Some(goose) = info.meta.as_ref().and_then(|m| m.get("goose")) {
                inner.active_run = goose
                    .get("activeRunId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
        }
        crate::session::dispatch(self.host.as_ref(), notif);
    }

    /// A `session/request_permission` arrived: stash the responder and prompt
    /// the UI. The host emits only while this peer's session is displayed.
    fn on_permission_request(
        &self,
        req: RequestPermissionRequest,
        responder: Responder<RequestPermissionResponse>,
    ) {
        let request = crate::session::permission_request(&req);
        self.inner
            .lock()
            .pending_permission
            .insert(request.tool_call_id.clone(), responder);
        let key = self.active_session_id().unwrap_or_default();
        self.host.permission(&key, request);
    }

    /// Server→client requests the UI can answer (recipe params, elicitation).
    /// The same parked-responder seam as the spine: the core exposes
    /// `respond_recipe_params`/`respond_elicitation` regardless of which wire
    /// asked, so declaring the capability on a peer now means it works there.
    fn on_server_request(&self, method: &str, params: &Value, responder: Responder<Value>) {
        match method {
            "_goose/unstable/session/recipe/request-params" => {
                crate::spine::park_server_request(
                    PendingRequest::RecipeParams,
                    method,
                    params,
                    responder,
                );
            }
            "elicitation/create" => {
                crate::spine::park_server_request(
                    PendingRequest::Elicitation,
                    method,
                    params,
                    responder,
                );
            }
            _ => {
                let _ = responder.respond_with_error(
                    AcpError::method_not_found()
                        .data(format!("not supported by this client: {method}")),
                );
            }
        }
    }

    // -- emits ----------------------------------------------------------------

    fn emit_sessions(&self, sessions: Vec<SessionSummary>) {
        self.listener.on_roam_sessions(self.label.clone(), sessions);
    }

    fn emit_status(&self, status: &str) {
        self.listener
            .on_roam_peer_status(self.label.clone(), status.to_string());
    }
}

/// The `updatedAt` a peer's paint plan compares a cached transcript against.
///
/// It is the CORE's stamp — the very value `save_cache_for` wrote into the file —
/// and only falls back to this peer's own session list for a session the core has
/// no stamp for yet (listed, never opened). The two are copies of one field and
/// they drift: the list moves on `session_info_update`, while a turn end
/// re-baselines the core from a `session/info` probe. Comparing against the list
/// made the answer "stale" for every peer chat, forever — so each open merged a
/// `REPLAY_TAIL` window, which is what "it re-downloads the whole chat when I
/// switch" was.
pub(crate) fn freshness_stamp(
    host: &dyn crate::session::DispatchHost,
    list: &[SessionSummary],
    key: &str,
) -> String {
    let stamp = host.listed_stamp(key);
    if !stamp.is_empty() {
        return stamp;
    }
    list.iter()
        .find(|s| s.id == key)
        .map(|s| s.updated_at.clone())
        .unwrap_or_default()
}

/// The unstable shim routes session-bound RPCs through this when the session
/// belongs to a peer (`roam:<label>:` prefix) — the peer's connection answers
/// tool/extension/config calls for its own sessions.
impl RpcConn for RoamPeer {
    fn rpc(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        RoamPeer::rpc(self, method, params)
    }

    fn active_session_id(&self) -> Option<String> {
        RoamPeer::active_session_id(self)
    }
}

impl crate::session::ChatEndpoint for RoamPeer {
    fn request(&self, method: &str, params: Value) -> BoxFuture<'static, Result<Value, AcpError>> {
        let conn = self.inner.lock().conn.clone();
        let msg = UntypedMessage::new(method, self.wire_params(params));
        match (conn, msg) {
            (Some(conn), Ok(msg)) => {
                Box::pin(async move { conn.send_request(msg).block_task().await })
            }
            (None, _) => {
                Box::pin(async { Err(AcpError::internal_error().data("roam peer not connected")) })
            }
            (_, Err(error)) => Box::pin(async move { Err(error) }),
        }
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), AcpError> {
        RoamPeer::notify(self, method, params)
    }

    fn wire_id(&self, display: &str) -> String {
        self.strip_prefix(display)
    }

    fn accepts_prompt(&self) -> bool {
        matches!(self.status(), ConnectionStatus::Ready)
    }

    fn label(&self) -> Option<String> {
        Some(self.label.clone())
    }

    fn session_cwd(&self, display: &str) -> Option<String> {
        let raw = self.strip_prefix(display);
        self.inner.lock().session_cwds.get(&raw).cloned()
    }
}

// ---------------------------------------------------------------------------
// SessionNotification translation helpers (the "same dispatch the spine uses")
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Routing (CONTRACT §6: chat routes to the last-opened session's owner)
// ---------------------------------------------------------------------------

/// Route a chat intent (CONTRACT §6: chat routes to the last-opened session's
/// owner). Pick the peer that owns the active session: the one whose label
/// equals `active_label`. `None` when no roam session is active (chat routes
/// to the main connection). The spine calls this from its chat/unstable intents.
pub(crate) fn active_peer<'a>(
    peers: &'a [Arc<RoamPeer>],
    active_label: &Option<String>,
) -> Option<&'a Arc<RoamPeer>> {
    let label = active_label.as_ref()?;
    peers.iter().find(|peer| &peer.label == label)
}

/// Resolve the peer that owns a `roam:<label>:<id>` session.
///
/// The id is a namespaced string, not a tuple: the label itself may contain
/// `:` (connection-card labels are free-form). Splitting on the first `:` and
/// looking for a peer with the first component used to miss those peers and
/// let the id fall through to the MAIN wire, where a `roam:` session silently
/// no-ops. Match the full `roam:<label>:` prefix of each KNOWN peer instead,
/// and take the longest match so that a label `desk:1` wins over `desk` for
/// `roam:desk:1:s2` while `roam:desk:s1` still resolves to `desk`.
///
/// Called from `Core`'s routing seam in `lib.rs`; the allow keeps the lib
/// target quiet until that call site is switched over from `split(':')`.
#[allow(dead_code)]
pub(crate) fn peer_for_session(
    peers: &[Arc<RoamPeer>],
    session_id: &str,
) -> Option<Arc<RoamPeer>> {
    let rest = session_id.strip_prefix(crate::session::ROAM_PREFIX)?;
    peers
        .iter()
        .filter(|peer| {
            rest.len() > peer.label.len()
                && rest.starts_with(&peer.label)
                && rest.as_bytes()[peer.label.len()] == b':'
        })
        .max_by_key(|peer| peer.label.len())
        .cloned()
}

/// Snapshot of every session known to the live roam peers, keyed by the
/// display id (`roam:<label>:<raw>`) the clients route by. The core's
/// `all_sessions()` concatenates this with the main wire's list: labels are
/// unique per registry, so the two namespaces cannot collide.
#[allow(dead_code)] // the core host calls this from `Core::all_sessions`
pub(crate) fn all_peer_sessions(peers: &[Arc<RoamPeer>]) -> Vec<SessionSummary> {
    peers.iter().flat_map(|peer| peer.sessions()).collect()
}

/// Every page of a peer's `session/list`, merged into one reply.
///
/// The peer's goose pages at 50 rows ordered by last message, exactly like the main
/// wire's, so a busy peer pushes a project's own chats off the first page and the drawer
/// draws one row where there used to be five (see `Core::list_sessions`). A first page
/// that FAILS is an error — the caller reports it — while a later page that fails keeps
/// what was already collected, so one bad cursor cannot blank the list.
///
/// `tags` narrows every page to sessions carrying those pairs — the only read a tag
/// has, and how a peer's modes are learned.
async fn list_all_peer_sessions(
    cx: &agent_client_protocol::ConnectionTo<Agent>,
    tags: &[(String, String)],
) -> Result<ListSessionsResponse, AcpError> {
    // Every page needs the bound, not just the first one in the handshake:
    // the mode walk and Relist call this helper too, and a stalled page there
    // used to park the peer's whole command loop forever.
    const PAGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
    let mut all: Vec<SessionInfo> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut first = true;
    for _ in 0..crate::session::SESSION_LIST_MAX_PAGES {
        let mut req = ListSessionsRequest::new().meta(crate::session::session_list_meta(tags));
        if let Some(next) = cursor.clone() {
            req = req.cursor(next);
        }
        let page = tokio::time::timeout(PAGE_TIMEOUT, cx.send_request(req).block_task()).await;
        let page = match page {
            Ok(reply) => reply,
            Err(_) => Err(AcpError::internal_error()
                .data("roam session/list page timed out")),
        };
        match page {
            Ok(page) => {
                first = false;
                all.extend(page.sessions);
                match page.next_cursor {
                    Some(next) if !next.is_empty() => cursor = Some(next),
                    _ => break,
                }
            }
            Err(error) => {
                if first {
                    return Err(AcpError::internal_error()
                        .data(format!("roam session/list: {error}")));
                }
                break;
            }
        }
    }
    Ok(ListSessionsResponse::new(all))
}

/// Which of a peer's sessions are NOT plain chats, by raw session id.
///
/// A mode is a tag, and this server writes tags and filters on them but never returns
/// one on a row — so the only read is one filtered `session/list` per known non-default
/// value, the same walk the main connection does. A failure here is NOT fatal: the list
/// still renders, and the affected rows simply read as chats.
///
/// Known gap, shared with the main wire: goose omits empty sessions from `session/list`,
/// so a chat with no messages yet is invisible to this walk. [`PeerInner::open_session_mode`]
/// covers the one this client just created.
async fn peer_session_modes(
    cx: &agent_client_protocol::ConnectionTo<Agent>,
) -> HashMap<String, String> {
    let mut modes = HashMap::new();
    for value in crate::session::MODE_VALUES
        .iter()
        .filter(|value| **value != crate::session::DEFAULT_MODE)
    {
        let tags = [(
            crate::session::MODE_TAG_KEY.to_string(),
            (*value).to_string(),
        )];
        if let Ok(list) = list_all_peer_sessions(cx, &tags).await {
            for info in list.sessions {
                modes.insert(info.session_id.to_string(), (*value).to_string());
            }
        }
    }
    modes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConfigOption, ItemKind};
    use agent_client_protocol::schema::v1::{
        ConfigOptionUpdate, ContentBlock, ContentChunk, SessionInfo, SessionInfoUpdate,
        TextContent, ToolCall, ToolCallId,
    };
    use serde_json::{json, Map};
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
            // Advisory: never a lifecycle state, but the arm keeps the match
            // exhaustive and the recording listener from choking on one.
            ConnectionStatus::Alert { .. } => "Alert",
        }
    }

    impl CoreListener for RecordingListener {
    fn on_advisor_report(&self, _: String, _: String) {}

    fn on_sensitivity(&self, _data: String) {}
    fn on_thread_timeline(&self, _label: String, _rows: Vec<crate::TimelineRow>) {}
    fn on_file(&self, _: crate::attachments::FileEvent) {}
        fn on_replay_progress(&self, _: String, _: i64, _: i64, _: bool) {}
        fn on_thread_ready(&self, _: String, _: String) {}
        fn on_status(&self, status: ConnectionStatus) {
            self.events
                .lock()
                .push(format!("status {}", status_name(&status)));
        }
        fn on_sessions(&self, sessions: Vec<SessionSummary>) {
            self.events
                .lock()
                .push(format!("sessions {}", sessions.len()));
        }
        fn on_usage(&self, used: i64, size: i64, _cost: f64, currency: String) {
            self.events
                .lock()
                .push(format!("usage {used} {size} {currency}"));
        }
        fn on_run_ended(&self, _session_id: String, stop_reason: String) {
            self.events.lock().push(format!("run_ended {stop_reason}"));
        }
        fn on_speak(&self, text: String) {
            self.events.lock().push(format!("speak {text}"));
        }
        fn on_item(&self, op: crate::TranscriptOp) {
            use crate::TranscriptOp as Op;
            let line = match &op {
                Op::Reset { .. } => "item Reset".to_string(),
                Op::Upsert { item } => format!("item Upsert {} kind={:?}", item.id, item.kind),
                Op::AppendText { id, chunk } => format!("item AppendText {id} {chunk}"),
                Op::AppendOutput { id, chunk } => format!("item AppendOutput {id} {chunk}"),
                Op::Remove { id } => format!("item Remove {id}"),
                Op::Window { has_older, .. } => format!("item Window has_older={has_older}"),
            };
            self.events.lock().push(line);
        }
        fn on_config(&self, options: Vec<crate::ConfigOption>) {
            self.events.lock().push(format!("config {}", options.len()));
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
        fn on_session_opened(&self, session_id: String) {
            self.events.lock().push(format!("session_opened {session_id}"));
        }

        fn on_active_run(&self, session_id: String, run_id: String) {
            self.events
                .lock()
                .push(format!("active_run {session_id} {run_id}"));
        }

        fn on_commands(&self, _commands: Vec<String>) {}
        fn on_session_open_failed(&self, _session_id: String, _message: String) {}
        fn on_tasks(&self, _: String) {}
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

    /// The production dispatch host composes the core; tests record the same
    /// decisions on the listener.
    struct TestHost {
        label: String,
        hub: Arc<crate::session::SessionHub>,
        recorder: Arc<RecordingListener>,
        replaying: Mutex<std::collections::HashSet<String>>,
        /// The core's freshness baseline per key (`Core::session_updated_at`).
        stamps: Mutex<std::collections::HashMap<String, String>>,
        /// Keys whose stamp was re-read after a load.
        probes: Mutex<Vec<String>>,
    }

    impl crate::session::DispatchHost for TestHost {
        fn display_id(&self, wire: &str) -> String {
            crate::session::display_id(Some(&self.label), wire)
        }
        fn store(&self, key: &str) -> Arc<crate::transcript::TranscriptStore> {
            self.hub.store(key)
        }
        fn touched(&self, key: String, title: String, updated_at: String) {
            self.recorder
                .events
                .lock()
                .push(format!("touched {key} {title} {updated_at}"));
        }
        fn active_run(&self, key: String, run_id: String) {
            self.recorder
                .events
                .lock()
                .push(format!("active_run {key} {run_id}"));
            if !run_id.is_empty() {
                self.replaying.lock().remove(&key);
            }
        }
        fn turn_lost(&self, key: &str) {
            self.recorder
                .events
                .lock()
                .push(format!("turn_lost {key}"));
        }
        fn config(&self, key: &str, options: Vec<ConfigOption>) {
            if self.hub.is_displayed(key) {
                self.recorder
                    .events
                    .lock()
                    .push(format!("config {}", options.len()));
            }
        }
        fn usage(&self, key: &str, used: i64, size: i64, _cost: f64, currency: String) {
            if self.hub.is_displayed(key) {
                self.recorder
                    .events
                    .lock()
                    .push(format!("usage {used} {size} {currency}"));
            }
        }
        fn commands(&self, _key: &str, _names: Vec<String>) {}
        fn permission(&self, key: &str, request: crate::PermissionRequest) {
            if self.hub.is_displayed(key) {
                self.recorder
                    .events
                    .lock()
                    .push(format!("permission {}", request.tool_call_id));
            }
        }
        fn begin_replay(&self, key: &str, _suppress: bool) {
            self.replaying.lock().insert(key.to_string());
        }
        fn end_replay(&self, key: &str) {
            self.replaying.lock().remove(key);
        }
        fn replaying(&self, key: &str) -> bool {
            self.replaying.lock().contains(key)
        }
        fn accepts_chat(&self, _key: &str) -> bool {
            true
        }
        fn background(&self, key: &str) {
            self.hub.mark_new(key);
        }
        fn endpoint_ready(&self, _label: Option<&str>) {}
        fn cache_session(&self, _key: &str) {}
        fn session_stamp(&self, key: &str, updated_at: &str) {
            if !updated_at.is_empty() {
                self.stamps.lock().insert(key.to_string(), updated_at.to_string());
            }
        }
        fn listed_stamp(&self, key: &str) -> String {
            self.stamps.lock().get(key).cloned().unwrap_or_default()
        }
        fn is_incomplete(&self, _key: &str) -> bool {
            false
        }
        fn turn_in_flight(&self, key: &str) -> bool {
            self.recorder
                .events
                .lock()
                .iter()
                .any(|e| e == &format!("in_flight {key}"))
        }
        fn clear_incomplete(&self, key: &str) {
            self.recorder
                .events
                .lock()
                .push(format!("settled {key}"));
        }
        fn mark_incomplete(&self, key: &str) {
            self.recorder
                .events
                .lock()
                .push(format!("mark_incomplete {key}"));
        }
        fn replay_tick(&self, key: &str, message_id: &str) {
            self.recorder
                .events
                .lock()
                .push(format!("replay_tick {key} {message_id}"));
        }
        fn paging(&self, _key: &str) -> bool {
            false
        }
        fn replay_backlog(&self, key: &str, skipped: usize, tail: usize) {
            self.recorder
                .events
                .lock()
                .push(format!("backlog {key} {skipped} {tail}"));
        }
        fn tool_run(&self, _key: &str, _run_id: &str) {}
        fn step_closed(&self, _key: &str) {}
        fn stamp_probe(&self, key: &str, _persist: bool) {
            self.probes.lock().push(key.to_string());
        }
        fn is_summarizer(&self, _key: &str) -> bool {
            false
        }
        fn summarizer_chunk(&self, _key: &str, _text: &str) -> bool {
            false
        }
        fn take_summarizer_reply(&self, _key: &str) -> String {
            String::new()
        }
    }

    fn offline_peer(
        label: &str,
        listener: Arc<RecordingListener>,
        is_active: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> (Arc<RoamPeer>, tokio_mpsc::UnboundedReceiver<PeerCommand>) {
        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
        // The supervisor slot gets its own (unused) channel: these tests drive
        // the state machine directly and assert on `cmd_rx` returned below,
        // never through a live command loop.
        let (_slot_tx, slot_rx) = tokio_mpsc::unbounded_channel();
        let as_dyn: Arc<dyn CoreListener> = listener.clone();
        let hub = crate::session::SessionHub::new(listener.clone());
        // The old `is_active` gate is now the hub's displayed key. Tests use
        // session `s1`.
        if is_active() {
            hub.display(&format!("roam:{label}:s1"));
        }
        let host = Arc::new(TestHost {
            label: label.to_string(),
            hub: hub.clone(),
            recorder: listener,
            replaying: Mutex::new(std::collections::HashSet::new()),
            stamps: Mutex::new(std::collections::HashMap::new()),
            probes: Mutex::new(Vec::new()),
        });
        let peer = Arc::new(RoamPeer {
            label: label.to_string(),
            inner: Mutex::new(PeerInner::new()),
            cmd_tx,
            cmd_rx: Arc::new(tokio::sync::Mutex::new(slot_rx)),
            listener: as_dyn,
            host,
            hub,
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

    // -- custom-notification keys (the ABI-4 session gate) ---------------------

    #[test]
    fn custom_notification_keys_are_namepaced_for_the_gate() {
        // The gate in `dispatch_notification` compares `sessionId` against the
        // DISPLAYED id, and a peer chat's displayed id is `roam:<label>:<raw>`.
        // The peer must key its notifications the same way or the gate eats
        // them (grouse stats regression).
        let keyed = RoamPeer::prefix_notification_key(
            &json!({"sessionId": "s-1", "update": {"sessionUpdate": "message_usage"}}),
            "colins-box",
        );
        assert_eq!(
            keyed.get("sessionId").and_then(Value::as_str),
            Some("roam:colins-box:s-1")
        );
        // The rest of the payload survives the rewrite intact.
        assert_eq!(
            keyed.pointer("/update/sessionUpdate"),
            Some(&json!("message_usage"))
        );

        // The main-wire chat: what the gate names and what arrives — untouched.
        let same = RoamPeer::prefix_notification_key(&json!({"sessionId": "s-2"}), "colins-box");
        assert_eq!(same.get("sessionId").and_then(Value::as_str), Some("roam:colins-box:s-2"));
        assert_eq!(same.get("update"), None);

        // Sessionless payloads (older servers) pass by design — no rewrite.
        let sessionless = RoamPeer::prefix_notification_key(&json!({"used": 5}), "colins-box");
        assert_eq!(sessionless.get("sessionId"), None);
        let empty = RoamPeer::prefix_notification_key(&json!({"sessionId": ""}), "colins-box");
        assert_eq!(empty.get("sessionId").and_then(Value::as_str), Some(""));
    }

    // -- browse-mode state transitions ----------------------------------------

    #[test]
    fn browse_list_arrives_no_session_auto_opens() {
        let listener = test_listener();
        let (peer, mut cmd_rx) = offline_peer(
            "laptop",
            listener.clone(),
            gate(Arc::new(AtomicBool::new(false))),
        );

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
        let (peer, mut cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        peer.apply_sessions(&list_response(&[("s1", "Title", "2026-01-01T00:00:00Z")]));

        // Prefixed id (what the UI holds) is stripped back to the raw id.
        peer.open_session("roam:laptop:s1".to_string(), "/home/user".to_string());
        match cmd_rx.try_recv() {
            Ok(PeerCommand::OpenSession {
                session_id, cwd, ..
            }) => {
                assert_eq!(session_id, "s1");
                assert_eq!(cwd, "/home/user");
            }
            other => panic!("expected OpenSession command, got {other:?}"),
        }
    }

    #[test]
    fn a_forced_peer_reload_queues_the_force_flag() {
        let listener = test_listener();
        let (peer, mut cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        peer.apply_sessions(&list_response(&[("s1", "Title", "2026-01-01T00:00:00Z")]));

        // The desktop's "reload from the start": the core routes it here with
        // force, and the peer must take a full replay rather than the cache.
        peer.open_session_with("roam:laptop:s1".to_string(), String::new(), true);
        match cmd_rx.try_recv() {
            Ok(PeerCommand::OpenSession { force, .. }) => assert!(force),
            other => panic!("expected a forced OpenSession, got {other:?}"),
        }
    }

    #[test]
    fn open_session_queues_while_connecting() {
        // Parity with the main connection: a tap during the supervised
        // reconnect window must queue, not vanish — the next live link drains
        // the command channel. Only a DEAD peer (after the supervisor gave
        // up) drops opens.
        let listener = test_listener();
        let (peer, mut cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
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
        assert!(
            cmd_rx.try_recv().is_err(),
            "a disconnected peer must not accept opens"
        );
    }

    #[test]
    fn open_session_falls_back_to_session_cwd_when_blank() {
        let listener = test_listener();
        let (peer, mut cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        peer.apply_sessions(&list_response(&[("s1", "Title", "2026-01-01T00:00:00Z")]));

        // A blank cwd (roam-only client, no main last_config) must resolve to
        // the session's own cwd from session/list, not fail the load.
        peer.open_session("s1".to_string(), String::new());
        match cmd_rx.try_recv() {
            Ok(PeerCommand::OpenSession {
                session_id, cwd, ..
            }) => {
                assert_eq!(session_id, "s1");
                assert!(!cwd.is_empty());
            }
            other => panic!("expected OpenSession command, got {other:?}"),
        }
    }

    #[test]
    fn a_peer_load_prefers_the_peers_own_cwd_over_the_callers() {
        // `open_on_peer` in lib.rs supplies the MAIN connection's cwd, which
        // can be empty (roam-only client) or just wrong (the remote goose
        // rewrote working_dir). `session/load` rewrites from what we send, so
        // the peer's own remembered cwd must win.
        let listener = test_listener();
        let (peer, mut cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        peer.inner.lock().session_cwds.insert(
            "s1".to_string(),
            "/home/remote/work".to_string(),
        );
        peer.open_session(
            "roam:laptop:s1".to_string(),
            "/home/local/other".to_string(),
        );
        match cmd_rx.try_recv() {
            Ok(PeerCommand::OpenSession { cwd, .. }) => {
                assert_eq!(cwd, "/home/remote/work");
            }
            other => panic!("expected OpenSession command, got {other:?}"),
        }

        // Unknown session: the caller's value remains the fallback.
        peer.open_session("s2".to_string(), "/home/local/fallback".to_string());
        match cmd_rx.try_recv() {
            Ok(PeerCommand::OpenSession { cwd, .. }) => {
                assert_eq!(cwd, "/home/local/fallback");
            }
            other => panic!("expected OpenSession command, got {other:?}"),
        }
    }

    #[test]
    fn session_list_meta_survives_typed_deserialization() {
        // Decisive: does the typed SDK schema keep `_meta` on a session/list
        // entry, or drop it before to_summary can read it?
        let reply: ListSessionsResponse = serde_json::from_value(serde_json::json!({
            "sessions": [{
                "sessionId": "s1",
                "cwd": "/home/user",
                "title": "Chat",
                "updatedAt": "2026-01-01T00:00:00Z",
                "_meta": {
                    "messageCount": 42,
                    "lastMessageSnippet": "hi there",
                    "model": "m-a",
                    "hasRecipe": true
                }
            }]
        }))
        .expect("deserialize a session/list reply");
        assert_eq!(reply.sessions.len(), 1);
        assert!(
            reply.sessions[0].meta.is_some(),
            "the typed SessionInfo must keep _meta: {:?}",
            reply.sessions[0].meta
        );
        let summary = crate::session::summary_from_info(Some("laptop"), &reply.sessions[0])
            .expect("listed session");
        assert_eq!(summary.message_count, 42);
        assert_eq!(summary.last_message_snippet.as_deref(), Some("hi there"));
    }

    #[test]
    fn a_listed_peer_session_carries_its_meta() {
        let listener = test_listener();
        let (peer, _) = offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        let mut meta = Map::new();
        meta.insert("messageCount".into(), Value::from(42));
        meta.insert("lastMessageSnippet".into(), Value::from("hi there"));
        meta.insert("model".into(), Value::from("m-a"));
        meta.insert("hasRecipe".into(), Value::from(true));
        let mut info = SessionInfo::new("s1".to_string(), "/home/user");
        info.title = Some("Chat".into());
        info.updated_at = Some("2026-01-01T00:00:00Z".into());
        info.meta = Some(meta);
        peer.apply_sessions(&ListSessionsResponse::new(vec![info]));

        // The drawer's metadata survives both the `_meta` parse and the
        // field-by-field re-emit in sessions() (which used to zero it).
        let sessions = peer.sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "roam:laptop:s1");
        assert_eq!(sessions[0].message_count, 42);
        assert_eq!(
            sessions[0].last_message_snippet.as_deref(),
            Some("hi there")
        );
        assert_eq!(sessions[0].model, "m-a");
        assert!(sessions[0].has_recipe);
    }

    #[test]
    fn unexpected_drop_records_resume_and_stays_silent() {
        // The supervisor owns the status line while it re-dials: a drop must
        // NOT surface "disconnected"/"error" (that was the old terminal
        // behavior that made peers look dead), only record the resume and
        // free the app's turn state.
        let listener = test_listener();
        let (peer, _cmd_rx) = offline_peer(
            "laptop",
            listener.clone(),
            gate(Arc::new(AtomicBool::new(true))),
        );
        peer.apply_sessions(&list_response(&[("s1", "Title", "2026-01-01T00:00:00Z")]));
        peer.inner.lock().open_session_id = Some("s1".to_string());
        peer.inner.lock().active_run = Some("run-9".to_string());

        peer.connection_ended(Err(
            agent_client_protocol::Error::internal_error().data("roam stream ended")
        ));

        assert!(matches!(peer.status(), ConnectionStatus::Disconnected));
        assert_eq!(
            peer.inner.lock().resume_pending.as_deref(),
            Some("s1"),
            "the open session must be owed to the next live link"
        );
        let events = listener.events.lock();
        assert!(
            !events
                .iter()
                .any(|e| e.starts_with("peer_status laptop disconnected")
                    || e.starts_with("peer_status laptop error")),
            "a supervised drop emits no terminal status: {events:?}"
        );
        // The run died with the link: the empty run id frees the app's busy state.
        assert!(
            events.iter().any(|e| e == "active_run roam:laptop:s1 "),
            "the dead run id must be cleared for the app: {events:?}"
        );
        assert!(peer.inner.lock().active_run.is_none());
    }

    #[test]
    fn a_command_failure_stays_advisory_and_keeps_the_peer_ready() {
        // `fail()` alone set the terminal Error status while the command loop
        // kept running: no redial, no later intents accepted, one stranded
        // peer after a transient session/new error.
        let listener = test_listener();
        let (peer, _cmd_rx) =
            offline_peer("laptop", listener.clone(), gate(Arc::new(AtomicBool::new(true))));
        peer.inner.lock().status = ConnectionStatus::Error {
            message: "stale".to_string(),
        };
        peer.command_failed("roam session/new: timeout".to_string());

        assert!(matches!(peer.status(), ConnectionStatus::Ready));
        let events = listener.events.lock();
        assert!(
            events.iter().any(|e| e == "peer_status laptop ready"),
            "Ready must be restored so the next intent is queued: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| e == "peer_status laptop alert: roam session/new: timeout"),
            "a command failure is text only, not a dead peer: {events:?}"
        );
        assert!(
            !events.iter().any(|e| e.starts_with("peer_status laptop error:")),
            "an advisory failure must not look terminal: {events:?}"
        );
    }

    #[test]
    fn clean_end_is_terminal_and_owes_nothing() {
        let listener = test_listener();
        let (peer, _cmd_rx) = offline_peer(
            "laptop",
            listener.clone(),
            gate(Arc::new(AtomicBool::new(true))),
        );
        peer.apply_sessions(&list_response(&[("s1", "Title", "2026-01-01T00:00:00Z")]));
        peer.inner.lock().open_session_id = Some("s1".to_string());

        peer.connection_ended(Ok(()));

        assert_eq!(
            peer.inner.lock().resume_pending,
            None,
            "a deliberate close owes no resume"
        );
        let events = listener.events.lock();
        assert!(
            events
                .iter()
                .any(|e| e == "peer_status laptop disconnected"),
            "a clean end surfaces disconnected: {events:?}"
        );
    }

    #[test]
    fn peer_rows_carry_an_effective_mode() {
        // A mode is a tag, and a tag is write-only: the walk's answer arrives out of
        // band (raw session id -> mode) and is stamped on the row. Every row it did
        // not claim reads as the DEFAULT, never `None` — the same rule the main
        // connection applies, so no client has to re-derive "absent means chat".
        let listener = test_listener();
        let (peer, _cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        let mut modes = HashMap::new();
        modes.insert("s2".to_string(), "dev".to_string());
        peer.apply_sessions_with_modes(
            &list_response(&[
                ("s1", "Plain", "2026-01-01T00:00:00Z"),
                ("s2", "Dev", "2026-01-02T00:00:00Z"),
            ]),
            &modes,
        );

        let sessions = peer.inner.lock().sessions.clone();
        let mode_of = |raw: &str| {
            sessions
                .iter()
                .find(|s| s.id == format!("roam:laptop:{raw}"))
                .and_then(|s| s.mode.clone())
        };
        assert_eq!(mode_of("s2").as_deref(), Some("dev"));
        assert_eq!(mode_of("s1").as_deref(), Some("chat"));
    }

    #[test]
    fn emitted_rows_keep_the_effective_mode() {
        // `sessions()` rebuilds the uniffi record field by field; it used to
        // hardcode `mode: None` here, so the walk's answer was stored but every
        // client saw a plain chat badge for a peer's dev chats.
        let listener = test_listener();
        let (peer, _cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        let mut modes = HashMap::new();
        modes.insert("s2".to_string(), "dev".to_string());
        peer.apply_sessions_with_modes(
            &list_response(&[
                ("s1", "Plain", "2026-01-01T00:00:00Z"),
                ("s2", "Dev", "2026-01-02T00:00:00Z"),
            ]),
            &modes,
        );

        let emitted = peer.sessions();
        let mode_of = |raw: &str| {
            emitted
                .iter()
                .find(|s| s.id == format!("roam:laptop:{raw}"))
                .and_then(|s| s.mode.clone())
        };
        assert_eq!(mode_of("s2").as_deref(), Some("dev"));
        assert_eq!(mode_of("s1").as_deref(), Some("chat"));
    }

    #[test]
    fn a_relist_merges_cwds_instead_of_replacing_them() {
        let listener = test_listener();
        let (peer, _cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        peer.apply_sessions(&list_response(&[("s1", "Old", "2026-01-01T00:00:00Z")]));
        // A just-created empty session is invisible to the server list, but
        // its cwd was learned at session/new. The hidden summarizer has one
        // too, under a session the drawer never lists.
        peer.inner
            .lock()
            .session_cwds
            .insert("s9".to_string(), "/work/new".to_string());
        peer.inner.lock().session_cwds.insert(
            "summary-1".to_string(),
            "/work/summary".to_string(),
        );

        peer.apply_sessions(&list_response(&[("s2", "New", "2026-02-01T00:00:00Z")]));

        let cwds = peer.inner.lock().session_cwds.clone();
        assert_eq!(cwds.get("s2").map(String::as_str), Some("/home/user"));
        assert_eq!(cwds.get("s9").map(String::as_str), Some("/work/new"));
        assert_eq!(
            cwds.get("summary-1").map(String::as_str),
            Some("/work/summary"),
            "hidden sessions' cwds must survive a relist too"
        );

        // Server truth wins when the same session is listed with a new cwd.
        let mut moved = list_response(&[("s2", "New", "2026-02-01T00:00:00Z")]);
        moved.sessions[0].cwd = "/work/moved".into();
        peer.apply_sessions(&moved);
        assert_eq!(
            peer.inner.lock().session_cwds.get("s2").map(String::as_str),
            Some("/work/moved")
        );
    }

    #[test]
    fn cwd_memory_is_bounded_without_dropping_the_open_session() {
        let listener = test_listener();
        let (_peer, _cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        let mut stored = HashMap::new();
        for i in 0..(RoamPeer::MAX_SESSION_CWDS + 10) {
            stored.insert(format!("old-{i}"), "/old".to_string());
        }
        let fresh: HashMap<String, String> = (0..10)
            .map(|i| (format!("new-{i}"), "/new".to_string()))
            .collect();
        RoamPeer::merge_session_cwds(&mut stored, fresh.clone(), None);
        assert!(stored.len() <= RoamPeer::MAX_SESSION_CWDS);
        assert!(fresh.keys().all(|k| stored.contains_key(k)));

        // The open session's entry is protected even when everything else is
        // stale, and a smaller list does not prune at all.
        stored.insert("open".to_string(), "/open".to_string());
        RoamPeer::merge_session_cwds(&mut stored, HashMap::new(), Some("open"));
        assert_eq!(stored.get("open").map(String::as_str), Some("/open"));
    }

    #[test]
    fn an_empty_new_chat_keeps_the_mode_it_was_created_with() {
        // goose's session/list omits sessions with no messages, so a chat this client
        // just created is unioned in from local knowledge — including its mode, which
        // neither the list nor the per-mode walk can see yet.
        let listener = test_listener();
        let (peer, _cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        {
            let mut inner = peer.inner.lock();
            inner.open_session_id = Some("s9".to_string());
            inner.open_session_mode = Some("dev".to_string());
        }
        peer.apply_sessions_with_modes(
            &list_response(&[("s1", "Other", "2026-01-01T00:00:00Z")]),
            &HashMap::new(),
        );

        let sessions = peer.inner.lock().sessions.clone();
        let fresh = sessions
            .iter()
            .find(|s| s.id == "roam:laptop:s9")
            .expect("the open-but-empty chat is unioned into the list");
        assert_eq!(fresh.mode.as_deref(), Some("dev"));
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
        let (peer, mut cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        peer.apply_sessions(&list_response(&[("s1", "Title", "2026-01-01T00:00:00Z")]));
        peer.new_session("/home/user".to_string());
        match cmd_rx.try_recv() {
            Ok(PeerCommand::NewSession { cwd, .. }) => assert_eq!(cwd, "/home/user"),
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
        let (peer, mut cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        peer.relist();
        match cmd_rx.try_recv() {
            Ok(PeerCommand::Relist) => {}
            other => panic!("expected Relist command, got {other:?}"),
        }
    }

    #[test]
    fn new_session_queues_while_connecting_rejects_dead() {
        let listener = test_listener();
        let (peer, mut cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        peer.new_session("/home/user".to_string());
        match cmd_rx.try_recv() {
            Ok(PeerCommand::NewSession { cwd, .. }) => assert_eq!(cwd, "/home/user"),
            other => panic!("expected NewSession queued while connecting, got {other:?}"),
        }
        peer.connection_ended(Ok(()));
        peer.new_session("/home/user".to_string());
        assert!(
            cmd_rx.try_recv().is_err(),
            "a disconnected peer must not accept creates"
        );
    }

    #[test]
    fn applying_a_new_list_replaces_sessions() {
        let listener = test_listener();
        let (peer, _cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        peer.apply_sessions(&list_response(&[("s1", "Old", "2026-01-01T00:00:00Z")]));
        peer.apply_sessions(&list_response(&[("s2", "New", "2026-02-01T00:00:00Z")]));
        let sessions = peer.sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "roam:laptop:s2");
    }

    #[test]
    fn apply_sessions_unions_the_open_empty_session() {
        // goose's session/list only returns sessions that HAVE messages, so a
        // chat this client just created (open, zero messages) is invisible to
        // the relist — and without a union the drawer would never learn it
        // exists. The open session must survive the list replacement until the
        // server itself lists it (first message landed).
        let listener = test_listener();
        let (peer, _cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        peer.inner.lock().open_session_id = Some("s9".to_string());
        peer.apply_sessions(&list_response(&[("s1", "Old", "2026-01-01T00:00:00Z")]));
        let sessions = peer.sessions();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, "roam:laptop:s9");
        assert_eq!(sessions[1].id, "roam:laptop:s1");
        // Once the peer lists it, no duplicate row and server truth wins.
        peer.apply_sessions(&list_response(&[
            ("s9", "New", "2026-02-01T00:00:00Z"),
            ("s1", "Old", "2026-01-01T00:00:00Z"),
        ]));
        let sessions = peer.sessions();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, "roam:laptop:s9");
        assert_eq!(sessions[0].title, "New");
    }

    #[test]
    fn archiving_the_open_session_does_not_resurrect_it() {
        let listener = test_listener();
        let (peer, _cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        peer.apply_sessions(&list_response(&[("s1", "Open", "2026-01-01T00:00:00Z")]));
        {
            let mut inner = peer.inner.lock();
            inner.open_session_id = Some("s1".to_string());
            inner.open_session_listed = true;
            inner.resume_pending = Some("s1".to_string());
        }

        // lib.rs re-lists after archive/delete; the server no longer lists s1.
        peer.apply_sessions(&list_response(&[("s2", "Other", "2026-02-01T00:00:00Z")]));

        let sessions = peer.sessions();
        assert_eq!(sessions.len(), 1, "no ghost row for the deleted chat");
        assert_eq!(sessions[0].id, "roam:laptop:s2");
        let inner = peer.inner.lock();
        assert_eq!(inner.open_session_id, None);
        assert_eq!(inner.resume_pending, None, "a dead chat owes no resume");
    }

    #[test]
    fn an_empty_open_chat_survives_a_relist_that_does_not_show_it() {
        // The opposite case: goose omits zero-message sessions, so absence
        // from the list alone must NOT clear the synthetic row.
        let listener = test_listener();
        let (peer, _cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        {
            let mut inner = peer.inner.lock();
            inner.open_session_id = Some("s9".to_string());
            inner.open_session_mode = Some("dev".to_string());
            inner.open_session_listed = false;
        }
        peer.apply_sessions(&list_response(&[("s1", "Other", "2026-01-01T00:00:00Z")]));
        let sessions = peer.sessions();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, "roam:laptop:s9");
        assert_eq!(sessions[0].mode.as_deref(), Some("dev"));
    }

    /// The freshness baseline is the CORE's stamp, not the peer's own list.
    ///
    /// Regression: the two are copies of one field and drift (a turn end
    /// re-baselines the core from a `session/info` probe; the list only moves on
    /// `session_info_update`), so comparing against the list answered "stale"
    /// for every peer chat and each open merged a REPLAY_TAIL window — the
    /// re-download-on-switch report. Here the list still holds the OLD stamp
    /// while the cache was written with the core's: the plan must say fresh.
    #[test]
    fn a_peer_paint_trusts_the_core_stamp_not_its_own_list() {
        let listener = test_listener();
        let (peer, _cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        let key = "roam:laptop:s1";
        peer.apply_sessions(&list_response(&[("s1", "Old", "2026-01-01T00:00:00Z")]));
        // The core learned the post-turn stamp; the list never caught up.
        peer.host.session_stamp(key, "2026-02-02T00:00:00Z");
        let list = peer.inner.lock().sessions.clone();
        assert_eq!(
            super::freshness_stamp(peer.host.as_ref(), &list, key),
            "2026-02-02T00:00:00Z",
            "the core's stamp wins"
        );

        let items = vec![crate::Item {
            created: 0,
            id: "m1".into(),
            kind: crate::ItemKind::Agent,
            text: "hello".into(),
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
        }];
        peer.cache.save_transcript(key, &items, "2026-02-02T00:00:00Z", true);
        let plan = crate::session::plan_paint(
            &peer.cache,
            key,
            &super::freshness_stamp(peer.host.as_ref(), &list, key),
            false,
            false,
        );
        assert!(plan.suppress, "a cache matching the core stamp is FRESH");
        assert_eq!(plan.tail, 1, "a fresh cache asks the wire for one row");

        // The list alone would have called it stale — that is the bug.
        let stale = crate::session::plan_paint(
            &peer.cache,
            key,
            &list.iter().find(|s| s.id == key).unwrap().updated_at,
            false,
            false,
        );
        assert!(stale.merge && !stale.suppress);
        assert_eq!(stale.tail, crate::session::REPLAY_TAIL);
    }

    /// A wrapper host must FORWARD the two stamp accessors, not inherit their
    /// defaults: `PrefixHost` sat between the peer and the core without them, so
    /// the peer's paint plan fell back to its own session list and every reopen
    /// replayed a tail (2026-11-06, the "still not caching" report).
    #[test]
    fn the_peer_host_forwards_the_stamp_accessors() {
        let listener = test_listener();
        let (peer, _cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        let key = "roam:laptop:s1";
        peer.host.session_stamp(key, "2026-03-03T00:00:00Z");
        let prefixed = crate::session::PrefixHost::new(
            "laptop".to_string(),
            peer.host.clone(),
            peer.hub.clone(),
        );
        use crate::session::DispatchHost as _;
        assert_eq!(prefixed.listed_stamp(key), "2026-03-03T00:00:00Z");
        assert!(!prefixed.is_incomplete(key));
    }

    /// A session the core has no stamp for yet (listed by a peer, never opened)
    /// still gets a baseline — the list's — so the first open can merge rather
    /// than repaint from scratch.
    #[test]
    fn a_peer_paint_falls_back_to_the_list_when_the_core_has_no_stamp() {
        let listener = test_listener();
        let (peer, _cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        peer.apply_sessions(&list_response(&[("s1", "Old", "2026-01-01T00:00:00Z")]));
        let list = peer.inner.lock().sessions.clone();
        assert_eq!(
            super::freshness_stamp(peer.host.as_ref(), &list, "roam:laptop:s1"),
            "2026-01-01T00:00:00Z"
        );
        assert_eq!(super::freshness_stamp(peer.host.as_ref(), &list, "roam:laptop:nope"), "");
    }

    /// The FileEvent/open id helper: a raw wire id gets this peer's namespace,
    /// an id that already carries any roam prefix is passed through untouched.
    #[test]
    fn display_id_helper_is_prefixed_and_idempotent() {
        let listener = test_listener();
        let (peer, _cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        assert_eq!(peer.display_id("s1"), "roam:laptop:s1");
        assert_eq!(peer.display_id("roam:laptop:s1"), "roam:laptop:s1");
        assert_eq!(peer.display_id("roam:nas:s2"), "roam:nas:s2");
    }

    /// `Core::all_sessions()` is fed by this snapshot: every peer's rows keep
    /// their `roam:<label>:` namespace, so the core can concatenate them with
    /// the main list without a client-side merge.
    #[test]
    fn all_peer_sessions_keeps_each_peers_namespace() {
        let a = offline_peer(
            "laptop",
            test_listener(),
            gate(Arc::new(AtomicBool::new(false))),
        )
        .0;
        let b = offline_peer(
            "nas",
            test_listener(),
            gate(Arc::new(AtomicBool::new(false))),
        )
        .0;
        a.apply_sessions(&list_response(&[("s1", "A", "2026-01-01T00:00:00Z")]));
        b.apply_sessions(&list_response(&[("s1", "B", "2026-01-01T00:00:00Z")]));
        let all = all_peer_sessions(&[a, b]);
        assert_eq!(all.len(), 2);
        let ids: Vec<_> = all.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"roam:laptop:s1"));
        assert!(ids.contains(&"roam:nas:s1"));
    }

    #[test]
    fn wire_params_strips_own_roam_prefix_from_session_id() {
        let listener = test_listener();
        let (peer, _cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
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
        let (peer, _cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(true))));
        // A raw id (already correct) and another peer's prefix (cannot reach
        // this connection) are not rewritten.
        assert_eq!(
            peer.wire_params(json!({"sessionId": "s1"}))["sessionId"],
            json!("s1")
        );
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
    fn dispatch_gates_chat_events_on_displayed_session() {
        let listener = test_listener();
        let (peer, _cmd_rx) = offline_peer(
            "laptop",
            listener.clone(),
            gate(Arc::new(AtomicBool::new(false))),
        );
        peer.inner.lock().open_session_id = Some("s1".to_string());

        let notif = SessionNotification::new(
            "s1",
            SessionUpdate::AgentMessageChunk(
                ContentChunk::new(ContentBlock::Text(TextContent::new("hello"))).message_id("m1"),
            ),
        );
        peer.dispatch(notif);

        // Background: the store accumulates, but no chat events forward; the
        // first content marks the session (green dot).
        let store = peer.hub.store("roam:laptop:s1");
        assert_eq!(store.transcript().len(), 1);
        assert_eq!(store.transcript()[0].content, "hello");
        let events = listener.events.lock();
        assert!(!events.iter().any(|e| e.starts_with("item ")), "{events:?}");
        assert!(events
            .iter()
            .any(|e| e.starts_with("touched roam:laptop:s1")));
        drop(events);

        // Displayed: the same store now paints.
        peer.hub.display("roam:laptop:s1");
        peer.dispatch(SessionNotification::new(
            "s1",
            SessionUpdate::AgentMessageChunk(
                ContentChunk::new(ContentBlock::Text(TextContent::new(" world"))).message_id("m1"),
            ),
        ));
        let events = listener.events.lock();
        assert!(events.iter().any(|e| e.starts_with("item ")), "{events:?}");
    }

    #[test]
    fn dispatch_config_option_update_forwards_when_active() {
        let listener = test_listener();
        let (peer, _cmd_rx) = offline_peer(
            "laptop",
            listener.clone(),
            gate(Arc::new(AtomicBool::new(false))),
        );

        let notif = SessionNotification::new(
            "s1",
            SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(vec![
                agent_client_protocol::schema::v1::SessionConfigOption::select(
                    "model",
                    "Model",
                    "deepseek",
                    Vec::<agent_client_protocol::schema::v1::SessionConfigSelectOption>::new(),
                ),
            ])),
        );
        // Background session: the config must not repaint the visible pickers.
        peer.dispatch(notif.clone());
        assert!(!listener
            .events
            .lock()
            .iter()
            .any(|e| e.starts_with("config ")));

        // Displayed: forwarded to on_config.
        peer.hub.display("roam:laptop:s1");
        peer.dispatch(notif);
        let events = listener.events.lock();
        assert!(events.iter().any(|e| e == "config 1"));
    }

    #[test]
    fn dispatch_tool_call_kinds() {
        let listener = test_listener();
        let (peer, _cmd_rx) = offline_peer(
            "laptop",
            listener.clone(),
            gate(Arc::new(AtomicBool::new(true))),
        );
        peer.inner.lock().open_session_id = Some("s1".to_string());

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
        // A plain call becomes a Tool item whose detail carries the raw input.
        let store = peer.hub.store("roam:laptop:s1");
        let items = store.rich_transcript();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, ItemKind::Tool);
        assert!(items[0].detail.contains("ls"), "{:?}", items[0].detail);
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
    fn routing_matches_the_longest_known_label_prefix() {
        // Connection-card labels are free-form and may contain ':'. The old
        // `rest.split(':').next()` found no peer for `desk:1`, so the roam id
        // fell through to the main wire with a `roam:` prefix and no-oped.
        let listener = test_listener();
        let off = gate(Arc::new(AtomicBool::new(false)));
        let desk = offline_peer("desk", listener.clone(), off.clone()).0;
        let nested = offline_peer("desk:1", listener, off).0;
        let peers = vec![desk, nested];

        let owner = |id: &str| peer_for_session(&peers, id).map(|p| p.label().to_string());
        assert_eq!(owner("roam:desk:s1").as_deref(), Some("desk"));
        assert_eq!(owner("roam:desk:1:s2").as_deref(), Some("desk:1"));
        // A longer label must not swallow a different short label's id.
        assert_eq!(owner("roam:desktop:s3"), None);
        assert_eq!(owner("roam:unknown:s4"), None);
        assert_eq!(owner("s5"), None);
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
        assert_eq!(
            crate::session::tool_call_status_str(&ToolCallStatus::Pending),
            "pending"
        );
        assert_eq!(
            crate::session::tool_call_status_str(&ToolCallStatus::InProgress),
            "in_progress"
        );
        assert_eq!(
            crate::session::tool_call_status_str(&ToolCallStatus::Completed),
            "completed"
        );
        assert_eq!(
            crate::session::tool_call_status_str(&ToolCallStatus::Failed),
            "failed"
        );
        assert_eq!(
            crate::session::permission_option_kind_str(&PermissionOptionKind::AllowOnce),
            "allow_once"
        );
        assert_eq!(
            crate::session::permission_option_kind_str(&PermissionOptionKind::AllowAlways),
            "allow_always"
        );
        assert_eq!(
            crate::session::permission_option_kind_str(&PermissionOptionKind::RejectOnce),
            "reject_once"
        );
        assert_eq!(
            crate::session::permission_option_kind_str(&PermissionOptionKind::RejectAlways),
            "reject_always"
        );
    }

    #[test]
    fn session_info_update_refreshes_the_session_list() {
        let listener = test_listener();
        let (peer, _cmd_rx) =
            offline_peer("laptop", listener, gate(Arc::new(AtomicBool::new(false))));
        peer.inner.lock().sessions.push(SessionSummary {
            mode: None,
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

    #[test]
    fn a_live_title_update_reaches_the_app_under_the_prefixed_id() {
        // The drawer keys peer sessions by `roam:<label>:<id>` (every other
        // chat-scoped event is prefixed; `staged_touch`'s green-dot touch always
        // was), so a raw id here matched nothing and a peer's auto-generated
        // title — or a rename from another client — only appeared after a full
        // session re-list.
        let listener = test_listener();
        let (peer, _cmd_rx) = offline_peer(
            "laptop",
            listener.clone(),
            gate(Arc::new(AtomicBool::new(true))),
        );

        peer.dispatch(SessionNotification::new(
            "s1",
            SessionUpdate::SessionInfoUpdate(
                SessionInfoUpdate::new()
                    .title("named by goose".to_string())
                    .updated_at("2026-08-23T11:00:00Z".to_string()),
            ),
        ));

        let events = listener.events.lock();
        assert!(
            events
                .iter()
                .any(|e| e == "touched roam:laptop:s1 named by goose 2026-08-23T11:00:00Z"),
            "title must carry the prefixed id: {events:?}"
        );
    }

    fn pin_test_cache() -> (CacheStore, PathBuf) {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "grouse-card-pin-test-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        (CacheStore::new(dir.clone()), dir)
    }

    #[test]
    fn card_pin_round_trip_mismatch_and_clear() {
        let (cache, dir) = pin_test_cache();
        assert_eq!(roam_card_pin(&cache, "home"), None);

        // The first dial enrolls the label; every later redial compares.
        verify_card_fingerprint(&cache, "home", "fp-one").unwrap();
        assert_eq!(roam_card_pin(&cache, "home").as_deref(), Some("fp-one"));
        verify_card_fingerprint(&cache, "home", "fp-one").unwrap();

        let error = verify_card_fingerprint(&cache, "home", "fp-two").unwrap_err();
        assert!(matches!(error, CardPinError::Mismatch { .. }));
        assert!(error.to_string().contains("changed"), "{error}");
        assert_eq!(
            roam_card_pin(&cache, "home").as_deref(),
            Some("fp-one"),
            "a refused dial must never re-pin the label"
        );

        // Disk, not an in-memory slot: a fresh CacheStore sees the same pin.
        let reopened = CacheStore::new(dir.clone());
        assert_eq!(
            roam_card_pins(&reopened).get("home").map(String::as_str),
            Some("fp-one")
        );
        // Labels are independent trust namespaces.
        verify_card_fingerprint(&cache, "nas", "fp-two").unwrap();
        assert_eq!(roam_card_pin_enroll(&cache, "nas", "fp-three"), Ok(()));
        assert_eq!(roam_card_pin(&cache, "nas").as_deref(), Some("fp-three"));

        // Explicit clear is the only way to re-enroll.
        roam_card_pin_clear(&cache, "home").unwrap();
        assert_eq!(roam_card_pin(&cache, "home"), None);
        verify_card_fingerprint(&cache, "home", "fp-two").unwrap();
        assert_eq!(roam_card_pin(&cache, "home").as_deref(), Some("fp-two"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(pins_path(&cache))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "the pin file is a trust decision");
            let temps: Vec<_> = std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
                .collect();
            assert!(temps.is_empty(), "an atomic-write temp survived: {temps:?}");
        }

        roam_card_pin_clear(&cache, "home").unwrap();
        roam_card_pin_clear(&cache, "nas").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_card_pin_store_fails_closed_until_cleared() {
        let (cache, dir) = pin_test_cache();
        let path = pins_path(&cache);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        assert!(matches!(
            verify_card_fingerprint(&cache, "home", "fp-one"),
            Err(CardPinError::Store(_))
        ));
        roam_card_pin_clear(&cache, "home").unwrap();
        verify_card_fingerprint(&cache, "home", "fp-one").unwrap();
        assert_eq!(roam_card_pin(&cache, "home").as_deref(), Some("fp-one"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
