#![cfg_attr(rustfmt, rustfmt::skip)]
//! Full end-to-end test: this crate's `roam_connect` against a real
//! `goose serve --roam` host over a real relay. Needs infrastructure, so it is
//! ignored by default:
//!
//!   1. A relay: `cargo run -p iroh-relay --bin iroh-relay` (or set
//!      `GOOSE_ROAM_RELAYS` on the host).
//!   2. A host built from the fork with roaming enabled:
//!      `cargo build --features roaming --bin goose` then
//!      `goose serve --roam` with a trust file, and the test's identity
//!      accepted (`goose roam peers accept <key>`).
//!   3. Run with: `cargo test --test e2e -- --ignored --nocapture`.

use grouse_roam_core::{identity_generate, roam_connect};

#[test]
#[ignore = "requires a live serve --roam host and relay"]
fn connect_to_live_host_and_do_acp_initialize() {
    let secret = identity_generate();
    let card = std::env::var("ROAM_TEST_CARD").expect("ROAM_TEST_CARD must be set");
    let stream = roam_connect(&secret, &card, Some("grouse-core-test".into()))
        .expect("dial + handshake should succeed against an accepting host");

    // ACP initialize over the stream — proves the byte duplex is ACP-usable.
    let frame = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}"#;
    stream.write(frame.to_vec()).expect("write");

    let mut acc = Vec::new();
    loop {
        let chunk = stream.read(16384).expect("read");
        if chunk.is_empty() {
            panic!("EOF before initialize reply");
        }
        acc.extend_from_slice(&chunk);
        // The ACP framing here is length-prefixed; for the smoke test, stop when
        // we have a full JSON object that answers id 1.
        let text = String::from_utf8_lossy(&acc);
        if text.contains("\"id\":1") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&acc);
    assert!(text.contains("result"), "expected a result, got: {text}");
    stream.shutdown();
}
