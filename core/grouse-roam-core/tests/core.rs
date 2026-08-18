//! JVM-independent unit tests for the core. The dial-path test needs no host:
//! a card pointing at a dead relay exercises the full bind→dial→handshake
//! plumbing and must fail cleanly.
//!
//! The full end-to-end (core ↔ `goose serve --roam`) needs a built goose and a
//! relay; see `tests/e2e.rs` (ignored) for the shape.

use grouse_roam_core::{
    card_fingerprint, identity_generate, identity_public_key, roam_connect,
};
use base64::Engine;

#[test]
fn identity_round_trip_is_stable() {
    let secret = identity_generate();
    // Public key derived from the same secret must be identical every time.
    let pk1 = identity_public_key(&secret).unwrap();
    let pk2 = identity_public_key(&secret).unwrap();
    assert_eq!(pk1, pk2);
    assert!(pk1.len() >= 16, "public key should be a hex node id");
}

#[test]
fn identity_rejects_garbage() {
    assert!(identity_public_key("not-base64!!").is_err());
    assert!(identity_public_key("").is_err());
    // 31 bytes is not a 32-byte secret key.
    let short = base64::engine::general_purpose::STANDARD.encode([0u8; 31]);
    assert!(identity_public_key(&short).is_err());
}

#[test]
fn card_round_trip_and_fingerprint() {
    let key = iroh::SecretKey::generate();
    let card = goose_roaming::ConnectionCard::new(
        iroh::EndpointId::from(key.public()),
        vec!["https://relay.example.com".to_string()],
    );
    let encoded = card.encode().unwrap();
    let fp = card_fingerprint(&encoded).unwrap();
    assert!(!fp.is_empty());
    assert_eq!(card.fingerprint(), fp, "fingerprint must be stable across encode/decode");

    // Garbage cards are rejected, not misread.
    assert!(card_fingerprint("junk").is_err());
    assert!(card_fingerprint("").is_err());
}

#[test]
fn connect_to_dead_relay_fails_cleanly() {
    let secret = identity_generate();
    let key = iroh::SecretKey::generate();
    // A relay that nothing is listening on: the dial must error, not hang or panic.
    let card = goose_roaming::ConnectionCard::new(
        iroh::EndpointId::from(key.public()),
        vec!["https://127.0.0.1:9".to_string()],
    )
    .encode()
    .unwrap();
    let err = match roam_connect(&secret, &card, Some("test".into())) {
        Err(e) => e,
        Ok(_) => panic!("expected a dial failure"),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("connect") || msg.contains("relay") || msg.contains("transport"),
        "unexpected error: {msg}"
    );
}
