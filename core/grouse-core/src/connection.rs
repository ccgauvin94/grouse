// SPDX-License-Identifier: AGPL-3.0-or-later

//! Connection lifecycle, status machine, reconnect timers and generation-bound hooks.
//! The facade, records and shared state stay in `lib.rs`; this module
//! contributes inherent `impl Core` methods only. Behaviour is unchanged.

use super::*;

impl Core {
    /// Build a fresh connection: gracefully shut down any current one, wire
    /// the hooks, register the conn for the unstable shim, and spawn the
    /// connection task. Returns the conn and the ready receiver (the bounded
    /// `connect()` waits on it; reconnect/timer paths ignore it).
    pub(crate) fn connect_impl(
        &self,
        config: ServerConfig,
        spec: ConnectSpec,
        suppress_replay: bool,
        merge_replay: bool,
    ) -> (
        Arc<crate::spine::Conn>,
        oneshot::Receiver<Result<(), String>>,
    ) {
        let gen = {
            let mut state = self.inner.state.lock();
            state.conn_gen += 1;
            let gen = state.conn_gen;
            // The handshake announces one successful bind on Ready. A Resume
            // names the session the UI asked for; a New learns its id from the
            // reply (and only announces one that actually became the display).
            // An attaching wire binds nothing and watches nothing.
            state.open_watch = match &spec {
                ConnectSpec::Resume { session_id, .. } => Some(OpenWatch {
                    gen,
                    expected: Some(crate::session::display_id(None, session_id)),
                }),
                ConnectSpec::New { .. } => Some(OpenWatch { gen, expected: None }),
                ConnectSpec::Attach => None,
            };
            gen
        };
        // Graceful teardown of any previous wire.
        if let Some(old) = self.inner.conn.lock().take() {
            old.shutdown();
        }
        if let Some(task) = self.inner.conn_task.lock().take() {
            drop(task); // the old task finishes after its graceful close
        }

        let host = self.host();
        let (conn, ready_rx) = crate::spine::Conn::new(
            self.inner.listener.clone(),
            self.inner.hub.clone(),
            host,
            config.clone(),
            spec,
        );
        conn.set_suppress_replay(suppress_replay);
        conn.set_merge_replay(merge_replay);
        // Every hook is generation-bound. A superseded wire can still deliver a
        // handshake failure, config reply or terminal status after the new one
        // is live; without the guard it rewinds the new wire's state machine.
        conn.set_on_status(self.status_hook(gen));
        conn.set_on_open_failed(self.open_failed_hook(gen));
        conn.set_on_config(self.config_hook(gen));
        conn.set_on_ended(self.ended_hook(gen));
        crate::spine::set_current_conn(Some(conn.clone()));

        let transport = WsTransport::new(
            &config.host,
            config.port,
            &config.secret_key,
            config.use_tls,
            config.accept_invalid_certs,
            config.ca_cert_pem.clone(),
        );
        let task_conn = conn.clone();
        let task = crate::roam::runtime().spawn(async move {
            let result = crate::spine::run_connection(task_conn.clone(), transport).await;
            task_conn.on_connection_ended(result);
        });

        {
            let mut state = self.inner.state.lock();
            state.last_config = Some(config);
            state.user_disconnect = false;
            state.reconnect_gen += 1; // a fresh connect supersedes pending timers
            state.reconnect_attempts = 0;
        }
        *self.inner.conn.lock() = Some(conn.clone());
        *self.inner.conn_task.lock() = Some(task);
        (conn, ready_rx)
    }

    /// Block (bounded) until the connection is ready or fails. The reconnect
    /// path runs inside the runtime and never blocks.
    pub(crate) fn wait_ready(&self, ready_rx: oneshot::Receiver<Result<(), String>>) {
        if tokio::runtime::Handle::try_current().is_ok() {
            return;
        }
        let rt = crate::roam::runtime();
        rt.block_on(async {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(15), ready_rx).await;
        });
    }

    /// Mirror a conn status transition emitted by a specific wire. The direct
    /// callers (live new/load reuse) have no wire generation — they emit on
    /// behalf of the current one and stay unguarded.
    pub(crate) fn on_conn_status(&self, status: ConnectionStatus) {
        self.on_conn_status_from(None, status);
    }

    pub(crate) fn on_conn_status_from(&self, gen: Option<u64>, status: ConnectionStatus) {
        if let Some(gen) = gen {
            let current = self.inner.state.lock().conn_gen == gen;
            if !current {
                return; // a superseded wire cannot rewind the current state
            }
        }
        let mut state = self.inner.state.lock();
        if state.user_disconnect {
            drop(state);
            self.emit_status(ConnectionStatus::Disconnected);
            return;
        }
        state.status = status.clone();
        let lost = if matches!(status, ConnectionStatus::Ready) {
            state.reconnect_attempts = 0;
            state.reconnect_gen += 1;
            Vec::new()
        } else {
            // A main-wire drop kills ITS turns; a roam peer's turn is a
            // different connection and must survive this. Do NOT remove the
            // keys here: `turn_lost` is the one helper that ends a lost turn
            // everywhere (roam uses it too) — it releases `in_flight`, marks
            // the store incomplete, seals/saves it and emits `run_ended`.
            state
                .in_flight
                .iter()
                .filter(|key| !key.starts_with(crate::session::ROAM_PREFIX))
                .cloned()
                .collect()
        };
        drop(state);
        self.turns_lost(lost);
        if matches!(status, ConnectionStatus::Ready) {
            // A session/load replay just finished — fresh-cache opens, resume
            // after reconnect, and in-place resync all land here with the full
            // replayed transcript in the store. Persist it so the NEXT open
            // renders from cache (freshness matches) instead of replaying the
            // same delta again. on_prompt_done alone only covered the
            // prompt-turn case, which is why reopening a replayed session
            // re-streamed it every time.
            // An unowned store belongs to whatever session just bound: a fresh
            // session/new starts empty and its rows accumulate from here.
            // The session THIS wire bound (the display may belong to a peer).
            let bound = self
                .inner
                .conn
                .lock()
                .as_ref()
                .and_then(|conn| conn.active_session_id());
            // The handshake's bind arrived before this Ready. Announce it once
            // here; a failed load left nothing bound (and cleared its watch),
            // so the advisory Ready after grouse#19 cannot report an open.
            self.maybe_announce_open(bound.as_deref(), gen);
            if let Some(key) = bound {
                {
                    let mut state = self.inner.state.lock();
                    // The replay/new just completed, so the store is a faithful
                    // copy of the server's transcript — unless a turn is still
                    // in flight.
                    if state.in_flight.contains(&key) {
                        state.incomplete.insert(key.clone());
                    } else {
                        state.incomplete.remove(&key);
                    }
                }
                self.save_cache_for(&key);
                // Record the server's current stamp from a session/info probe
                // (no cache write: a load's store is only current up to the
                // stamp its replay carried). This makes a later open compare
                // the cache against the server's real position.
                self.refresh_stamp(&key, false);
            }
            self.flush_pending();
        }
        self.inner.listener.on_status(status);
    }

    /// End every turn in `keys` whose wire is gone. The roam path does exactly
    /// this in `RoamPeer::connection_ended`; the main wire and `roam_disconnect`
    /// used to drop their keys from `in_flight` by hand, leaving the client
    /// "working" forever with a composer full of queued prompts. Clearing
    /// `active_runs` here also stops a dead run id from re-arming the turn on a
    /// later touch; the empty run id frees busy/steer state on the client.
    pub(crate) fn turns_lost(&self, keys: impl IntoIterator<Item = String>) {
        let host = self.host();
        for key in keys {
            self.inner.state.lock().active_runs.remove(&key);
            self.inner.listener.on_active_run(key.clone(), String::new());
            crate::session::DispatchHost::turn_lost(&*host, &key);
        }
    }

    /// `session/info` probe for the active session: record its updatedAt (the
    /// cache-freshness comparison) and refresh the per-session resync baseline
    /// so our own just-completed change does not read as a remote one. Does not
    /// write the cache — see [`Self::refresh_stamp`].
    pub(crate) fn probe_stamp(&self) {
        if let Some(session_id) = self.active_session_id() {
            self.refresh_stamp(&session_id, false);
        }
    }

    /// Re-read a session's stamp from its OWNING connection. `persist` writes
    /// the store with the probed stamp — only safe when the store holds the
    /// turn that moved it (after our own prompt). Works for the main wire and
    /// a peer alike.
    pub(crate) fn refresh_stamp(&self, session_id: &str, persist: bool) {
        let Some(owner) = self.owner_for(session_id) else {
            return;
        };
        let params = json!({ "sessionId": owner.wire_id(session_id) });
        let core = self.clone();
        let key = session_id.to_string();
        crate::roam::runtime().spawn(async move {
            let probed =
                crate::session::request(owner.as_ref(), "_goose/unstable/session/info", params)
                    .await
                    .ok()
                    .and_then(|reply| reply.get("session").cloned())
                    .map(|session| {
                        let updated_at = session
                            .get("updatedAt")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let message_count = session
                            .pointer("/_meta/messageCount")
                            .and_then(Value::as_i64)
                            .unwrap_or(-1);
                        (updated_at, message_count)
                    });
            let remote_touched = {
                let state = core.inner.state.lock();
                state.touched_during_turn.contains(&key)
            };
            if let Some((updated_at, message_count)) = probed {
                let mut state = core.inner.state.lock();
                if !updated_at.is_empty() {
                    state
                        .session_updated_at
                        .insert(key.clone(), updated_at.clone());
                }
                if message_count >= 0 {
                    state.message_counts.insert(key.clone(), message_count);
                }
                // Baseline for the move test in `on_session_probe`. Without
                // this, the first `session_info_update` after a prompt (the
                // server's own run-end touch) probes "moved" and replays the
                // session we just finished streaming. A touch that arrived
                // DURING our turn is the exception: re-baselining here would
                // hide the remote rows forever (finish_turn re-arms the resync).
                if !remote_touched && !updated_at.is_empty() && message_count >= 0 {
                    state.sync_stamps.insert(key.clone(), (updated_at, message_count));
                }
            }
            if persist && !remote_touched {
                core.save_cache_for(&key);
            }
        });
    }

    /// A connection ended. Explicit disconnect → nothing more (status already
    /// surfaced as Disconnected). Superseded connection → inert. Every OTHER
    /// drop advances the backoff budget, including a wire established by
    /// `connect_attach` that bound nothing — those previously stayed offline
    /// forever because reconnect was gated on `active_session_id()`.
    ///
    /// The resume target is explicit and conservative: bind/display only the
    /// session the client is actually showing, and only if the main wire owns
    /// it. A roam chat on screen (`roam:` display id or an active peer label)
    /// means the main wire reconnects ATTACHED, never repainting the peer's
    /// chat with a stale main session (bind == display; grouse#58aaef0a).
    pub(crate) fn on_connection_ended(&self, gen: u64, _result: Result<(), String>) {
        let resume = {
            let state = self.inner.state.lock();
            if state.user_disconnect {
                drop(state);
                crate::spine::set_current_conn(None);
                return;
            }
            if state.conn_gen != gen {
                return;
            }
            let displayed = self.inner.hub.displayed_key();
            let bound = self
                .inner
                .conn
                .lock()
                .as_ref()
                .and_then(|conn| conn.active_session_id());
            let peer = self.inner.active_peer_label.read().clone();
            match (bound, displayed, peer) {
                (Some(session), Some(key), None)
                    if session == key && !session.starts_with(crate::session::ROAM_PREFIX) =>
                {
                    Some(session)
                }
                _ => None,
            }
        };
        crate::spine::set_current_conn(None);
        self.schedule_reconnect(resume);
    }

    /// Exponential backoff: 500ms·2^n capped at 15s, at most 6 attempts; reset
    /// on Ready (desktop maybeReconnect). Surfaced only via `on_status`.
    pub(crate) fn schedule_reconnect(&self, resume: Option<String>) {
        let mut state = self.inner.state.lock();
        if state.user_disconnect {
            return;
        }
        if state.reconnect_attempts >= 6 {
            drop(state);
            self.emit_status(ConnectionStatus::Error {
                message: "connection lost; giving up after 6 reconnect attempts".to_string(),
            });
            return;
        }
        let delay_ms = crate::spine::reconnect_delay_ms(state.reconnect_attempts);
        state.reconnect_gen += 1;
        let gen = state.reconnect_gen;
        drop(state);
        let core = self.clone();
        crate::roam::runtime().spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            core.on_reconnect_timer(gen, resume);
        });
    }

    pub(crate) fn on_reconnect_timer(&self, gen: u64, resume: Option<String>) {
        let mut state = self.inner.state.lock();
        if state.user_disconnect || state.reconnect_gen != gen {
            return;
        }
        state.reconnect_attempts += 1;
        let config = state.last_config.clone();
        drop(state);
        let Some(config) = config else { return };
        let Some(resume) = resume else {
            // No main-owned chat on screen: bring the wire back WITHOUT binding
            // one (a roam chat or a landing page is still displayed).
            let (_, _rx) = self.connect_impl(config, ConnectSpec::Attach, false, false);
            return;
        };
        // The reader may have switched to a peer chat during the backoff. The
        // display gate is checked again here — resuming now would bind and
        // repaint a chat nobody is looking at.
        let still_displayed = {
            let displayed = self.inner.hub.displayed_key();
            let peer = self.inner.active_peer_label.read().clone();
            peer.is_none()
                && displayed.as_deref() == Some(resume.as_str())
                && !resume.starts_with(crate::session::ROAM_PREFIX)
        };
        if !still_displayed {
            let (_, _rx) = self.connect_impl(config, ConnectSpec::Attach, false, false);
            return;
        }
        let cwd = self.resolve_cwd(&resume);
        // The freshness gate open_session applies: a session whose cached
        // transcript is up to date must not replay just because the socket
        // dropped (screen-off / background reconnects re-streamed the whole
        // convo every time). The store kept the live transcript across the
        // drop, so a fresh suppress keeps the UI exactly as it was; a stale one
        // replays (see below: the live rows are marked provisional and the
        // replay replaces them).
        let paint = self.plan_cache_paint(&resume, false);
        let suppress = paint.suppress;
        // The store holds the LIVE transcript across the drop, not a painted
        // cache, so there is no merge base: `replace_rich_for_merge` was never
        // called and the store's `merging` flag is off. A "merge" here would
        // therefore just APPEND the replay onto the live rows — and goose
        // re-assigns `tool_call_id`s from `call_0` on every replay, so appending
        // duplicates every replayed row and collides the ids (the
        // repeated/duplicated-bubble bug). Arm the provisional drop instead: the
        // first replayed row supersedes the live rows and the replay rebuilds
        // cleanly. Never arm a merge on reconnect — the paint it would need does
        // not exist.
        //
        // The replay that follows is bounded by what the store already holds
        // (`session::replay_held`: a tail never asks for less than the store's
        // rows), so a chat resumed with 60 rows on screen re-fetches ~140
        // messages instead of the whole conversation — and a store with the
        // whole conversation still gets the whole conversation back.
        if !suppress {
            self.store_for(&resume).mark_provisional();
        }
        let (_, _rx) = self.connect_impl(
            config,
            ConnectSpec::Resume {
                session_id: resume,
                cwd,
            },
            suppress,
            false,
        );
    }

    // -- hooks ----------------------------------------------------------------

    pub(crate) fn status_hook(&self, gen: u64) -> Arc<dyn Fn(ConnectionStatus) + Send + Sync> {
        let core = self.clone();
        Arc::new(move |status| core.on_conn_status_from(Some(gen), status))
    }

    pub(crate) fn config_hook(&self, gen: u64) -> Arc<dyn Fn(Vec<ConfigOption>) + Send + Sync> {
        let core = self.clone();
        Arc::new(move |options| {
            // Config belongs to the wire that answered the initialize handshake;
            // a late reply from a dead wire must not overwrite the new one's.
            let current = core.inner.state.lock().conn_gen == gen;
            if current {
                core.on_config_reply(options);
            }
        })
    }

    pub(crate) fn ended_hook(&self, gen: u64) -> Arc<dyn Fn(Result<(), String>) + Send + Sync> {
        let core = self.clone();
        Arc::new(move |result| core.on_connection_ended(gen, result))
    }

    pub(crate) fn emit_status(&self, status: ConnectionStatus) {
        self.inner.state.lock().status = status.clone();
        self.inner.listener.on_status(status);
    }
}
