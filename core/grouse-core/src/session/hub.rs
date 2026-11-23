// SPDX-License-Identifier: AGPL-3.0-or-later

//! One transcript store per session plus the display gate.
//!
//! Background sessions accumulate silently; only the displayed store paints.

use super::*;

// ---------------------------------------------------------------------------
// One store per session, one display gate
// ---------------------------------------------------------------------------

/// Every session the core knows about — the main connection's and every roam
/// peer's — gets exactly one [`TranscriptStore`]. The `displayed` key is what
/// the single UI transcript window shows; a background session's store still
/// accumulates (and is saved to the cache) but emits no painting.
///
/// This replaces the per-transport split: the old `PeerStore` + staging map and
/// the spine's single store + listener forwarder were two implementations of
/// this one idea.
pub(crate) struct SessionHub {
    listener: Arc<dyn crate::CoreListener>,
    stores: Mutex<HashMap<String, Arc<crate::transcript::TranscriptStore>>>,
    /// Scratch stores for a backfill PAGE: while a page is being ingested, the
    /// dispatch funnel hands that key's rows here instead of to the session's
    /// store, so one page can be turned into items without touching what is on
    /// screen (and a failed page changes nothing). Installed and removed by
    /// `Core::page`; see `docs/TRANSCRIPT_MODEL.md`.
    page_targets: Mutex<HashMap<String, Arc<crate::transcript::TranscriptStore>>>,
    displayed: Arc<RwLock<Option<String>>>,
    /// Sessions whose background store gained content since the last time they
    /// were displayed (the drawer's green dot).
    has_new: Mutex<HashSet<String>>,
}

impl SessionHub {
    pub(crate) fn new(listener: Arc<dyn crate::CoreListener>) -> Arc<Self> {
        Arc::new(Self {
            listener,
            stores: Mutex::new(HashMap::new()),
            page_targets: Mutex::new(HashMap::new()),
            displayed: Arc::new(RwLock::new(None)),
            has_new: Mutex::new(HashSet::new()),
        })
    }

    /// The store for a session, created on first use. Its emissions are gated:
    /// nothing paints unless this key is the displayed one.
    pub(crate) fn store(&self, key: &str) -> Arc<crate::transcript::TranscriptStore> {
        let mut stores = self.stores.lock();
        if let Some(store) = stores.get(key) {
            return store.clone();
        }
        let store = Arc::new(crate::transcript::TranscriptStore::new(Box::new(
            SessionGate {
                key: key.to_string(),
                displayed: self.displayed.clone(),
                inner: self.listener.clone(),
            },
        )));
        store.set_session(key);
        stores.insert(key.to_string(), store.clone());
        store
    }

    /// The store a NOTIFICATION for `key` belongs to: the session's own, unless a
    /// backfill page has claimed the key. Only the dispatch funnel uses this —
    /// every other caller wants [`Self::store`].
    pub(crate) fn dispatch_store(&self, key: &str) -> Arc<crate::transcript::TranscriptStore> {
        if let Some(scratch) = self.page_targets.lock().get(key) {
            return scratch.clone();
        }
        self.store(key)
    }

    /// Point (or un-point) a key's notifications at a scratch store for the
    /// duration of one backfill page.
    pub(crate) fn set_page_target(
        &self,
        key: &str,
        target: Option<Arc<crate::transcript::TranscriptStore>>,
    ) {
        let mut targets = self.page_targets.lock();
        match target {
            Some(store) => {
                targets.insert(key.to_string(), store);
            }
            None => {
                targets.remove(key);
            }
        }
    }

    /// Whether a store exists for `key` — the chat has been opened (or painted)
    /// in this process, so a prompt naming it is not a stale UI id.
    pub(crate) fn has_store(&self, key: &str) -> bool {
        self.stores.lock().contains_key(key)
    }

    /// Make `key` the session the UI shows. Does not paint by itself.
    pub(crate) fn display(&self, key: &str) -> Arc<crate::transcript::TranscriptStore> {
        let store = self.store(key);
        *self.displayed.write() = Some(key.to_string());
        self.has_new.lock().remove(key);
        store
    }

    /// Show no session (a fresh chat whose id is not known yet).
    pub(crate) fn clear_display(&self) {
        *self.displayed.write() = None;
    }

    pub(crate) fn is_displayed(&self, key: &str) -> bool {
        self.displayed.read().as_deref() == Some(key)
    }

    pub(crate) fn displayed_key(&self) -> Option<String> {
        self.displayed.read().clone()
    }

    pub(crate) fn displayed_store(&self) -> Option<Arc<crate::transcript::TranscriptStore>> {
        self.displayed_key().map(|key| self.store(&key))
    }

    /// A background session produced content: set its green dot once.
    pub(crate) fn mark_new(&self, key: &str) {
        if self.is_displayed(key) {
            return;
        }
        if self.has_new.lock().insert(key.to_string()) {
            self.listener.on_session_touched(
                key.to_string(),
                String::new(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis().to_string())
                    .unwrap_or_default(),
            );
        }
    }

    pub(crate) fn has_new(&self, key: &str) -> bool {
        self.has_new.lock().contains(key)
    }

    /// Drop a session's store (deleted session, closed peer).
    pub(crate) fn forget(&self, key: &str) {
        self.stores.lock().remove(key);
        // A page claimed this key just before the session was deleted or the
        // peer closed. Leaving the target installed would route that key's late
        // notifications into a scratch store nobody reads (and resurrect a
        // store we just dropped).
        self.page_targets.lock().remove(key);
        self.has_new.lock().remove(key);
        if self.displayed_key().as_deref() == Some(key) {
            self.clear_display();
        }
    }
}

/// The sink for a scratch store: rows land in the store, nothing is emitted.
/// Used to turn one backfill page's notifications into items without touching
/// the transcript on screen — see `Core::page` and `docs/TRANSCRIPT_MODEL.md`.
pub(crate) struct NullListener;

impl crate::CoreListener for NullListener {
    fn on_session_opened(&self, _: String) {}
    fn on_file(&self, _: crate::attachments::FileEvent) {}
    fn on_advisor_report(&self, _session_id: String, _text: String) {}

    fn on_sensitivity(&self, _: String) {}
    fn on_status(&self, _: crate::ConnectionStatus) {}
    fn on_sessions(&self, _: Vec<crate::SessionSummary>) {}
    fn on_item(&self, _: crate::TranscriptOp) {}
    fn on_usage(&self, _: i64, _: i64, _: f64, _: String) {}
    fn on_run_ended(&self, _: String, _: String) {}
    fn on_thread_timeline(&self, _: String, _: Vec<crate::TimelineRow>) {}
    fn on_speak(&self, _: String) {}
    fn on_config(&self, _: Vec<crate::ConfigOption>) {}
    fn on_permission_request(&self, _: crate::PermissionRequest) {}
    fn on_session_touched(&self, _: String, _: String, _: String) {}
    fn on_projects(&self, _: Vec<crate::ProjectSummary>) {}
    fn on_roam_peer_status(&self, _: String, _: String) {}
    fn on_roam_sessions(&self, _: String, _: Vec<crate::SessionSummary>) {}
    fn on_peer_new_session(&self, _: String, _: String) {}
    fn on_active_run(&self, _: String, _: String) {}
    fn on_commands(&self, _: Vec<String>) {}
    fn on_session_open_failed(&self, _: String, _: String) {}
    fn on_tasks(&self, _: String) {}
    fn on_thread_ready(&self, _: String, _: String) {}
    fn on_replay_progress(&self, _: String, _: i64, _: i64, _: bool) {}
}

/// The per-store listener: paints only while its key is displayed; everything
/// that is turn CONTROL (run end, open failure) and not painting passes.
struct SessionGate {
    key: String,
    displayed: Arc<RwLock<Option<String>>>,
    inner: Arc<dyn crate::CoreListener>,
}

impl SessionGate {
    fn shown(&self) -> bool {
        self.displayed.read().as_deref() == Some(self.key.as_str())
    }
}

impl crate::CoreListener for SessionGate {
    fn on_sensitivity(&self, data: String) {
        // Not chat-scoped: the knob is about the whole board, so it is always forwarded.
        self.inner.on_sensitivity(data);
    }
    fn on_file(&self, event: crate::attachments::FileEvent) {
        // Not painting: a fetch was started by a row in THIS chat, so it is
        // forwarded whatever is on screen (a background fetch must not stall).
        self.inner.on_file(event);
    }
    fn on_status(&self, status: crate::ConnectionStatus) {
        self.inner.on_status(status);
    }
    fn on_sessions(&self, sessions: Vec<crate::SessionSummary>) {
        self.inner.on_sessions(sessions);
    }
    fn on_item(&self, op: crate::TranscriptOp) {
        if self.shown() {
            self.inner.on_item(op);
        }
    }
    fn on_usage(&self, used: i64, size: i64, cost: f64, currency: String) {
        if self.shown() {
            self.inner.on_usage(used, size, cost, currency);
        }
    }
    fn on_run_ended(&self, session_id: String, stop_reason: String) {
        // Turn control, not painting: a background turn must still release
        // its chat's in-flight state — which is why the key travels with it.
        self.inner.on_run_ended(session_id, stop_reason);
    }

    fn on_thread_timeline(&self, label: String, rows: Vec<crate::TimelineRow>) {
        self.inner.on_thread_timeline(label, rows);
    }
    fn on_speak(&self, text: String) {
        if self.shown() {
            self.inner.on_speak(text);
        }
    }
    fn on_advisor_report(&self, session_id: String, text: String) {
        // Chat-scoped paint: a review belongs beside the turn it reviews, so a
        // background chat's report is dropped rather than shown over another.
        if self.shown() {
            self.inner.on_advisor_report(session_id, text);
        }
    }
    fn on_config(&self, options: Vec<crate::ConfigOption>) {
        self.inner.on_config(options);
    }
    fn on_permission_request(&self, request: crate::PermissionRequest) {
        self.inner.on_permission_request(request);
    }
    fn on_session_touched(&self, session_id: String, title: String, updated_at: String) {
        self.inner.on_session_touched(session_id, title, updated_at);
    }
    fn on_projects(&self, projects: Vec<crate::ProjectSummary>) {
        self.inner.on_projects(projects);
    }
    fn on_roam_peer_status(&self, label: String, status: String) {
        self.inner.on_roam_peer_status(label, status);
    }
    fn on_roam_sessions(&self, label: String, sessions: Vec<crate::SessionSummary>) {
        self.inner.on_roam_sessions(label, sessions);
    }
    fn on_peer_new_session(&self, label: String, session_id: String) {
        self.inner.on_peer_new_session(label, session_id);
    }
    fn on_active_run(&self, session_id: String, run_id: String) {
        self.inner.on_active_run(session_id, run_id);
    }
    fn on_commands(&self, commands: Vec<String>) {
        self.inner.on_commands(commands);
    }
    fn on_session_open_failed(&self, session_id: String, message: String) {
        self.inner.on_session_open_failed(session_id, message);
    }
    fn on_session_opened(&self, session_id: String) {
        // Not a paint: it is the ANSWER to an open intent, so a background
        // session's successful bind must still reach the client that asked.
        self.inner.on_session_opened(session_id);
    }
    fn on_tasks(&self, tasks: String) {
        self.inner.on_tasks(tasks);
    }
    fn on_thread_ready(&self, label: String, session_id: String) {
        // Not gated: this is an ANSWER to a tap, and the whole point is that it
        // arrives when the chat on screen is NOT yet the thread's.
        self.inner.on_thread_ready(label, session_id);
    }
    fn on_replay_progress(&self, session_id: String, loaded: i64, total: i64, done: bool) {
        // Gated like every other per-session event: a reload of a chat that is
        // not on screen must not paint that chat's bar.
        if self.shown() && session_id == self.key {
            self.inner.on_replay_progress(session_id, loaded, total, done);
        }
    }
}
