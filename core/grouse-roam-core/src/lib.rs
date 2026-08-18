//! grouse-roam-core: the native roam transport for the Grouse Android (and
//! later KDE) clients.
//!
//! The transport itself lives in the goose fork's `goose-roaming` crate — this
//! crate is deliberately THIN: it exposes exactly the surface a native ACP
//! client needs, through uniffi-generated Kotlin bindings:
//!
//!   * identity — generate/derive the iroh secret key. The app owns the bytes
//!     (SecureStore on Android); the core never persists anything.
//!   * connect — dial a host from its `ConnectionCard`, do the roam
//!     handshake, and hand back a blocking byte-stream handle.
//!   * stream — blocking read/write/close over the authenticated duplex. The
//!     caller (AcpClient's roam transport) speaks ACP framing over it — the
//!     same framing goose uses on stdio.
//!
//! Everything is synchronous on the uniffi boundary: calls run on uniffi's
//! worker threads, and the internal tokio runtime drives iroh underneath.

use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::sync::mpsc::Receiver;

use base64::Engine;
use futures::AsyncReadExt;
use futures::io::AsyncWriteExt;
use goose_roaming::{ConnectionCard, RoamingConfig, RoamingIdentity, RoamingNode, RoamingClientStream};
use iroh::SecretKey;
use tokio::runtime::Runtime;

uniffi::setup_scaffolding!();

mod capi;

/// A blocking handle over an authenticated roam stream.
///
/// One reader task per stream pulls bytes off the iroh duplex into a channel;
/// `read()` blocks on that channel, `write()` blocks on the write half. EOF is
/// an empty Vec. FIN via `shutdown()` drops the write half (FIN to the peer).
/// One chunk off the duplex: bytes, or the reason the stream ended.
type Chunk = Result<Vec<u8>, String>;

#[derive(uniffi::Object)]
pub struct RoamStream {
    rt: &'static Runtime,
    rx: Mutex<Receiver<Chunk>>,
    // A sender clone so close/cancel can interrupt a blocked read() — the
    // reader task's own sender lives in the task, unreachable from here.
    cancel: parking_lot::Mutex<Option<mpsc::Sender<Chunk>>>,
    pending: Mutex<Vec<u8>>,
    write: Mutex<Option<Box<dyn futures::io::AsyncWrite + Send + Unpin>>>,
    closed: Mutex<bool>,
}

fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| Runtime::new().expect("tokio runtime"))
}

/// Generate a fresh iroh secret key, base64-encoded. The app persists this in
/// its secure store and hands it back on every connect — the core keeps no
/// state.
#[uniffi::export]
pub fn identity_generate() -> String {
    let secret = SecretKey::generate();
    base64::engine::general_purpose::STANDARD.encode(secret.to_bytes())
}

/// The public key (hex) for a secret key — the identity a host sees in
/// `peers list` when deciding whether to accept this device.
#[uniffi::export]
pub fn identity_public_key(secret_b64: &str) -> Result<String, RoamError> {
    let secret = decode_secret(secret_b64)?;
    Ok(RoamingIdentity::from_secret(secret).public_key().to_string())
}

/// The fingerprint of a connection card, for the pairing UI to display before
/// the user pastes it into the host.
#[uniffi::export]
pub fn card_fingerprint(card: &str) -> Result<String, RoamError> {
    Ok(ConnectionCard::decode(card)
        .map_err(|e| RoamError::Message(format!("invalid card: {e}")))?
        .fingerprint())
}

/// Dial a host from its card (endpoint id + relay URLs), complete the roam
/// handshake, and return the authorized stream. Only succeeds once the host
/// has accepted this device's key into its allowlist.
///
/// Blocking: the dial + relay round trip takes seconds; call off the UI
/// thread. The returned stream is ready for ACP framing.
#[uniffi::export]
pub fn roam_connect(
    secret_b64: &str,
    card: &str,
    label: Option<String>,
) -> Result<Arc<RoamStream>, RoamError> {
    let secret = decode_secret(secret_b64)?;
    let identity = RoamingIdentity::from_secret(secret);
    let card = ConnectionCard::decode(card)
        .map_err(|e| RoamError::Message(format!("invalid card: {e}")))?;

    let rt = runtime();
    // Keep the NODE (iroh Endpoint) alive for the stream's whole lifetime. iroh 1.0.3
    // tears down every connection when its Endpoint drops — holding only the `Connection`
    // is not enough (the Endpoint runs the IO actor). Dropping the node here is what made
    // connect succeed, then the very first read return "stream closed".
    let (node, stream): (Arc<RoamingNode>, RoamingClientStream) = rt
        .block_on(async {
            // Client node: bind with defaults (public relays, no inbound trust)
            // and dial the card's endpoint.
            let node = RoamingNode::bind(RoamingConfig::new(identity))
                .await
                .map_err(|e| RoamError::Message(format!("bind: {e}")))?;
            let stream = node
                .connect(&card, label)
                .await
                .map_err(|e| RoamError::Message(format!("connect: {e}")))?;
            Ok::<_, RoamError>((node, stream))
        })?;

    let (write, read, conn) = stream.into_futures_io();
    let (tx, rx) = mpsc::channel::<Chunk>();
    let cancel_tx = tx.clone();
    rt.spawn(async move {
        // `conn` AND `node` are moved in here and held for the task's lifetime — dropping
        // either closes the iroh connection under the stream.
        let mut read = read;
        let _conn = conn;
        let _node = node;
        let mut buf = vec![0u8; 16384];
        loop {
            match read.read(&mut buf).await {
                Ok(0) | Err(_) => {
                    let _ = tx.send(Err("stream closed".into()));
                    break;
                }
                Ok(n) => {
                    if tx.send(Ok(buf[..n].to_vec())).is_err() {
                        break; // consumer gone
                    }
                }
            }
        }
    });

    Ok(Arc::new(RoamStream {
        rt,
        rx: Mutex::new(rx),
        cancel: parking_lot::Mutex::new(Some(cancel_tx)),
        pending: Mutex::new(Vec::new()),
        write: Mutex::new(Some(Box::new(write))),
        closed: Mutex::new(false),
    }))
}

#[uniffi::export]
impl RoamStream {
    /// Read up to `max` bytes, blocking until at least one chunk arrives.
    /// Empty Vec = EOF. Excess bytes are buffered for the next call.
    pub fn read(&self, max: i64) -> Result<Vec<u8>, RoamError> {
        let max = max.max(0) as usize;
        loop {
            // Scope the `pending` lock so it is DROPPED before we block on recv() and before
            // re-locking below. Holding it across recv() (and then re-locking the same
            // non-reentrant std Mutex to append) deadlocked read the instant the first chunk
            // arrived — the read task had the bytes, but this side never woke to drain them.
            {
                let mut pending = self.pending.lock().unwrap();
                if !pending.is_empty() {
                    let n = pending.len().min(max);
                    let out: Vec<u8> = pending.drain(..n).collect();
                    return Ok(out);
                }
            }
            if *self.closed.lock().unwrap() {
                return Ok(Vec::new());
            }
            let chunk = self
                .rx
                .lock()
                .unwrap()
                .recv()
                .map_err(|_| RoamError::Message("stream closed".into()))?;
            match chunk {
                Err(e) => return Err(RoamError::Message(e)),
                Ok(bytes) => self.pending.lock().unwrap().extend_from_slice(&bytes),
            }
        }
    }

    /// Blocking write of the whole buffer.
    pub fn write(&self, data: Vec<u8>) -> Result<(), RoamError> {
        let mut write = self.write.lock().unwrap();
        let w = write.as_mut().ok_or_else(|| RoamError::Message("stream closed".into()))?;
        // flush after write_all: on iroh's SendStream, write_all only queues bytes; without a
        // flush a small newline-framed ACP message can sit in the send buffer and never reach
        // the host, so the host never reads a line and never replies (initialize hangs).
        self.rt
            .block_on(async {
                w.write_all(&data).await?;
                w.flush().await
            })
            .map_err(|e| RoamError::Message(format!("write: {e}")))
    }

    /// Drop the write half (FIN to the peer). Reads continue until EOF.
    pub fn shutdown(&self) {
        *self.closed.lock().unwrap() = true;
        self.write.lock().unwrap().take();
    }

    /// Interrupt a blocked `read()` (used by close paths): the next recv
    /// returns an error instead of waiting for the peer.
    pub fn cancel(&self) {
        if let Some(tx) = self.cancel.lock().take() {
            let _ = tx.send(Err("cancelled".into()));
        }
    }
}

fn decode_secret(secret_b64: &str) -> Result<SecretKey, RoamError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(secret_b64.trim())
        .map_err(|e| RoamError::Message(format!("invalid identity: {e}")))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| RoamError::Message("identity must be 32 bytes".into()))?;
    Ok(SecretKey::from_bytes(&arr))
}

#[derive(Debug, uniffi::Error)]
pub enum RoamError {
    Message(String),
}

impl std::fmt::Display for RoamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoamError::Message(m) => write!(f, "{m}"),
        }
    }
}
impl std::error::Error for RoamError {}
