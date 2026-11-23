// SPDX-License-Identifier: AGPL-3.0-or-later

//! The main-wire `initialize` → new/resume handshake and live session opens.
//!
//! Replay gates, open-failure handling and open-success emission are unchanged;
//! only their home file moved.

use super::*;

impl Conn {
    pub(crate) async fn handshake(&self, cx: ConnectionTo<Agent>) -> Result<(), AcpError> {
        if self.is_closing() {
            return Err(Self::cancelled_error());
        }
        *self.inner.connection.lock() = Some(cx.clone());
        self.set_status(ConnectionStatus::Connecting);

        // initialize: protocolVersion 1 + the desktop's exact client caps
        // (goosed hard-fails parameterized session/new without
        // recipeParameterRequests). `rpc_async` applies the shared 30s control
        // timeout; without it a server that answers pings but not initialize
        // left `connect()` parked for the full 15s ready bound with no reason.
        let _init: Value = self
            .rpc_async("initialize", initialize_params())
            .await
            .map_err(|error| {
                AcpError::internal_error().data(format!("initialize failed: {error}"))
            })?;
        if self.is_closing() {
            return Err(Self::cancelled_error());
        }

        let spec = self.inner.spec.lock().take();
        // Labelled so an advisory (non-timeout) load failure can skip the
        // rest of the spec handling and still reach `on_ready` + the idle
        // loop: the wire is usable, only the requested chat failed to load.
        'handshake: {
        match spec {
            Some(ConnectSpec::Resume { session_id, cwd }) => {
                let replay_key =
                    crate::session::display_id(self.inner.label.as_deref(), &session_id);
                // A merge fetched the overlap and the tail, so the store is
                // current up to the server's probe. A full replay (no cache)
                // may still lag it, and a fresh cache is already current.
                let persist_stamp = !self.inner.suppress_replay.load(Ordering::SeqCst)
                    && self.inner.merge_replay.load(Ordering::SeqCst);
                // Set the open-session id BEFORE the load request: the server
                // may stream replay chunks ahead of the reply (the spine's own
                // gateway test does), and the session-membership gate must not
                // drop them. start_new_session overwrites this if load fails.
                if self.is_closing() {
                    return Err(Self::cancelled_error());
                }
                *self.inner.session_id.lock() = Some(session_id.clone());
                let store = self.store_for(&session_id);
                let suppress_replay = self.inner.suppress_replay.load(Ordering::SeqCst);
                let merge_replay = self.inner.merge_replay.load(Ordering::SeqCst);
                // The replay itself (silent ingest → merge → one repaint) is the
                // shared lifecycle, so this wire cannot drift from the peer's.
                // Race it against shutdown: dropping the future lets the
                // replay guard release its hold, and a cancelled wire must
                // never reach bind_session or emit_config below.
                let endpoint: &dyn crate::session::ChatEndpoint = self;
                let replay = self
                    .race_shutdown(crate::session::replay_session(
                        endpoint,
                        self.inner.host.as_ref(),
                        &store,
                        &replay_key,
                        &cwd,
                        suppress_replay,
                        merge_replay,
                    ))
                    .await;
                let reply = match replay {
                    Ok(Ok(reply)) => reply,
                    Ok(Err(error)) => {
                        // A stale/archived session cannot be resumed. Do NOT
                        // fall back to a fresh session: the client asked for
                        // THIS one, and a silent `session/new` is what made a
                        // tapped thread open as a blank chat (grouse#19). Keep
                        // the painted cache (the best copy of the chat we have —
                        // it stays provisional, so it can never suppress the
                        // replay of a later successful load) and report; the
                        // client decides to retry or to start a new chat.
                        *self.inner.session_id.lock() = None;
                        self.report_open_failed(
                            session_id,
                            format!("session/load failed: {error}"),
                        );
                        // A load that never answered is NOT a Ready handshake:
                        // surface the failure on the status channel and unblock
                        // `connect()` with Err through `on_connection_ended`.
                        // Other load failures stay advisory (the wire is fine).
                        if handshake_timed_out(&error) {
                            return Err(AcpError::internal_error().data(format!(
                                "session/load failed: {error}"
                            )));
                        }
                        break 'handshake;
                    }
                    Err(cancelled) => return Err(cancelled),
                };
                if self.is_closing() {
                    return Err(Self::cancelled_error());
                }
                self.bind_session(&session_id);
                self.emit_config(&reply);
                if persist_stamp {
                    // The merge fetched the overlap and tail; stamp the
                    // result with the server's current updatedAt.
                    self.inner.host.stamp_probe(&replay_key, true);
                }
            }
            Some(ConnectSpec::New {
                recipe_id,
                project_id,
                mode,
                display,
            }) => {
                self.race_shutdown(self.start_new_session(recipe_id, project_id, mode, display))
                    .await??;
            }
            // Handshake only: the caller is showing something this wire does not
            // own (a peer's chat) or nothing at all, so there is no session to
            // bind — and binding one would repaint the client's transcript.
            // `session/list` and every connection-level call work without a
            // session; the first `open_session` binds.
            Some(ConnectSpec::Attach) => {}
            None => {
                self.race_shutdown(self.start_new_session(None, None, None, true))
                    .await??;
            }
        }
        }

        if self.is_closing() {
            return Err(Self::cancelled_error());
        }
        self.on_ready();

        // Keep the connection alive until Core disconnects or the transport
        // ends (either way `connect_with` returns and `on_connection_ended`
        // fires). The shutdown race covers the whole idle span too.
        tokio::select! {
            _ = self.cancelled() => Err(Self::cancelled_error()),
            _ = cx.incoming_closed() => Ok(()),
        }
    }

    pub(crate) async fn start_new_session(
        &self,
        recipe_id: Option<String>,
        project_id: Option<String>,
        mode: Option<String>,
        display: bool,
    ) -> Result<(), AcpError> {
        let params = session_new_params(
            &self.inner.config,
            recipe_id.as_deref(),
            project_id.as_deref(),
            mode.as_deref(),
        );
        let reply: Value = self
            .rpc_async("session/new", params)
            .await
            .map_err(|error| {
                AcpError::internal_error().data(format!("session/new failed: {error}"))
            })?;
        if self.is_closing() {
            return Err(Self::cancelled_error());
        }
        let session_id = reply
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // A cold `connect()` may create a throwaway session while the UI still
        // shows the last chat's cached paint; keep that paint. With nothing
        // painted (a first launch), or for an explicit new chat, show it.
        if display || self.inner.hub.displayed_key().is_none() {
            self.bind_new_session(&session_id);
        } else {
            *self.inner.session_id.lock() = Some(session_id);
        }
        self.emit_config(&reply);
        Ok(())
    }

    /// Live-wire `session/new` without tearing the connection down. Used by
    /// `Core::new_session` when a Ready connection with the same host/port/key
    /// already exists — avoids the `old.shutdown()` race that killed in-flight
    /// `sources/create` during project creation. Returns the new session id.
    pub(crate) async fn live_new_session_async(
        &self,
        recipe_id: Option<String>,
        project_id: Option<String>,
        mode: Option<String>,
    ) -> Result<String, AcpError> {
        if self.is_closing() {
            return Err(Self::cancelled_error());
        }
        let params = session_new_params(
            &self.inner.config,
            recipe_id.as_deref(),
            project_id.as_deref(),
            mode.as_deref(),
        );
        let reply: Value = self.rpc_async("session/new", params).await?;
        let session_id = reply
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        self.bind_new_session(&session_id);
        self.emit_config(&reply);
        Ok(session_id)
    }

    /// `session/new` that does NOT adopt the reply.
    ///
    /// `live_new_session_async` binds the new session to the connection, which is
    /// right for a chat the user asked for and wrong for anything created in the
    /// background: a thread's conversation is minted while the reader may be
    /// mid-sentence in another chat, and binding it would silently move them.
    pub(crate) async fn new_session_detached(
        &self,
        project_id: Option<&str>,
    ) -> Result<String, AcpError> {
        if self.is_closing() {
            return Err(Self::cancelled_error());
        }
        let params = session_new_params(&self.inner.config, None, project_id, None);
        let reply: Value = self.rpc_async("session/new", params).await?;
        Ok(reply
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// Live-wire `session/load` — same wire-reuse rationale as
    /// `live_new_session_async`. The blocking twin that used to sit here had no
    /// caller once session handling became async end to end.
    /// `bind_only` is the session-with-a-live-turn load: bind it on the wire and
    /// discard the replay, verified or not (a verification that downgraded this
    /// to a merge would anchor on the live rows and truncate the reply being
    /// streamed — see `session::PaintPlan::bind_only`).
    pub(crate) async fn live_load_session_async(
        &self,
        session_id: String,
        cwd: String,
        suppress_replay: bool,
        merge_replay: bool,
        bind_only: bool,
    ) -> Result<(), AcpError> {
        if self.is_closing() {
            return Err(Self::cancelled_error());
        }
        // NOTE: the gate/session id is ALREADY this session (`prebind_session`,
        // called by the open before the cwd round trip), so a failure cannot be
        // "restored" to the previous chat from here — see the Err arm below for
        // what a failed load leaves behind.
        self.inner.session_id.lock().replace(session_id.clone());
        let store = self.store_for(&session_id);
        let replay_key = crate::session::display_id(self.inner.label.as_deref(), &session_id);
        // VERIFY a suppression before arming the gates: a fresh-cache claim is
        // built from the baseline this core last saw listed, and another client's
        // writes never move it. Downgraded to a bounded merge here (the one place
        // before the gates are set) so the replayed rows are kept rather than
        // dropped — see `session::verified_ask`.
        let endpoint: &dyn crate::session::ChatEndpoint = self;
        let ask = if bind_only {
            crate::session::ReplayAsk::bind_only()
        } else {
            crate::session::verified_ask(
                endpoint,
                self.inner.host.as_ref(),
                &replay_key,
                crate::session::ReplayAsk {
                    suppress: suppress_replay,
                    merge: merge_replay,
                    tail: crate::session::first_tail(suppress_replay, merge_replay),
                },
            )
            .await
        };
        let (suppress_replay, merge_replay) = (ask.suppress, ask.merge);
        self.inner
            .suppress_replay
            .store(suppress_replay, Ordering::SeqCst);
        self.inner
            .merge_replay
            .store(merge_replay, Ordering::SeqCst);
        // A merge fetched the tail, so its result may be stamped with a newer
        // probe; a full replay (no cache) may still lag it, and a fresh cache
        // is already current.
        let persist_stamp = merge_replay && !suppress_replay;
        // One shared replay lifecycle (see `session::replay_session`): hold the
        // rows, load, close any merge, repaint once — the peer path uses the
        // same function, which is what keeps the two from drifting apart.
        let reply = crate::session::replay_session(
            endpoint,
            self.inner.host.as_ref(),
            &store,
            &replay_key,
            &cwd,
            suppress_replay,
            merge_replay,
        )
        .await;
        // Disarm the replay gates for the NEXT load: a fresh-cache open arms
        // `suppress_replay`, and a later full rebuild (resync) that did not reset
        // it would have every replayed row dropped — an empty transcript.
        endpoint.reset_replay_gates();
        let reply = reply.inspect_err(|_| {
            // The load never happened, so the connection must NOT keep claiming
            // this session (grouse#19): nothing is bound, and the gate names
            // nothing, until a retry or a new chat binds one.
            *self.inner.session_id.lock() = None;
        })?;
        self.bind_session(&session_id);
        self.emit_config(&reply);
        if persist_stamp {
            self.inner.host.stamp_probe(&replay_key, true);
        }
        Ok(())
    }

}
/// The `session/new` params. `_meta.client` present ⇒ `SessionType::User`, so
/// Desktop/CLI can see these chats. `recipeId` starts a recipe session;
/// `projectId` files the chat under a project *at creation* — stock goose
/// persists it to the `project_id` column, which is the create-time bridge off
/// the doomed `session/project/update` mutation (see grouse#5).
pub(crate) fn session_new_params(
    config: &ServerConfig,
    recipe_id: Option<&str>,
    project_id: Option<&str>,
    mode: Option<&str>,
) -> Value {
    crate::session::session_new_params(
        &config.client_id,
        &config.cwd,
        recipe_id,
        project_id,
        mode,
    )
}

/// Did a handshake RPC fail because the host never answered (as opposed to
/// answering with an error)? Only a timeout gets a terminal status and a
/// `ready` error; a rejected `session/load` stays advisory (grouse#19).
pub(crate) fn handshake_timed_out(error: &AcpError) -> bool {
    let text = error.to_string().to_lowercase();
    text.contains("timed out") || text.contains("gave no answer")
}

/// The `initialize` params: protocolVersion 1 + the client capabilities the
/// goosed server needs (fs, elicitation form, and the goose `_meta` gate for
/// custom notifications + parameterized recipes + MCP-App hosting.
///
/// `mcpHostCapabilities.extensions["io.modelcontextprotocol/ui"]` is what makes
/// the server's `host_supports_mcp_apps()` gate true over ACP; without it goose
/// never hydrates a tool's `_meta.ui.resourceUri` into the trusted
/// `_meta.goose.mcpApp` on the tool_call update, and every MCP App (the
/// autovisualiser charts included) silently degrades to a plain tool chip.
pub(crate) fn initialize_params() -> Value {
    crate::session::initialize_params()
}

/// The `session/list` params: list user + ACP sessions (desktop listSessions).
pub(crate) fn session_list_params(cursor: Option<&str>, tags: &[(String, String)]) -> Value {
    crate::session::session_list_params(cursor, tags)
}

