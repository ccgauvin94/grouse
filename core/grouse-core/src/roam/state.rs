// SPDX-License-Identifier: AGPL-3.0-or-later

//! Peer state snapshots, id namespace helpers and notification dispatch.
//! The prefixed ids, display gate and run-id mirror are unchanged.

use super::*;

impl RoamPeer {
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
    pub(crate) fn report_session_opened(&self, display_id: &str) {
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

    pub(crate) fn strip_prefix(&self, id: &str) -> String {
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
    pub(crate) fn prefix_notification_key(params: &Value, label: &str) -> Value {
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
    pub(crate) fn cache_key(&self, raw: &str) -> String {
        format!("roam:{}:{raw}", self.label)
    }

    pub(crate) fn emit_sessions(&self, sessions: Vec<SessionSummary>) {
        self.listener.on_roam_sessions(self.label.clone(), sessions);
    }

    pub(crate) fn emit_status(&self, status: &str) {
        self.listener
            .on_roam_peer_status(self.label.clone(), status.to_string());
    }

    /// One peer-specific pre-pass (its own session-list metadata), then the
    /// shared `session::dispatch`. The hub's display gate decides what paints.
    pub(crate) fn dispatch(&self, notif: SessionNotification) {
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

