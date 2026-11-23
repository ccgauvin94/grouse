// SPDX-License-Identifier: AGPL-3.0-or-later

//! Peer session lifecycle: intents into the command loop, the browse-mode
//! list, load/create replay gates and open-success emission. Moved verbatim.

use super::*;

impl RoamPeer {
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
    pub(crate) fn wire_params(&self, params: Value) -> Value {
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
    pub(crate) async fn load_session(
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
    pub(crate) async fn create_session(
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
    pub(crate) fn emit_config(&self, reply: &Value, key: &str) {
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
    pub(crate) fn merge_session_cwds(
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
    pub(crate) fn apply_sessions(&self, list: &ListSessionsResponse) {
        self.apply_sessions_with_modes(list, &HashMap::new());
    }

    /// [`Self::apply_sessions`] plus the per-mode walk's answer: raw session id ->
    /// mode, for the sessions that are NOT plain chats.
    pub(crate) fn apply_sessions_with_modes(
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

}
