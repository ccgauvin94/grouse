// SPDX-License-Identifier: AGPL-3.0-or-later

//! Custom WebSocket transport for the grouse core.
//!
//! The stock `agent-client-protocol-http::HttpClient` cannot express what the
//! goosed server needs: its `run_ws` performs `connect_async` with no headers
//! and the platform's default TLS roots. grouse requires:
//!
//!   * an `X-Secret-Key` header on the WebSocket upgrade request, and
//!   * hostname+chain TLS verification by default, with a documented
//!     trust-all opt-out for self-signed hosts.
//!
//! This module implements [`ConnectTo<Client>`] directly: it performs the
//! handshake with a rustls connector (WebPKI-verifying unless
//! `accept_invalid_certs` is set), then feeds the resulting byte stream into
//! the SDK's [`Channel`] exactly like `HttpClient::run_ws` does —
//! newline-independent, JSON-RPC text frames both ways.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use agent_client_protocol::{Agent, Channel, Client, ConnectTo, Error as AcpError, TransportFrame};
use async_tungstenite::tokio::connect_async_with_tls_connector_and_config;
use async_tungstenite::tungstenite::http::Request;
use async_tungstenite::tungstenite::Message as WsMessage;
use futures::future::{select, BoxFuture};
use futures::stream::StreamExt;
use futures::Stream;
use rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use tokio_rustls::TlsConnector;

/// A WebSocket transport for a goosed ACP server.
///
/// Constructed from a [`crate::ServerConfig`]; the state machine hands this to
/// `Client::builder().connect_with(transport, …)`.
pub struct WsTransport {
    endpoint: String,
    host: String,
    port: u16,
    use_tls: bool,
    secret_key: String,
    /// Accept any server certificate. When `false` (default) the connector
    /// verifies the chain and hostname against WebPKI roots plus
    /// `ca_cert_pem`.
    accept_invalid_certs: bool,
    /// PEM-encoded CA certificate(s) for the verifier's trust store.
    ca_cert_pem: Option<String>,
}

impl WsTransport {
    /// Build a transport from the parts of a `ServerConfig`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        host: &str,
        port: u16,
        secret_key: &str,
        use_tls: bool,
        accept_invalid_certs: bool,
        ca_cert_pem: Option<String>,
    ) -> Self {
        let scheme = if use_tls { "wss" } else { "ws" };
        Self {
            endpoint: format!("{scheme}://{host}:{port}/acp"),
            host: host.to_string(),
            port,
            use_tls,
            secret_key: secret_key.to_string(),
            accept_invalid_certs,
            ca_cert_pem,
        }
    }

    /// The upgrade request: the full WebSocket client handshake (tungstenite
    /// 0.29 requires the client to supply Host/Upgrade/Sec-WebSocket-Key —
    /// it no longer fills them in), plus the `X-Secret-Key` header.
    fn handshake_request(&self) -> Result<Request<()>, AcpError> {
        let key = async_tungstenite::tungstenite::handshake::client::generate_key();
        // Host must be authority-only, and the port is OMITTED when it is the
        // scheme default (RFC 7230 §5.4). Two real failures came from getting
        // this wrong against the public endpoint: a trailing path made Caddy
        // reply 400 to the upgrade, and an explicit `:443` made its site
        // matcher reply 403 — while `Host: host` (what OkHttp sends) gets 101.
        let authority = if (self.use_tls && self.port == 443) || (!self.use_tls && self.port == 80) {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        };
        Request::builder()
            .method("GET")
            .uri(self.endpoint.as_str())
            .header("Host", authority)
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", key)
            .header("X-Secret-Key", self.secret_key.as_str())
            .body(())
            .map_err(|e| AcpError::internal_error().data(format!("invalid WebSocket request: {e}")))
    }

    /// The TLS connector for this transport: trust-all when
    /// `accept_invalid_certs`, otherwise WebPKI verification (with any
    /// user-supplied CA) — hostname included.
    fn connector(&self) -> Result<TlsConnector, AcpError> {
        if self.accept_invalid_certs {
            return Ok(trust_all_connector());
        }
        verified_connector(self.ca_cert_pem.as_deref())
    }

    /// Connect and drive the WebSocket against the SDK channel.
    async fn run(self, channel: Channel) -> Result<(), AcpError> {
        let request = self.handshake_request()?;
        let connector = self.connector()?;
        let (ws_stream, _response) =
            connect_async_with_tls_connector_and_config(request, Some(connector), None).await.map_err(
                |e| AcpError::internal_error().data(format!("WebSocket connect failed: {e}")),
            )?;
        let (ws_tx, ws_rx) = ws_stream.split();
        drive_ws(ws_tx, ws_rx, channel, KeepAlive::default()).await
    }
}

impl ConnectTo<Client> for WsTransport {
    async fn connect_to(self, client: impl ConnectTo<Agent>) -> Result<(), AcpError> {
        let (channel, transport) = ConnectTo::<Client>::into_channel_and_future(self);
        let shutdown_tx = channel.tx.clone();
        match select(
            std::pin::pin!(client.connect_to(channel)),
            std::pin::pin!(transport),
        )
        .await
        {
            futures::future::Either::Left((result, transport)) => {
                result?;
                // Reject sends from escaped client handles while preserving
                // messages already accepted into the channel, then let the
                // physical transport finish those messages.
                shutdown_tx.close_channel();
                transport.await
            }
            futures::future::Either::Right((result, _)) => result,
        }
    }

    fn into_channel_and_future(self) -> (Channel, BoxFuture<'static, Result<(), AcpError>>) {
        let (caller, transport) = Channel::duplex();
        (caller, Box::pin(self.run(transport)))
    }
}

/// Minimal sink abstraction so `drive_ws` stays generic over the stream type.
trait WsSink {
    fn send(
        &mut self,
        message: WsMessage,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send;
}

impl<S> WsSink for async_tungstenite::WebSocketSender<S>
where
    S: futures::AsyncRead + futures::AsyncWrite + Unpin + Send,
{
    async fn send(&mut self, message: WsMessage) -> Result<(), String> {
        async_tungstenite::WebSocketSender::send(self, message)
            .await
            .map_err(|error| error.to_string())
    }
}

/// Drive the WebSocket: serialize outbound frames and push inbound frames into
/// the SDK channel. Mirrors `agent-client-protocol-http`'s `drive_ws`, plus an
/// idle keepalive (see [`KeepAlive`]).
///
/// All socket WRITES go through one sender task fed by a `WsFrame` queue, so
/// JSON-RPC frames, keepalive pings, and the close frame share a single FIFO
/// and one `&mut ws_tx`. All socket READS go through one reader task; the
/// reader is the activity clock (any inbound frame — text, pong, tungstenite's
/// auto-reply pongs included) rebases the idle timer.
async fn drive_ws<Tx, Rx, RxError>(
    mut ws_tx: Tx,
    mut ws_rx: Rx,
    channel: Channel,
    ka: KeepAlive,
) -> Result<(), AcpError>
where
    Tx: WsSink + Send + 'static,
    Rx: Stream<Item = Result<WsMessage, RxError>> + Unpin,
    RxError: std::fmt::Display,
{
    let Channel {
        rx: mut outgoing,
        tx: incoming,
    } = channel;

    // Activity clock in ms since `epoch`. The reader bumps it on EVERY inbound
    // frame; the pinger also bumps it when it sends a ping, which enforces a
    // full `idle_after` gap between consecutive pings. A deadline strictly
    // greater than the last bump means "silent for that long".
    let epoch = Instant::now();
    let last_seen = Arc::new(AtomicU64::new(0));
    // Ping-outstanding flag + deadline (ms since epoch). Set with the ping;
    // cleared by the reader the moment any frame answers it.
    let ping_deadline = Arc::new(AtomicU64::new(u64::MAX));

    // Single writer: drain the frame queue until every producer is gone, then
    // close. The JSON-RPC pump forwards `outgoing` into the same queue, so a
    // ping queued behind traffic can never interleave mid-frame.
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::unbounded_channel::<WsMessage>();
    let sender = tokio::spawn(async move {
        while let Some(msg) = frame_rx.recv().await {
            if let Err(error) = ws_tx.send(msg).await {
                return Err(AcpError::internal_error()
                    .data(format!("ws send: {error}")));
            }
        }
        drop(ws_tx.send(WsMessage::Close(None)).await);
        Ok::<(), AcpError>(())
    });

    // Pump JSON-RPC frames into the writer queue. Dropped when the channel or
    // the socket dies, which is what ends the sender's drain loop.
    let pump = {
        let frame_tx = frame_tx.clone();
        async move {
            while let Some(frame) = outgoing.next().await {
                let text = frame.to_json().map_err(|error| {
                    AcpError::internal_error().data(format!("serialize: {error}"))
                })?;
                if frame_tx.send(WsMessage::Text(text.into())).is_err() {
                    // Sender gone (socket send failed); its result surfaces.
                    break;
                }
            }
            Ok::<(), AcpError>(())
        }
    };

    // Bounded inbound hand-off (S-RC-3): cap the number of server frames held
    // before the reader pauses the socket read. When the SDK client is slow
    // this stops the reader from buffering inbound frames without bound and
    // lets TCP flow-control push genuine backpressure onto the server. The
    // SDK channel's own internal queue is unbounded by design and out of our
    // control; this bounds everything the reader accepts. Ordering is
    // preserved — the forwarder copies the bounded queue into the SDK channel
    // FIFO.
    const INBOUND_CAP: usize = 128;
    let (inbound_tx, mut inbound_rx) =
        tokio::sync::mpsc::channel::<TransportFrame>(INBOUND_CAP);

    let reader = {
        let last_seen = last_seen.clone();
        let ping_deadline = ping_deadline.clone();
        async move {
            let mut discard_incoming = false;
            loop {
                match ws_rx.next().await {
                    Some(Ok(msg)) => {
                        // Any inbound frame is proof of life: rebase the idle
                        // timer and retire an outstanding ping (a pong answers
                        // it; any other frame proves the path is alive too).
                        last_seen.store(elapsed_ms(epoch, Instant::now()), Ordering::Relaxed);
                        ping_deadline.store(u64::MAX, Ordering::Relaxed);
                        match msg {
                            WsMessage::Text(text) => {
                                if discard_incoming {
                                    continue;
                                }
                                let frame = TransportFrame::parse_json(text.as_str());
                                // Await capacity: a full buffer stalls the socket
                                // read, so TCP flow control pushes backpressure
                                // onto the server.
                                if inbound_tx.send(frame).await.is_err() {
                                    // Forwarder gone (client channel closed);
                                    // drain the socket until it ends so graceful
                                    // shutdown still works.
                                    discard_incoming = true;
                                }
                            }
                            WsMessage::Binary(_)
                            | WsMessage::Ping(_)
                            | WsMessage::Pong(_)
                            | WsMessage::Frame(_) => {}
                            WsMessage::Close(frame) => {
                                return Err(AcpError::internal_error()
                                    .data(format!("WebSocket closed by peer: {frame:?}")));
                            }
                        }
                    }
                    Some(Err(e)) => {
                        return Err(AcpError::internal_error().data(format!("ws recv: {e}")));
                    }
                    None => {
                        return Err(AcpError::internal_error().data("WebSocket stream ended"));
                    }
                }
            }
        }
    };

    // The keepalive state machine: one tick per `check`. Silent < idle_after →
    // idle; silent < ping_deadline-armed-then → dead.
    let pinger = {
        let last_seen = last_seen.clone();
        let ping_deadline = ping_deadline.clone();
        let frame_tx = frame_tx.clone();
        async move {
            let mut ticker = tokio::time::interval(ka.check);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let idle_ms = ka.idle_after.as_millis().min(u64::MAX as u128) as u64;
            let pong_ms = ka.pong_timeout.as_millis().min(u64::MAX as u128) as u64;
            loop {
                ticker.tick().await;
                let now = elapsed_ms(epoch, Instant::now());
                let silent = now.saturating_sub(last_seen.load(Ordering::Relaxed));
                if silent < idle_ms {
                    continue;
                }
                if ping_deadline.load(Ordering::Relaxed) == u64::MAX {
                    // Arm the probe: send a ping, rebase the idle clock (pings
                    // stay behind traffic, never on top of it), and give the
                    // peer a pong window.
                    if frame_tx.send(WsMessage::Ping(Vec::new().into())).is_err() {
                        return; // socket side is ending
                    }
                    last_seen.store(now, Ordering::Relaxed);
                    ping_deadline.store(now + pong_ms, Ordering::Relaxed);
                } else if now > ping_deadline.load(Ordering::Relaxed) {
                    // The probe went unanswered: the wire is silently reaped.
                    // Error out so the core's backoff reconnect fires NOW,
                    // while the app can still act on the status.
                    eprintln!(
                        "grouse-core: WebSocket keepalive: no reply to ping within \
                         {}ms — treating the connection as dropped",
                        ka.pong_timeout.as_millis()
                    );
                    return;
                }
            }
        }
    };

    let forwarder = async move {
        let mut forward = true;
        while let Some(frame) = inbound_rx.recv().await {
            if forward && incoming.unbounded_send(frame).is_err() {
                // Client channel closed: stop forwarding but keep draining
                // the bounded queue so the reader unblocks and finishes.
                forward = false;
            }
        }
        Ok::<(), AcpError>(())
    };

    let pinger = async move {
        pinger.await;
        Err(AcpError::internal_error()
            .data("keepalive: no reply from server (connection reaped)"))
    };

    // select! takes the futures by value and drops the branches that lose,
    // which also drops their `frame_tx` clones. After it, the queue's only
    // producer left is our own handle: dropping it lets the sender drain what
    // is queued (Close frame last) and finish — graceful on a clean pump
    // finish. On a dead wire the sender may be wedged on a socket that will
    // never accept writes again, so the reap is bounded: the real outcome is
    // already in `result`.
    let result = tokio::select! {
        result = pump => result,
        result = reader => result,
        _ = forwarder => Ok(()),
        result = pinger => result,
    };
    drop(frame_tx);
    let sender_result =
        match tokio::time::timeout(std::time::Duration::from_secs(1), sender).await {
            Ok(Ok(r)) => r,
            Ok(Err(join_err)) => Err(AcpError::internal_error()
                .data(format!("ws sender task: {join_err}"))),
            Err(_) => Ok(()),
        };
    sender_result.and(result)
}

/// A `ServerCertVerifier` that accepts every certificate. The trust-all path
/// for self-signed goosed hosts — only used when `accept_invalid_certs` is
/// set (the WebPKI verifier is the default).
#[derive(Debug)]
struct NoVerify;

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
        ]
    }
}

/// Build a TLS connector whose certificate verifier accepts anything.
fn trust_all_connector() -> TlsConnector {
    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify))
        .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
}

/// Build a TLS connector that verifies the server chain and hostname against
/// the WebPKI trust store, extended by any user-supplied CA. This is the
/// default — the goosed public host already serves a WebPKI-valid Let's
/// Encrypt certificate, so trust-all was a silent downgrade on every public
/// path (an on-path party could substitute a certificate, read the
/// `X-Secret-Key`, and impersonate the server).
fn verified_connector(ca_cert_pem: Option<&str>) -> Result<TlsConnector, AcpError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(pem) = ca_cert_pem {
        use rustls::pki_types::pem::PemObject;
        for cert in CertificateDer::pem_slice_iter(pem.as_bytes()).flatten() {
            roots.add(cert).map_err(|e| {
                AcpError::internal_error().data(format!("invalid CA certificate: {e}"))
            })?;
        }
    }
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

// ---------------------------------------------------------------------------
// Keepalive
// ---------------------------------------------------------------------------

/// Idle-time WebSocket keepalive policy. A goosed socket that sees no traffic
/// is reaped silently by NATs, Caddy-style proxies, and tailnet idle timeouts:
/// nothing errors, nothing is received, and the client only discovers the dead
/// wire when it next tries to read — in practice never, because the Android
/// process is frozen in the background. The transport therefore pings an idle
/// connection and treats a missing answer as a drop, so the core's backoff
/// reconnect fires while the app can still act on it.
///
/// "Smart" means: the ping is the *absence* of traffic's consequence, never an
/// extra frame while a turn streams (any inbound byte rebases the idle timer,
/// and a pong that answers a live ping also counts); the interval is a cheap
/// check tick, not the ping cadence.
#[derive(Clone, Copy, Debug)]
pub struct KeepAlive {
    /// How often the idle/deadline state machine runs.
    pub check: std::time::Duration,
    /// Silence this long before a ping goes out.
    pub idle_after: std::time::Duration,
    /// No inbound frame this long after a ping ⇒ the peer is gone.
    pub pong_timeout: std::time::Duration,
}

impl Default for KeepAlive {
    fn default() -> Self {
        Self {
            check: std::time::Duration::from_secs(5),
            idle_after: std::time::Duration::from_secs(30),
            pong_timeout: std::time::Duration::from_secs(15),
        }
    }
}

/// Milliseconds since `start`, saturating.
fn elapsed_ms(start: Instant, now: Instant) -> u64 {
    now.duration_since(start).as_millis().min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod keepalive_tests {
    use super::*;
    use futures::Stream;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::Duration;

    /// Records every frame `drive_ws` sends.
    struct RecordingSink {
        tx: tokio::sync::mpsc::UnboundedSender<WsMessage>,
    }

    impl WsSink for RecordingSink {
        async fn send(&mut self, message: WsMessage) -> Result<(), String> {
            let _ = self.tx.send(message);
            Ok(())
        }
    }

    /// An inbound stream that yields what its receiver pushes; when the
    /// sender is dropped without sending, it stays pending forever (a silent
    /// peer) rather than ending the socket.
    struct PushStream {
        rx: tokio::sync::mpsc::Receiver<Result<WsMessage, String>>,
    }

    impl Stream for PushStream {
        type Item = Result<WsMessage, String>;
        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            self.rx.poll_recv(cx)
        }
    }

    fn fast_keepalive() -> KeepAlive {
        KeepAlive {
            check: Duration::from_millis(10),
            idle_after: Duration::from_millis(30),
            pong_timeout: Duration::from_millis(80),
        }
    }

    // drive_ws owns ws_tx, so the test observes sends over a channel and
    // keeps the counterpart half of the SDK channel alive (an immediately
    // dropped `Channel` would close `outgoing` and end the writer before any
    // ping tick).
    #[allow(clippy::type_complexity)]
    fn harness(ka: KeepAlive) -> (
        tokio::runtime::Runtime,
        tokio::sync::mpsc::UnboundedReceiver<WsMessage>,
        tokio::sync::mpsc::Sender<Result<WsMessage, String>>,
        tokio::task::JoinHandle<Result<(), AcpError>>,
        Channel,
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (sent_tx, sent_rx) = tokio::sync::mpsc::unbounded_channel();
        let (in_tx, in_rx) = tokio::sync::mpsc::channel::<Result<WsMessage, String>>(8);
        let (transport_side, client_side) = Channel::duplex();
        let join = rt.spawn(async move {
            drive_ws(
                RecordingSink { tx: sent_tx },
                PushStream { rx: in_rx },
                transport_side,
                ka,
            )
            .await
        });
        (rt, sent_rx, in_tx, join, client_side)
    }

    #[test]
    fn idle_connection_gets_pinged() {
        let (rt, mut sent, _in, _join, _keep) = harness(fast_keepalive());
        rt.block_on(async {
            // No inbound traffic: a ping must appear without any outgoing
            // JSON-RPC frame.
            let msg = tokio::time::timeout(Duration::from_secs(2), sent.recv())
                .await
                .expect("a keepalive ping is sent while idle");
            assert!(matches!(msg, Some(WsMessage::Ping(_))), "expected Ping, got {msg:?}");
        });
    }

    #[test]
    fn streaming_traffic_suppresses_pings() {
        let (rt, mut sent, in_tx, _join, _keep) = harness(fast_keepalive());
        rt.block_on(async {
            // Steady inbound traffic (what a live turn looks like) keeps the
            // idle timer rebased: no pings while frames arrive.
            for _ in 0..6 {
                in_tx
                    .send(Ok(WsMessage::Text("{\"x\":1}".into())))
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
            // Nothing (JSON-RPC or ping) has been sent: the queue is empty.
            match sent.try_recv() {
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
                other => panic!("no frame may be sent while traffic flows, got {other:?}"),
            }
        });
    }

    #[test]
    fn unanswered_ping_kills_the_connection() {
        let (rt, _sent, _in, join, _keep) = harness(fast_keepalive());
        // The peer answers nothing: the pong deadline must surface as an
        // error (which the core turns into its backoff reconnect), not a hang.
        // (The timeout future must be constructed INSIDE block_on — the time
        // driver only exists on the runtime context.)
        let result = rt
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(2), join)
                    .await
                    .expect("the dead connection is detected")
                    .expect("no panic")
            });
        let rendered = format!("{}", result.unwrap_err());
        assert!(rendered.contains("keepalive"), "expected a keepalive error, got {rendered}");
    }

    #[test]
    fn pong_resets_the_deadline() {
        let (rt, mut sent, in_tx, join, _keep) = harness(fast_keepalive());
        rt.block_on(async {
            // Answer every ping with a pong; two pong windows later the
            // connection must still be alive.
            let mut answered = 0;
            while answered < 2 {
                match tokio::time::timeout(Duration::from_secs(2), sent.recv())
                    .await
                    .expect("ping")
                {
                    Some(WsMessage::Ping(_)) => {
                        in_tx.send(Ok(WsMessage::Pong(vec![].into()))).await.unwrap();
                        answered += 1;
                    }
                    other => panic!("expected Ping, got {other:?}"),
                }
            }
            assert!(!join.is_finished(), "answered pings must not kill the socket");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_header_is_authority_only() {
        // Regression: the Host header used to carry the path (`host:443/acp`),
        // which Caddy rejects with 400 Bad Request on the upgrade.
        let t = WsTransport::new("goose.gauvin.id", 443, "k", true, false, None);
        let req = t.handshake_request().unwrap();
        // Default wss port: the Host MUST omit it (Caddy's site matcher 403s
        // an explicit :443 on the upgrade; OkHttp omits it and gets 101).
        assert_eq!(req.headers().get("Host").unwrap(), "goose.gauvin.id");

        let t = WsTransport::new("192.168.1.5", 3284, "k", false, false, None);
        let req = t.handshake_request().unwrap();
        assert_eq!(req.headers().get("Host").unwrap(), "192.168.1.5:3284");

        let t = WsTransport::new("example.com", 80, "k", false, false, None);
        let req = t.handshake_request().unwrap();
        assert_eq!(req.headers().get("Host").unwrap(), "example.com");
    }
}
