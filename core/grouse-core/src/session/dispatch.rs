// SPDX-License-Identifier: AGPL-3.0-or-later

//! The single `session/update` notification pipeline and its host seam.
//!
//! Both the WebSocket spine and a roam peer feed notifications into [`dispatch`];
//! [`PrefixHost`] adds a peer's id prefix without forking the dispatch rules.

use super::*;
use super::translate::share_from_tool_call;

// ---------------------------------------------------------------------------
// One notification pipeline for every transport
// ---------------------------------------------------------------------------

/// What a connection asks the core to do with a `session/update`. The spine
/// and a roam peer both hand their notifications to [`dispatch`] with an
/// implementation of this; the transport is only `display_id` + the wire.
pub(crate) trait DispatchHost: Send + Sync {
    /// Display id for a wire session id (identity, or the peer prefix).
    fn display_id(&self, wire: &str) -> String;
    /// The store for a display id.
    fn store(&self, key: &str) -> Arc<crate::transcript::TranscriptStore>;
    /// Session metadata (`session_info_update`).
    fn touched(&self, key: String, title: String, updated_at: String);
    /// Run start (non-empty id) or end (empty id).
    fn active_run(&self, key: String, run_id: String);
    /// The wire carrying `key`'s turn is gone, so nothing on it can ever end
    /// that turn. Ending it is the transport's call, not a timeout's: only the
    /// connection knows the reply it was waiting for can no longer arrive.
    fn turn_lost(&self, key: &str);
    /// A config snapshot (only while `key` is displayed).
    fn config(&self, key: &str, options: Vec<ConfigOption>);
    fn usage(&self, key: &str, used: i64, size: i64, cost: f64, currency: String);
    fn commands(&self, key: &str, names: Vec<String>);
    /// A permission prompt (only while `key` is displayed).
    fn permission(&self, key: &str, request: PermissionRequest);
    /// A `session/load` replay is beginning for `key`. `suppress` marks a load
    /// whose rows must be DISCARDED: the cache it was planned from is current,
    /// so the minimal tail the server replays is already on screen and letting
    /// it land would append a second copy of the last turn (measured on the
    /// peer, which had no such gate: +64 rows on every fresh-cache open).
    fn begin_replay(&self, key: &str, suppress: bool);
    /// A live turn started for `key` (a replay is over).
    fn end_replay(&self, key: &str);
    fn replaying(&self, key: &str) -> bool;

    /// One message id ingested by a `session/load` replay, for the reload
    /// progress the clients paint.
    fn replay_tick(&self, key: &str, message_id: &str);
    /// A backfill page is being ingested for `key`. Its rows land in a SCRATCH
    /// store, so anything that belongs to the live turn — step labels, the green
    /// dot, usage, commands, config — must not fire for them (they are history,
    /// not news).
    fn paging(&self, key: &str) -> bool;
    /// The `session/load` reply reported a backlog of `skipped` leading messages
    /// (`0` = the transcript is complete), and `tail` is how much trailing
    /// history that load asked for. Drives `has_older` at the store's origin and
    /// sizes the next page.
    fn replay_backlog(&self, key: &str, skipped: usize, tail: usize);
    /// Fresh-cache opens drop replayed rows; other transports keep them.
    fn accepts_chat(&self, key: &str) -> bool;
    /// Background content arrived (green dot).
    fn background(&self, key: &str);
    /// A connection reached ready; queued intents may flush.
    fn endpoint_ready(&self, label: Option<&str>);
    /// Persist a session's store with the core's freshness/incomplete rule.
    fn cache_session(&self, key: &str);
    /// Record a listed session's `updatedAt` (the cache-freshness baseline)
    /// without emitting a touch event.
    fn session_stamp(&self, key: &str, updated_at: &str);
    /// The baseline [`DispatchHost::session_stamp`] last recorded for `key` —
    /// the exact value a cache is stamped WITH (`Core::save_cache_for`), so this
    /// is the only thing a paint plan may compare it against.
    ///
    /// A transport that keeps its own copy of the field (the roam peer kept its
    /// session list) drifts from it the moment anything updates one and not the
    /// other: the two disagree forever, `plan_paint` never reports fresh, and
    /// every single open replays [`REPLAY_TAIL`] messages — a chat that looks
    /// like it is re-downloaded on each switch. Empty means "unknown"; callers
    /// fall back to their own copy.
    fn listed_stamp(&self, key: &str) -> String;
    /// Whether `key`'s store is known to be truncated (a turn in flight, or one
    /// that ended in error), so a plan may not treat it as current or use it as
    /// a merge base. The peer used to pass a hard-coded `false` here.
    fn is_incomplete(&self, key: &str) -> bool;
    /// Whether a TURN is live for `key` right now — the one state in which the
    /// store is newer than the server (messages are persisted on completion, so
    /// the reply being streamed is not in any replay). Distinct from
    /// `is_incomplete`, which also covers a turn that ended badly.
    fn turn_in_flight(&self, key: &str) -> bool;
    /// Clear `key`'s incomplete flag: a load answered against the server, so the
    /// store is settled up to that answer.
    fn clear_incomplete(&self, key: &str);
    /// Record that `key`'s transcript is known to be MISSING rows the server holds,
    /// even though no turn is in flight. A merge gap is the case: the bounded tail
    /// started past the cache, the rebuild that was owed did not complete, and the
    /// rows on screen are the paint the client already had. Without this the next
    /// paint plan compares a cache stamp that says "current" against a transcript
    /// with a hole in it, and the hole survives every later open.
    fn mark_incomplete(&self, key: &str);
    /// The mode a session was listed or created with (`chat` / `dev`).
    ///
    /// A tag is write-only, so a mode is learned either at creation or from the
    /// per-value walk, and both are long before the turn it decides: the advisor runs
    /// at TURN END, so the core has to remember what the list taught it. Default:
    /// ignore, which is what a transport with nothing to say should do.
    fn session_mode(&self, _key: &str, _mode: &str) {}
    /// A plain tool call landed in `run_id` — the end-of-turn summarizer's
    /// worklist for `key`, oldest first.
    fn tool_run(&self, key: &str, run_id: &str);
    /// A call opened a NEW step, so every earlier step of this turn is now
    /// closed and can be labelled while the turn is still running. Without this
    /// a chip shows the provisional label (the thinking's first line) until the
    /// whole turn ends, which is most of what a reader sees.
    fn step_closed(&self, key: &str);
    /// Re-read `key`'s stamp from its server. `persist` writes the store with
    /// it — only after a FULL replay (the content is current) or our own turn.
    fn stamp_probe(&self, key: &str, persist: bool);
    /// True while `key` is the hidden summarizer session (its chunks are core
    /// bookkeeping, never chat content).
    fn is_summarizer(&self, key: &str) -> bool;
    /// Consume a summarizer agent chunk; true when `key` is a summarizer.
    fn summarizer_chunk(&self, key: &str, text: &str) -> bool;
    /// Take the summarizer's reply text (the id stays registered until the
    /// truncate completes).
    fn take_summarizer_reply(&self, key: &str) -> String;
}

/// Prefix a peer's session ids on top of the core's host.
///
/// The core owns its peers (`Core` → `RoamPeer`), so this holds a WEAK host
/// reference: a strong one would make Core → peer → host → Core a cycle and
/// leak the core.
pub(crate) struct PrefixHost {
    label: String,
    inner: std::sync::Weak<dyn DispatchHost>,
    /// The stores outlive the core's Arc in a background peer task; keeping
    /// them directly avoids a panic after the core drops.
    hub: Arc<SessionHub>,
}

impl PrefixHost {
    pub(crate) fn new(label: String, inner: Arc<dyn DispatchHost>, hub: Arc<SessionHub>) -> Self {
        Self {
            label,
            inner: Arc::downgrade(&inner),
            hub,
        }
    }

    fn host(&self) -> Option<Arc<dyn DispatchHost>> {
        self.inner.upgrade()
    }
}

/// The peer's host: the core's, with this label's prefix on display ids.
///
/// **Every trait method is required, and this wrapper forwards ALL of them — that
/// is the ratchet.** A method carrying a default body can be skipped here without
/// a word from the compiler, and the peer then behaves differently from the main
/// wire in a way nothing can see: `listed_stamp`/`is_incomplete` were defaulted,
/// so roam kept comparing a cached transcript against its own session list and
/// replayed a tail on every open (2026-11-06, two rounds to find because both my
/// tests and the compiler were looking past the wrapper). Adding a method to the
/// trait now breaks this impl until someone decides how to forward it.
impl DispatchHost for PrefixHost {
    fn display_id(&self, wire: &str) -> String {
        display_id(Some(&self.label), wire)
    }
    fn store(&self, key: &str) -> Arc<crate::transcript::TranscriptStore> {
        // The dispatch funnel: a backfill page's rows go to its scratch store
        // (see `SessionHub::dispatch_store`).
        self.hub.dispatch_store(key)
    }
    fn touched(&self, key: String, title: String, updated_at: String) {
        if let Some(host) = self.host() {
            host.touched(key, title, updated_at);
        }
    }
    fn active_run(&self, key: String, run_id: String) {
        if let Some(host) = self.host() {
            host.active_run(key, run_id);
        }
    }
    fn turn_lost(&self, key: &str) {
        if let Some(host) = self.host() {
            host.turn_lost(key);
        }
    }
    fn config(&self, key: &str, options: Vec<ConfigOption>) {
        if let Some(host) = self.host() {
            host.config(key, options);
        }
    }
    fn usage(&self, key: &str, used: i64, size: i64, cost: f64, currency: String) {
        if let Some(host) = self.host() {
            host.usage(key, used, size, cost, currency);
        }
    }
    fn commands(&self, key: &str, names: Vec<String>) {
        if let Some(host) = self.host() {
            host.commands(key, names);
        }
    }
    fn permission(&self, key: &str, request: PermissionRequest) {
        if let Some(host) = self.host() {
            host.permission(key, request);
        }
    }
    fn begin_replay(&self, key: &str, suppress: bool) {
        if let Some(host) = self.host() {
            host.begin_replay(key, suppress);
        }
    }
    fn replay_tick(&self, key: &str, message_id: &str) {
        if let Some(host) = self.host() {
            host.replay_tick(key, message_id);
        }
    }
    fn end_replay(&self, key: &str) {
        if let Some(host) = self.host() {
            host.end_replay(key);
        }
    }
    fn replaying(&self, key: &str) -> bool {
        self.host().is_some_and(|host| host.replaying(key))
    }
    fn paging(&self, key: &str) -> bool {
        self.host().is_some_and(|host| host.paging(key))
    }
    fn replay_backlog(&self, key: &str, skipped: usize, tail: usize) {
        if let Some(host) = self.host() {
            host.replay_backlog(key, skipped, tail);
        }
    }
    fn accepts_chat(&self, key: &str) -> bool {
        // The suppression window is the CORE's (one decision for both wires):
        // hardcoding `true` here kept a fresh-cache open from discarding its
        // replayed tail, so every such open appended the last turn again.
        match self.host() {
            Some(host) => host.accepts_chat(key),
            None => true,
        }
    }
    fn background(&self, key: &str) {
        if let Some(host) = self.host() {
            host.background(key);
        }
    }
    fn endpoint_ready(&self, _label: Option<&str>) {
        if let Some(host) = self.host() {
            host.endpoint_ready(Some(&self.label));
        }
    }
    fn cache_session(&self, key: &str) {
        if let Some(host) = self.host() {
            host.cache_session(key);
        }
    }
    fn session_stamp(&self, key: &str, updated_at: &str) {
        if let Some(host) = self.host() {
            host.session_stamp(key, updated_at);
        }
    }
    fn session_mode(&self, key: &str, mode: &str) {
        // The trait's default is a no-op, so forgetting this forward silently
        // stranded every roam DEV session's mode at the core: the advisor runs
        // at turn end, long after the list that taught us the mode.
        if let Some(host) = self.host() {
            host.session_mode(key, mode);
        }
    }
    /// FORWARDED, not defaulted. The trait's defaults are the empty stamp and
    /// `false`, which is exactly the wrong answer here: a peer's paint plan would
    /// then compare the cache against the peer's own session list again (the
    /// drift that made every roam open replay a tail) and treat an incomplete
    /// store as settled. A `DispatchHost` wrapper that forgets these two is
    /// invisible to the compiler, so the regression test lives next to the peer.
    fn listed_stamp(&self, key: &str) -> String {
        self.host().map(|host| host.listed_stamp(key)).unwrap_or_default()
    }
    fn is_incomplete(&self, key: &str) -> bool {
        self.host().is_some_and(|host| host.is_incomplete(key))
    }
    fn mark_incomplete(&self, key: &str) {
        if let Some(host) = self.host() {
            host.mark_incomplete(key);
        }
    }
    fn turn_in_flight(&self, key: &str) -> bool {
        self.host().map(|host| host.turn_in_flight(key)).unwrap_or(false)
    }
    fn clear_incomplete(&self, key: &str) {
        if let Some(host) = self.host() {
            host.clear_incomplete(key);
        }
    }
    fn tool_run(&self, key: &str, run_id: &str) {
        if let Some(host) = self.host() {
            host.tool_run(key, run_id);
        }
    }
    fn step_closed(&self, key: &str) {
        if let Some(host) = self.host() {
            host.step_closed(key);
        }
    }
    fn stamp_probe(&self, key: &str, persist: bool) {
        if let Some(host) = self.host() {
            host.stamp_probe(key, persist);
        }
    }
    fn is_summarizer(&self, key: &str) -> bool {
        self.host().is_some_and(|host| host.is_summarizer(key))
    }
    fn summarizer_chunk(&self, key: &str, text: &str) -> bool {
        self.host().is_some_and(|host| host.summarizer_chunk(key, text))
    }
    fn take_summarizer_reply(&self, key: &str) -> String {
        self.host()
            .map(|host| host.take_summarizer_reply(key))
            .unwrap_or_default()
    }
}

/// What a `session_info_update` declares about its session's run, in goose's
/// three states:
///
/// * `Some(id)` — a run is live; its id is the steer key.
/// * `Some("")` — goose EXPLICITLY says no run is live (`activeRunId: null`).
///   This is the client's earliest authoritative end-of-turn, emitted before
///   goose answers `session/prompt`.
/// * `None` — the update says nothing about runs. The generated session-name
///   update carries only `title`/`messageCount` with no `goose` key and can
///   land mid-turn, so it must never be read as an end.
pub(crate) fn declared_run_state(
    info: &agent_client_protocol::schema::v1::SessionInfoUpdate,
) -> Option<String> {
    info.meta
        .as_ref()
        .and_then(|meta| meta.get("goose"))
        .and_then(|goose| goose.get("activeRunId"))
        .map(|value| value.as_str().unwrap_or_default().to_string())
}

/// The single `session/update` dispatch. Every transport calls this; there is
/// no second copy of the accumulation rules.
pub(crate) fn dispatch(
    host: &dyn DispatchHost,
    notif: agent_client_protocol::schema::v1::SessionNotification,
) {
    use agent_client_protocol::schema::v1::SessionUpdate;

    let wire = notif.session_id.to_string();
    let key = host.display_id(&wire);
    // The hidden summarizer session is the core's own bookkeeping: collect its
    // text and keep every one of its updates out of the chat stores.
    if let agent_client_protocol::schema::v1::SessionUpdate::AgentMessageChunk(chunk) =
        &notif.update
    {
        if host.summarizer_chunk(&key, &chunk_text(chunk)) {
            return;
        }
    } else if host.is_summarizer(&key) {
        return;
    }
    // A replay's progress counts MESSAGES, not chunks — a message arrives as
    // many — so the number is comparable with the server's `messageCount`.
    if host.replaying(&key) {
        let mid = match &notif.update {
            SessionUpdate::UserMessageChunk(c) => c.message_id.as_ref().map(|m| m.to_string()),
            SessionUpdate::AgentMessageChunk(c) => c.message_id.as_ref().map(|m| m.to_string()),
            SessionUpdate::AgentThoughtChunk(c) => c.message_id.as_ref().map(|m| m.to_string()),
            _ => None,
        };
        if let Some(mid) = mid {
            host.replay_tick(&key, &mid);
        }
    }
    let store = host.store(&key);
    // A backfill page is HISTORY: its rows land in a scratch store and must not
    // drive the live turn's machinery (labels, green dot, usage, commands).
    let paging = host.paging(&key);
    let mut content = false;
    match notif.update {
        SessionUpdate::UserMessageChunk(chunk) => {
            if host.accepts_chat(&key) {
                if let Some(uri) = content_image_uri(&chunk.content) {
                    store.append_image(
                        "user",
                        &uri,
                        chunk
                            .message_id
                            .as_ref()
                            .map(|id| id.to_string())
                            .as_deref(),
                    );
                } else {
                    let text = chunk_text(&chunk);
                    if !text.is_empty() {
                        let created = chunk_created(&chunk);
                        let mid = chunk.message_id.map(|id| id.to_string());
                        store.append_chunk_at("user", &text, mid.as_deref(), false, created);
                    }
                }
                content = true;
            }
        }
        SessionUpdate::AgentMessageChunk(chunk) => {
            if host.accepts_chat(&key) {
                if let Some(uri) = content_image_uri(&chunk.content) {
                    store.append_image(
                        "agent",
                        &uri,
                        chunk
                            .message_id
                            .as_ref()
                            .map(|id| id.to_string())
                            .as_deref(),
                    );
                } else {
                    let text = chunk_text(&chunk);
                    if !text.is_empty() {
                        let created = chunk_created(&chunk);
                        let mid = chunk.message_id.map(|id| id.to_string());
                        store.append_chunk_at("agent", &text, mid.as_deref(), false, created);
                    }
                }
                content = true;
            }
        }
        SessionUpdate::AgentThoughtChunk(chunk) => {
            if host.accepts_chat(&key) {
                let text = chunk_text(&chunk);
                if !text.is_empty() {
                    if host.replaying(&key) {
                        // A replayed thought is framing for the tool run it
                        // precedes, never a row of its own.
                        store.replay_thought(&text);
                    } else {
                        store.append_chunk("thought", &text, None, true);
                        content = true;
                    }
                }
            }
        }
        SessionUpdate::ToolCall(tool) => {
            if host.accepts_chat(&key) {
                let id = tool.tool_call_id.to_string();
                // A SHARE is not a step in a loop: it becomes its own card, and the
                // rest of the tool machinery (runs, summaries, the chip group) is
                // skipped for it. Recognising it from the CALL (meta + arguments)
                // is what keeps the card out of the model's prose.
                if let Some(file) = share_from_tool_call(&tool) {
                    store.shared_file(&file, &id);
                    // A share is still CONTENT: the background green dot must be
                    // set for a chat that produced a file. The early `return`
                    // here used to skip the `host.background` below entirely.
                    content = true;
                } else {
                    let (kind, detail) = tool_call_kind(&tool);
                    store.tool_call(&tool.title, &detail, &id, kind);
                    // Keep the tool NAME: the title is the model's prose, and the activity
                    // panel needs to know a call is a delegate (background work) rather
                    // than a step of this turn.
                    store.tool_name(&id, &tool_call_name(&tool));
                    // The end-of-turn summarizer labels the turn's plain runs.
                    if let Some(run_id) = store.run_id_for(&id).filter(|_| !paging) {
                        // The call anchors its own run ⇒ it OPENED a step, so the one
                        // above it is finished and can be labelled now rather than at
                        // the end of the turn.
                        let opened_step = run_id == id;
                        host.tool_run(&key, &run_id);
                        if opened_step {
                            host.step_closed(&key);
                        }
                    }
                    content = true;
                }
            }
        }
        SessionUpdate::ToolCallUpdate(update) => {
            if host.accepts_chat(&key) {
                apply_tool_update(&store, &update);
                content = true;
            }
        }
        SessionUpdate::UsageUpdate(usage) => {
            let (amount, currency) = usage
                .cost
                .as_ref()
                .map(|cost| (cost.amount, cost.currency.clone()))
                .unwrap_or((0.0, String::new()));
            if !paging {
                host.usage(&key, usage.used as i64, usage.size as i64, amount, currency);
            }
        }
        SessionUpdate::SessionInfoUpdate(info) => {
            // `activeRunId` is three-state, and the difference between the last
            // two states is load-bearing:
            //
            //   "run_x"  — a run started (steer becomes available).
            //   null     — goose SAYS the run is over. This is the earliest
            //              authoritative end-of-turn the client ever gets: goose
            //              emits it before it answers `session/prompt`, so a lost
            //              or late response no longer strands the turn.
            //   absent   — says nothing about runs at all. The generated
            //              session-name update carries only `title`/`messageCount`
            //              with no `goose` key, and it can land mid-turn; reading
            //              it as "no run" used to clear the steer affordance in
            //              the middle of a live run.
            if let Some(run_id) = declared_run_state(&info) {
                host.active_run(key.clone(), run_id.clone());
                // A live run id can arrive WHILE a load is replaying (the server
                // pushes touches for other clients' writes too). Ending the
                // replay here also clears the suppression window, so the rest
                // of the replay paints on top of the cache. The replay's own
                // guard owns `end_replay` and closes it at the right time.
                if !run_id.is_empty() && !host.replaying(&key) {
                    host.end_replay(&key);
                }
            }
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
            if !title.is_empty() || !updated_at.is_empty() {
                host.touched(key.clone(), title, updated_at);
            }
        }
        SessionUpdate::ConfigOptionUpdate(config) if !paging => {
            let options = config
                .config_options
                .iter()
                .map(|option| ConfigOption {
                    id: option.id.to_string(),
                    value: config_option_value(option),
                    name: option.name.clone(),
                    choices: Vec::new(),
                })
                .collect();
            host.config(&key, options);
        }
        SessionUpdate::AvailableCommandsUpdate(commands) => {
            let names = commands
                .available_commands
                .iter()
                .map(|command| command.name.clone())
                .collect();
            if !paging {
                host.commands(&key, names);
            }
        }
        _ => {}
    }
    if content && !paging {
        host.background(&key);
    }
}

/// Apply a tool lifecycle update: app promotion, live output, final content.
pub(crate) fn apply_tool_update(
    store: &crate::transcript::TranscriptStore,
    update: &agent_client_protocol::schema::v1::ToolCallUpdate,
) {
    let id = update.tool_call_id.to_string();
    let status = update
        .fields
        .status
        .as_ref()
        .map(tool_call_status_str)
        .unwrap_or_default();
    if let Some(step) = tool_subagent_step(update) {
        store.tool_step(&id, step);
    }
    if let Some((uri, ext)) = tool_call_app(update) {
        if status != "in_progress" {
            store.tool_app(&id, &uri, &ext);
        }
    }
    if let Some(chunk) = tool_live_output(update) {
        if !chunk.is_empty() {
            store.tool_update(&id, status, &chunk, true);
        }
        return;
    }
    let output = tool_final_output(update);
    store.tool_update(&id, status, &output, false);
}

