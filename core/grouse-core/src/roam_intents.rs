// SPDX-License-Identifier: AGPL-3.0-or-later

//! Roam identity and connection-card pin intents.
//! The facade, records and shared state stay in `lib.rs`; this module
//! contributes inherent `impl Core` methods only. Behaviour is unchanged.

use super::*;

impl Core {
    // -- roam helpers ---------------------------------------------------------
    pub(crate) fn active_peer(&self) -> Option<Arc<RoamPeer>> {
        let peers = self.inner.peers.lock();
        let active = self.inner.active_peer_label.read().clone();
        active_peer(&peers, &active).cloned()
    }

    /// The device's roam identity (grouse-roam-core), generated + persisted
    /// under the cache dir on first use (desktop m_store roam_identity).
    pub(crate) fn roam_identity(&self) -> String {
        let dir = self.inner.cache.dir();
        let path = dir.join("roam_identity");
        // Never follow a planted symlink into someone else's file, and tighten
        // a pre-existing (pre-fix) 0644 identity BEFORE its contents are read.
        let regular = std::fs::symlink_metadata(&path)
            .map(|meta| !meta.file_type().is_symlink())
            .unwrap_or(false);
        if regular {
            #[cfg(unix)]
            crate::cache::make_private(&path);
            if let Ok(existing) = std::fs::read_to_string(&path) {
                let existing = existing.trim();
                if !existing.is_empty() {
                    return existing.to_string();
                }
            }
        }
        let secret = grouse_roam_core::identity_generate();
        // Atomic, 0600 from creation, unguessable temp name: a crash mid-write
        // must not leave a truncated identity that would re-roll the secret.
        let _ = write_secret_file(&path, &secret);
        secret
    }

    /// The pinned card fingerprint for a roam label, if the peer was enrolled.
    pub fn roam_card_pin(&self, label: String) -> Option<String> {
        crate::roam::roam_card_pin(&self.inner.cache, &label)
    }

    /// All `{label: fingerprint}` pins as JSON (uniffi has no map type).
    pub fn roam_card_pins(&self) -> String {
        serde_json::to_string(&crate::roam::roam_card_pins(&self.inner.cache))
            .unwrap_or_else(|_| "{}".to_string())
    }

    /// Explicitly enroll/replace a pin after the user confirmed a re-shared card.
    pub fn roam_card_pin_enroll(&self, label: String, fingerprint: String) -> Result<(), String> {
        crate::roam::roam_card_pin_enroll(&self.inner.cache, &label, &fingerprint)
    }

    /// Forget a label's pin so the next dial re-enrolls that peer.
    pub fn roam_card_pin_clear(&self, label: String) -> Result<(), String> {
        crate::roam::roam_card_pin_clear(&self.inner.cache, &label)
    }

    /// Override the persisted roam identity so the wire dials with the SAME key
    /// the UI advertises. The platform (desktop QSettings) holds the identity the
    /// user shows a host as a card; without this sync the core would generate and
    /// dial with its own separate secret, which the host never accepted
    /// (not_allowlisted). Blank/unset leaves the core's own identity in place.
    pub fn set_roam_identity(&self, secret: String) {
        let s = secret.trim();
        if s.is_empty() {
            return;
        }
        let dir = self.inner.cache.dir();
        let path = dir.join("roam_identity");
        // A rename over an existing path replaces the inode: the imported key
        // is never written through a predictable temp file or logged.
        let _ = write_secret_file(&path, s);
    }
}
