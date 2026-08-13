//! Custom WebSocket transport for the grouse core.
//!
//! The stock `agent-client-protocol-http::HttpClient` cannot express what the
//! goosed server needs: its `run_ws` performs `connect_async` with no headers
//! and the platform's default TLS roots. grouse requires:
//!
//!   * an `X-Secret-Key` header on the WebSocket upgrade request, and
//!   * trust-all TLS (the server presents a self-signed certificate).
//!
//! This module implements [`ConnectTo<Client>`] directly: it performs the
//! handshake with a rustls connector whose certificate verifier accepts every
//! server certificate, then feeds the resulting byte stream into the SDK's
//! [`Channel`] exactly like `HttpClient::run_ws` does — newline-independent,
//! JSON-RPC text frames both ways.

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
    secret_key: String,
}

impl WsTransport {
    /// Build a transport from the parts of a `ServerConfig`.
    pub fn new(host: &str, port: u16, secret_key: &str, use_tls: bool) -> Self {
        let scheme = if use_tls { "wss" } else { "ws" };
        Self {
            endpoint: format!("{scheme}://{host}:{port}/acp"),
            secret_key: secret_key.to_string(),
        }
    }

    /// The upgrade request: the full WebSocket client handshake (tungstenite
    /// 0.29 requires the client to supply Host/Upgrade/Sec-WebSocket-Key —
    /// it no longer fills them in), plus the `X-Secret-Key` header.
    fn handshake_request(&self) -> Result<Request<()>, AcpError> {
        let key = async_tungstenite::tungstenite::handshake::client::generate_key();
        Request::builder()
            .method("GET")
            .uri(self.endpoint.as_str())
            .header("Host", self.endpoint.trim_start_matches("ws://").trim_start_matches("wss://"))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", key)
            .header("X-Secret-Key", self.secret_key.as_str())
            .body(())
            .map_err(|e| AcpError::internal_error().data(format!("invalid WebSocket request: {e}")))
    }

    /// Connect and drive the WebSocket against the SDK channel.
    async fn run(self, channel: Channel) -> Result<(), AcpError> {
        let request = self.handshake_request()?;
        let connector = trust_all_connector();
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

    let reader = async move {
        let mut discard_incoming = false;
        loop {
            match ws_rx.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    if discard_incoming {
                        continue;
                    }
                    let frame = TransportFrame::parse_json(text.as_str());
                    if incoming.unbounded_send(frame).is_err() {
                        // The client channel closed; drain the socket until it
                        // ends so graceful shutdown still works.
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

    pin_mut!(writer, reader);
    match select(writer, reader).await {
        futures::future::Either::Left((result, _))
        | futures::future::Either::Right((result, _)) => result,
    }
}

/// A `ServerCertVerifier` that accepts every certificate. Trust-all for the
/// goosed server's self-signed cert — this is the whole point of the custom
/// transport, so it is deliberately not configurable here.
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
