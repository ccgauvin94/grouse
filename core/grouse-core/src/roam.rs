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

// The peer implementation is split by concern; the seams below stay
// re-exported at their existing `roam::` paths.
mod pins;
mod transport;
mod state;
mod supervisor;
mod session;
mod permissions;

pub(crate) use pins::{
    roam_card_pin, roam_card_pin_clear, roam_card_pin_enroll, roam_card_pins,
 CardPinError,
};
#[allow(unused_imports)]
use pins::{pins_path, verify_card_fingerprint, verify_card_pin};
#[allow(unused_imports)]
pub(crate) use transport::{RoamCodec, RoamTransport};
pub(crate) use state::{active_peer, all_peer_sessions, freshness_stamp, peer_for_session};
pub(crate) use supervisor::{list_all_peer_sessions, peer_session_modes};


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
pub enum Outcome {
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
