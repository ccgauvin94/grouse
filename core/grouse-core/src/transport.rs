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

use std::sync::Arc;

use agent_client_protocol::{Agent, Channel, Client, ConnectTo, Error as AcpError, TransportFrame};
use async_tungstenite::tokio::connect_async_with_tls_connector_and_config;
use async_tungstenite::tungstenite::http::Request;
use async_tungstenite::tungstenite::Message as WsMessage;
use futures::future::{select, BoxFuture};
use futures::stream::StreamExt;
use futures::{pin_mut, Stream};
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
        drive_ws(ws_tx, ws_rx, channel).await
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
/// the SDK channel. Mirrors `agent-client-protocol-http`'s `drive_ws`.
async fn drive_ws<Tx, Rx, RxError>(
    mut ws_tx: Tx,
    mut ws_rx: Rx,
    channel: Channel,
) -> Result<(), AcpError>
where
    Tx: WsSink,
    Rx: Stream<Item = Result<WsMessage, RxError>> + Unpin,
    RxError: std::fmt::Display,
{
    let Channel {
        rx: mut outgoing,
        tx: incoming,
    } = channel;

    let writer = async move {
        while let Some(frame) = outgoing.next().await {
            let text = frame
                .to_json()
                .map_err(|error| AcpError::internal_error().data(format!("serialize: {error}")))?;
            ws_tx
                .send(WsMessage::Text(text.into()))
                .await
                .map_err(|error| AcpError::internal_error().data(format!("ws send: {error}")))?;
        }

        drop(ws_tx.send(WsMessage::Close(None)).await);
        Ok::<(), AcpError>(())
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

    let reader = async move {
        let mut discard_incoming = false;
        loop {
            match ws_rx.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    if discard_incoming {
                        continue;
                    }
                    let frame = TransportFrame::parse_json(text.as_str());
                    // Await capacity: a full buffer stalls the socket read, so
                    // TCP flow control pushes backpressure onto the server.
                    if inbound_tx.send(frame).await.is_err() {
                        // Forwarder gone (client channel closed); drain the
                        // socket until it ends so graceful shutdown still works.
                        discard_incoming = true;
                    }
                }
                Some(Ok(WsMessage::Binary(_))) => {}
                Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Frame(_))) => {}
                Some(Ok(WsMessage::Close(frame))) => {
                    return Err(AcpError::internal_error()
                        .data(format!("WebSocket closed by peer: {frame:?}")));
                }
                Some(Err(e)) => {
                    return Err(AcpError::internal_error().data(format!("ws recv: {e}")));
                }
                None => {
                    return Err(AcpError::internal_error().data("WebSocket stream ended"));
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

    pin_mut!(writer, reader, forwarder);
    // Run writer, reader and forwarder concurrently; finish when the socket
    // side completes (mirrors the pre-existing select(writer, reader)).
    tokio::select! {
        result = writer => result,
        result = reader => result,
        _ = forwarder => Ok(()),
    }
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
