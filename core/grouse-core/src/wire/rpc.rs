// SPDX-License-Identifier: AGPL-3.0-or-later

//! The wire send/receive seams and the SDK client wiring. Everything here
//! dispatches into the re-exported `Conn` methods; no protocol behavior changed.

use super::*;

impl Conn {
    /// Synchronous request/reply. Safe from any thread: on a UI thread it
    /// parks on the core runtime until the response is dispatched; inside the
    /// runtime it uses `block_in_place`. Async callers use [`Conn::rpc_async`].
    /// Both go through the shared `session::rpc_timeout` policy — a hung
    /// host may pin an intent, never the calling thread.
    pub fn rpc(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        let conn = self.clone();
        let method = method.to_string();
        let fut = async move { conn.rpc_async(&method, params).await };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => tokio::task::block_in_place(move || handle.block_on(fut)),
            Err(_) => self.inner.runtime.block_on(fut),
        }
    }

    pub(crate) async fn rpc_async(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        // `session::request` calls back through `ChatEndpoint::request`, which
        // is the RAW send — no recursion, and exactly one timeout per RPC.
        crate::session::request(self, method, params).await
    }

    /// The wire send itself. Only [`ChatEndpoint::request`] and the bounded
    /// [`Conn::rpc_async`] call this; everything else gets the timeout policy.
    pub(crate) async fn raw_rpc_async(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        if self.is_closing() {
            return Err(Self::cancelled_error());
        }
        let conn = self
            .inner
            .connection
            .lock()
            .clone()
            .ok_or_else(|| AcpError::internal_error().data("not connected"))?;
        let msg = UntypedMessage::new(method, params)?;
        conn.send_request(msg).block_task().await
    }

    /// Fire-and-forget notification (`session/cancel`). Never waits for a
    /// reply (the server answers none).
    pub fn notify(&self, method: &str, params: Value) -> Result<(), AcpError> {
        let conn = self
            .inner
            .connection
            .lock()
            .clone()
            .ok_or_else(|| AcpError::internal_error().data("not connected"))?;
        let msg = UntypedMessage::new(method, params)?;
        conn.send_notification(msg)
    }

    pub(crate) fn dispatch_session_notification(&self, notif: SessionNotification) {
        // The shared pipeline intercepts the hidden summarizer session.
        crate::session::dispatch(self.inner.host.as_ref(), notif);
    }

}
impl RpcConn for Conn {
    fn rpc(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        Conn::rpc(self, method, params)
    }

    fn active_session_id(&self) -> Option<String> {
        Conn::active_session_id(self)
    }

    fn prebind_session(&self, session_id: &str) {
        Conn::prebind_session(self, session_id)
    }
}

impl crate::session::ChatEndpoint for Conn {
    fn request(
        &self,
        method: &str,
        params: Value,
    ) -> futures::future::BoxFuture<'static, Result<Value, AcpError>> {
        let conn = self.clone();
        let method = method.to_string();
        Box::pin(async move {
            // The policy lives in `session::request` around this seam; the raw
            // send also races shutdown so a superseded wire cannot pin an intent.
            conn.race_shutdown(conn.raw_rpc_async(&method, params))
                .await
                .and_then(|reply| reply)
        })
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), AcpError> {
        Conn::notify(self, method, params)
    }

    fn wire_id(&self, display: &str) -> String {
        display.to_string()
    }

    fn accepts_prompt(&self) -> bool {
        self.is_ready() && self.active_session_id().is_some()
    }

    fn label(&self) -> Option<String> {
        None
    }

    fn reset_replay_gates(&self) {
        self.inner.suppress_replay.store(false, Ordering::SeqCst);
        self.inner.merge_replay.store(false, Ordering::SeqCst);
    }
}

/// Wire the SDK `Client` (notification + server-request handlers) to the
/// transport and run the connection until it ends. The handlers dispatch into
/// `conn`; the foreground runs the handshake and then idles until shutdown or
/// transport EOF. Returns when the connection closes.
pub(crate) async fn run_connection(
    conn: Arc<Conn>,
    transport: WsTransport,
) -> Result<(), AcpError> {
    install_crypto_provider();
    let notif_conn = conn.clone();
    let permission_conn = conn.clone();
    let request_conn = conn.clone();
    let handlers = crate::session::ClientHandlers {
        notification: Arc::new(move |notif| notif_conn.dispatch_session_notification(notif)),
        custom_notification: Arc::new(notify_listeners),
        permission: Arc::new(move |req, responder| {
            permission_conn.on_permission_request(req, responder)
        }),
        request: Arc::new(move |method, params, responder| {
            request_conn.on_server_request(method, params, responder)
        }),
    };
    let foreground = move |cx: ConnectionTo<Agent>| -> futures::future::BoxFuture<'static, Result<(), AcpError>> {
        let conn = conn.clone();
        Box::pin(async move { conn.handshake(cx).await })
    };
    crate::session::connect_client(transport, handlers, foreground).await
}

/// The transport's TLS uses rustls' `ring` provider (Cargo.toml
/// pins it); `async-tungstenite`'s tokio-rustls feature re-enables the
/// `aws-lc-rs` default, so rustls cannot auto-select. Install `ring`
/// explicitly (once) before any connection builds a `ClientConfig`.
pub(crate) fn install_crypto_provider() {
    use std::sync::OnceLock;
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

