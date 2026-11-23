// SPDX-License-Identifier: AGPL-3.0-or-later

//! Connection-card pin store: enroll on first dial, refuse a changed trust
//! anchor on every later one (including supervisor redials). Verbatim move.

use super::*;

// ---------------------------------------------------------------------------
// Connection-card pinning
// ---------------------------------------------------------------------------

/// The card a peer first connected with is the identity of its `roam:<label>:`
/// namespace. Displaying a fingerprint without enforcing it let a changed
/// card (a re-shared host, a MITM'd pairing paste) silently become the trust
/// anchor for every chat under that label. The map lives in the core cache,
/// not in a client, so all three clients enforce the same pin.
const CARD_PIN_FILE: &str = "roam-card-pins.json";

/// Serializes read-modify-write of the pin file. Reads are cheap (one small
/// JSON file per dial); re-reading on every dial means a pin changed by
/// another process is never masked by an in-memory copy.
static CARD_PIN_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Debug)]
pub(crate) enum CardPinError {
    /// A malformed card: the old retry budget still applies (it is a bad
    /// paste, not a changed trust anchor).
    InvalidCard(String),
    /// The card's fingerprint differs from the enrolled one. Terminal: the
    /// user must explicitly clear the pin to re-enroll.
    Mismatch {
        label: String,
        pinned: String,
        got: String,
    },
    /// The pin file itself is unreadable/corrupt. Fail closed; clear repairs.
    Store(String),
}

impl fmt::Display for CardPinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCard(e) => write!(f, "invalid roam card: {e}"),
            Self::Mismatch {
                label,
                pinned,
                got,
            } => write!(
                f,
                "roam card for '{label}' changed: pinned {pinned}, got {got}. \
                 Refusing to dial — clear the pin to re-enroll this peer"
            ),
            Self::Store(e) => write!(
                f,
                "roam card pin store is unusable: {e}. Clear the pin to repair it"
            ),
        }
    }
}

pub(super) fn pins_path(cache: &CacheStore) -> PathBuf {
    cache.dir().join(CARD_PIN_FILE)
}

fn read_pins(path: &Path) -> Result<BTreeMap<String, String>, CardPinError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => {
            return Err(CardPinError::Store(format!(
                "cannot read {}: {e}",
                path.display()
            )))
        }
    };
    serde_json::from_slice(&bytes).map_err(|e| {
        CardPinError::Store(format!("cannot parse {}: {e}", path.display()))
    })
}

/// 0600 from creation, unguessable temp name, same-directory rename. The
/// fingerprint is public, but the pin file is a trust decision: a world-
/// writable pin (or a predictable temp that another user can plant) would let
/// anyone choose which host this client accepts for a label.
fn write_pins(path: &Path, pins: &BTreeMap<String, String>) -> Result<(), CardPinError> {
    let bytes = serde_json::to_vec(pins)
        .map_err(|e| CardPinError::Store(format!("cannot encode pins: {e}")))?;
    write_private_atomic(path, &bytes)
        .map_err(|e| CardPinError::Store(format!("cannot write {}: {e}", path.display())))
}

fn write_private_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let leaf = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("roam-card-pins.json");
    for attempt in 0..8 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = path.with_file_name(format!(
            ".{leaf}.tmp-{}-{nanos}-{seq:08x}-{attempt}",
            std::process::id()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = match options.open(&tmp) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let written = (|| {
            file.write_all(data)?;
            file.flush()?;
            file.sync_all()
        })();
        drop(file);
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        if let Some(parent) = path.parent() {
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique pin temp file",
    ))
}

/// The pinned fingerprint for a label, if any (for a client's pin viewer).
pub(crate) fn roam_card_pin(cache: &CacheStore, label: &str) -> Option<String> {
    let _guard = CARD_PIN_LOCK.lock();
    read_pins(&pins_path(cache)).ok()?.remove(label)
}

/// Every `{label: fingerprint}` pin, for a settings screen that lists them.
pub(crate) fn roam_card_pins(cache: &CacheStore) -> BTreeMap<String, String> {
    let _guard = CARD_PIN_LOCK.lock();
    read_pins(&pins_path(cache)).unwrap_or_default()
}

/// Explicitly enroll/replace a pin (e.g. after a user confirms a re-shared
/// card out of band).
pub(crate) fn roam_card_pin_enroll(
    cache: &CacheStore,
    label: &str,
    fingerprint: &str,
) -> Result<(), String> {
    let _guard = CARD_PIN_LOCK.lock();
    let path = pins_path(cache);
    let mut pins = read_pins(&path).map_err(|e| e.to_string())?;
    pins.insert(label.to_string(), fingerprint.to_string());
    write_pins(&path, &pins).map_err(|e| e.to_string())
}

/// Forget one label's pin so the next dial re-enrolls. A corrupt file is
/// repaired by writing the remaining pins (or an empty map) back.
pub(crate) fn roam_card_pin_clear(cache: &CacheStore, label: &str) -> Result<(), String> {
    let _guard = CARD_PIN_LOCK.lock();
    let path = pins_path(cache);
    let mut pins = read_pins(&path).unwrap_or_default();
    pins.remove(label);
    write_pins(&path, &pins).map_err(|e| e.to_string())
}

/// Enroll on first use, compare on every later dial. Called by the supervisor
/// on EVERY attempt, including reconnect redials.
pub(crate) fn verify_card_fingerprint(
    cache: &CacheStore,
    label: &str,
    fingerprint: &str,
) -> Result<(), CardPinError> {
    let _guard = CARD_PIN_LOCK.lock();
    let path = pins_path(cache);
    let mut pins = read_pins(&path)?;
    if let Some(pinned) = pins.get(label) {
        if pinned != fingerprint {
            return Err(CardPinError::Mismatch {
                label: label.to_string(),
                pinned: pinned.clone(),
                got: fingerprint.to_string(),
            });
        }
        return Ok(());
    }
    pins.insert(label.to_string(), fingerprint.to_string());
    write_pins(&path, &pins)
}

pub(super) fn verify_card_pin(
    cache: &CacheStore,
    label: &str,
    card: &str,
) -> Result<(), CardPinError> {
    let fingerprint = grouse_roam_core::card_fingerprint(card)
        .map_err(|e| CardPinError::InvalidCard(e.to_string()))?;
    verify_card_fingerprint(cache, label, &fingerprint)
}

