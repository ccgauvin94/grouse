// SPDX-License-Identifier: AGPL-3.0-or-later

//! Lifecycle status, shutdown races, and session binding for [`Conn`].
//!
//! The `closing` guard here is what keeps a superseded handshake from emitting
//! status or stealing the displayed chat; moved verbatim from `spine.rs`,
//! where the methods remain reachable at their existing paths.

use super::*;

impl Conn {
    pub(crate) fn is_ready(&self) -> bool {
        matches!(*self.inner.status.lock(), ConnectionStatus::Ready)
    }

    pub(crate) fn config_matches(&self, other: &ServerConfig) -> bool {
        let c = &self.inner.config;
        c.host == other.host
            && c.port == other.port
            && c.secret_key == other.secret_key
            && c.use_tls == other.use_tls
            && c.accept_invalid_certs == other.accept_invalid_certs
            && c.ca_cert_pem == other.ca_cert_pem
    }

    /// Signal an explicit disconnect — or a newer connect replacing this wire —
    /// marking the end deliberate: the handshake's idle select returns and the
    /// SDK shuts the connection down gracefully (Close frame included), and
    /// `on_connection_ended` surfaces no failure for it.
    pub(crate) fn shutdown(&self) {
        self.inner.closing.store(true, Ordering::SeqCst);
        // A `connect()` still parked on this wire must wake NOW, not after the
        // 15s wait_ready bound — and never with a success for a dead wire.
        if let Some(tx) = self.inner.ready.lock().take() {
            let _ = tx.send(Err("connection cancelled".to_string()));
        }
        self.drop_permission_owners();
        self.inner.shutdown.notify_waiters();
    }

    pub(crate) fn is_closing(&self) -> bool {
        self.inner.closing.load(Ordering::SeqCst)
    }

    /// Wake when this wire is deliberately closed. The first check matters as
    /// much as the notification: `shutdown()` sets `closing` BEFORE waking,
    /// so a waiter registered after the wake still sees the flag.
    pub(crate) async fn cancelled(&self) {
        loop {
            if self.is_closing() {
                return;
            }
            self.inner.shutdown.notified().await;
        }
    }

    pub(crate) fn cancelled_error() -> AcpError {
        AcpError::internal_error().data("connection cancelled by shutdown")
    }

    /// Race one await against shutdown and re-check the flag after it returns.
    /// A superseded handshake must not get as far as binding a session,
    /// emitting config, or signalling Ready.
    pub(crate) async fn race_shutdown<T, F>(&self, fut: F) -> Result<T, AcpError>
    where
        F: Future<Output = T>,
    {
        if self.is_closing() {
            return Err(Self::cancelled_error());
        }
        tokio::select! {
            _ = self.cancelled() => Err(Self::cancelled_error()),
            out = fut => {
                if self.is_closing() {
                    Err(Self::cancelled_error())
                } else {
                    Ok(out)
                }
            }
        }
    }

    /// Release every global permission owner registered by this connection.
    pub(crate) fn drop_permission_owners(&self) {
        let ids: Vec<String> = self.inner.permission_ids.lock().iter().cloned().collect();
        for id in ids {
            unregister_permission_answer_owned(&id, "main");
        }
        self.inner.permission_ids.lock().clear();
        self.inner.permission.lock().clear();
    }

    /// Current connection status snapshot.
    pub fn status(&self) -> ConnectionStatus {
        self.inner.status.lock().clone()
    }

    /// The session bound by `session/new` / `session/load`, if any.
    pub fn active_session_id(&self) -> Option<String> {
        self.inner.session_id.lock().clone()
    }

    /// Bind the chat-event gate to `session_id` NOW rather than when the load
    /// that confirms it lands.
    ///
    /// `session_id` is what the dispatch gate compares against: a chat event for
    /// any other session is treated as a backgrounded turn and dropped. The
    /// handshake assigns it on the runtime thread, but only *after*
    /// `Core::open_session` has resolved the session's cwd (cache → probe →
    /// `session/list`, i.e. a round trip) and spawned the connect. Meanwhile the
    /// app clears its transcript synchronously the moment it asks to switch. In
    /// that gap the gate still names the PREVIOUS chat, so that chat's in-flight
    /// chunks keep passing the gate and paint into the conversation now on
    /// screen. Pre-binding collapses a round trip of exposure to none: the gate
    /// moves in the same call the UI switches in.
    ///
    /// Safe against the load failing: a failed load leaves this id unset again
    /// (`live_load_session_async` / the handshake's Resume arm), so the gate
    /// names no session rather than the one that never loaded — and the failure
    /// is reported to the client instead of a substitute session being bound
    /// (grouse#19).
    pub fn prebind_session(&self, session_id: &str) {
        if self.is_closing() {
            return;
        }
        *self.inner.session_id.lock() = Some(session_id.to_string());
    }

    pub(crate) fn set_suppress_replay(&self, suppress: bool) {
        self.inner.suppress_replay.store(suppress, Ordering::SeqCst);
    }

    pub(crate) fn set_merge_replay(&self, merge: bool) {
        self.inner.merge_replay.store(merge, Ordering::SeqCst);
    }

    /// Bind a session without touching its store (a resume paints first).
    ///
    /// Every successful bind on this wire ends here: the handshake's Resume
    /// arm (including reconnect resume), `start_new_session` (via
    /// `bind_new_session`) and the live `session/load` reuse path all call it.
    /// That makes this the one seam for the optional bind hook. The shipped
    /// core does NOT announce opens from here: it emits
    /// `CoreListener::on_session_opened` centrally on Ready (which covers the
    /// handshake, including reconnect resume) and at the two live-reuse call
    /// sites, with bind-only loads deliberately excluded. `prebind_session`
    /// moves the event gate ahead of the load but is NOT a bind.
    pub(crate) fn bind_session_inner(&self, session_id: &str) -> Option<String> {
        if self.is_closing() {
            return None;
        }
        *self.inner.session_id.lock() = Some(session_id.to_string());
        let key = crate::session::display_id(self.inner.label.as_deref(), session_id);
        self.inner.hub.display(&key);
        Some(key)
    }

    pub(crate) fn bind_session(&self, session_id: &str) {
        if let Some(key) = self.bind_session_inner(session_id) {
            self.report_session_bound(&key);
        }
    }

    /// Notify a host that has opted into bind-driven emission. The shipped
    /// core deliberately installs no hook: it announces the same successful
    /// bind centrally on Ready (`Core::maybe_announce_open`) or at the
    /// live-reuse call site. Emitting here as well would double the event.
    pub(crate) fn report_session_bound(&self, display_id: &str) {
        if self.is_closing() {
            return;
        }
        if let Some(hook) = self.inner.on_session_bound.lock().clone() {
            hook(display_id.to_string());
        }
    }

    /// Bind a NEW session and clear the screen: there is no transcript yet, and
    /// the previous chat's rows must not linger behind the empty one. Any
    /// bind hook fires AFTER the clear, so a host reading the transcript in
    /// its callback sees the new chat's empty rows, not the old chat's.
    pub(crate) fn bind_new_session(&self, session_id: &str) {
        if let Some(key) = self.bind_session_inner(session_id) {
            self.inner.hub.store(&key).clear();
            self.report_session_bound(&key);
        }
    }

    /// The store for a session owned by this connection, in this connection's
    /// display namespace.
    pub(crate) fn store_for(&self, session_id: &str) -> Arc<TranscriptStore> {
        self.inner.hub.store(&crate::session::display_id(
            self.inner.label.as_deref(),
            session_id,
        ))
    }

    pub(crate) fn set_on_status(&self, f: Arc<dyn Fn(ConnectionStatus) + Send + Sync>) {
        *self.inner.on_status.lock() = Some(f);
    }

    pub(crate) fn set_on_open_failed(&self, f: Arc<dyn Fn(String, String) + Send + Sync>) {
        *self.inner.on_open_failed.lock() = Some(f);
    }

    /// Register a direct bind signal (DISPLAY id). The shipped core emits
    /// `on_session_opened` centrally and therefore installs NO hook here; a
    /// host that switches to bind-driven emission must register this AND
    /// remove the central `Core::maybe_announce_open` / live-reuse calls, or
    /// the UI gets the event twice.
    #[allow(dead_code)] // no hook installed by the shipped core
    pub(crate) fn set_on_session_bound(&self, f: Arc<dyn Fn(String) + Send + Sync>) {
        *self.inner.on_session_bound.lock() = Some(f);
    }

    /// Report an open/resume failure to Core. The connection binds NO session
    /// for it — a stale id must not become a fresh chat behind the client's
    /// back (grouse#19).
    pub(crate) fn report_open_failed(&self, session_id: impl Into<String>, message: impl Into<String>) {
        if self.is_closing() {
            return;
        }
        let hook = self.inner.on_open_failed.lock().clone();
        if let Some(hook) = hook {
            hook(session_id.into(), message.into());
        }
    }

    pub(crate) fn set_on_config(&self, f: Arc<dyn Fn(Vec<ConfigOption>) + Send + Sync>) {
        *self.inner.on_config.lock() = Some(f);
    }

    pub(crate) fn set_on_ended(&self, f: Arc<dyn Fn(Result<(), String>) + Send + Sync>) {
        *self.inner.on_ended.lock() = Some(f);
    }

    /// Signal Core that the connection ended (drop, explicit disconnect, or
    /// handshake failure). Deliberate closes surface no status — see below.
    pub(crate) fn on_connection_ended(&self, result: Result<(), AcpError>) {
        self.drop_permission_owners();
        // A deliberate `shutdown()` (superseded by a newer connect, or an
        // explicit disconnect) is not a failure: emit no terminal Error. The
        // replacement handshake owns the status story from here (Connecting →
        // Ready), or `disconnect()` already emitted Disconnected. The core's
        // ended hook below stays the sole reconnect/inertness decision point.
        if self.inner.closing.load(Ordering::SeqCst) {
            if let Some(hook) = self.inner.on_ended.lock().clone() {
                hook(result.map_err(|error| error.to_string()));
            }
            return;
        }
        let message = match &result {
            Ok(()) => "connection closed".to_string(),
            Err(error) => error.to_string(),
        };
        // Reflect the terminal status first so a UI thread parked in the
        // bounded `connect()` observes the failure the moment it wakes.
        self.set_status(ConnectionStatus::Error {
            message: message.clone(),
        });
        // A handshake that never reached ready must unblock the bounded
        // `connect()` wait with the failure instead of letting it time out.
        if !matches!(*self.inner.status.lock(), ConnectionStatus::Ready) {
            if let Some(tx) = self.inner.ready.lock().take() {
                let _ = tx.send(Err(message));
            }
        }
        // The reconnect decision lives in the core's ended hook.
        if let Some(hook) = self.inner.on_ended.lock().clone() {
            hook(result.map_err(|error| error.to_string()));
        }
    }

    pub(crate) fn set_status(&self, status: ConnectionStatus) {
        // A superseded wire has no status story: it must not overwrite the
        // replacement's Connecting/Ready or flash a stale Error.
        if self.is_closing() {
            return;
        }
        *self.inner.status.lock() = status.clone();
        if let Some(hook) = self.inner.on_status.lock().clone() {
            hook(status);
        }
    }

    pub(crate) fn signal_ready(&self, result: Result<(), String>) {
        if self.is_closing() && result.is_ok() {
            return;
        }
        if let Some(tx) = self.inner.ready.lock().take() {
            let _ = tx.send(result);
        }
    }

    pub(crate) fn on_ready(&self) {
        // Shutdown can land between the last replay await and this call.
        if self.is_closing() {
            return;
        }
        self.inner.suppress_replay.store(false, Ordering::SeqCst);
        self.inner.merge_replay.store(false, Ordering::SeqCst);
        // Still armed here => the replay produced nothing, so the server's copy
        // of this session is empty and the painted rows are gone from it.
        // Replay chunks precede the load reply, so nothing is racing this.
        if let Some(session_id) = self.inner.session_id.lock().clone() {
            self.store_for(&session_id).supersede_provisional();
            let key = crate::session::display_id(self.inner.label.as_deref(), &session_id);
            self.inner.host.end_replay(&key);
        }
        self.set_status(ConnectionStatus::Ready);
        if self.is_closing() {
            return;
        }
        self.signal_ready(Ok(()));
    }

    pub(crate) fn emit_config(&self, reply: &Value) {
        if self.is_closing() {
            return;
        }
        let options = parse_config_options(reply);
        if let Some(hook) = self.inner.on_config.lock().clone() {
            hook(options);
        }
    }

}
/// The core's exponential reconnect backoff curve: 500ms·2^n capped at 15s.
/// Shared by the main connection (`Core::schedule_reconnect`) and the roam
/// peer supervisor (`roam::RoamPeer::connect`) so both retry on the same
/// rhythm; only their attempt budgets differ.
pub(crate) fn reconnect_delay_ms(attempt: u32) -> u64 {
    500u64 * (1u64 << attempt.min(5)).min(15000 / 500)
}

