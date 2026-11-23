// SPDX-License-Identifier: AGPL-3.0-or-later

//! The peer supervisor: dial, pin check, SDK handshake, command loop,
//! reconnect posture and teardown. Budgets and generation guards are verbatim.

use super::*;

impl RoamPeer {
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
    pub(crate) async fn attempt(
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

    /// The browse-mode handshake + command loop, run inside `connect_with`'s
    /// foreground: initialize → `session/list` → resume the session that was
    /// open when the link last dropped, if any. Then serve commands until
    /// `Close`. Handshake failures log and return `Err` WITHOUT surfacing an
    /// error status — during supervised reconnect the supervisor owns the
    /// status line ("reconnecting: …"); only a final give-up or an explicit
    /// close surfaces a terminal status.
    pub(crate) async fn handshake_and_serve(
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

    /// The link ended. `Ok` = clean (close command, or the peer was torn
    /// down). `Err` = the transport dropped: the supervisor in
    /// [`Self::connect`] decides whether to re-dial, so this records the
    /// teardown (transcript save, conn drop, resume owed, dead run cleared)
    /// and stays SILENT on a drop — the supervisor owns the status line from
    /// here ("connecting: reconnecting…" or the final give-up).
    pub(crate) fn connection_ended(&self, result: Result<(), AcpError>) {
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
    pub(crate) fn fail(&self, message: String) {
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
    pub(crate) fn command_failed(&self, message: String) {
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
pub(crate) async fn list_all_peer_sessions(
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
pub(crate) async fn peer_session_modes(
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

