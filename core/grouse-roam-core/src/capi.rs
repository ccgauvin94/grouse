// SPDX-License-Identifier: AGPL-3.0-or-later

//! C ABI for the KDE desktop client.
//!
//! The desktop app dlopens this library (QLibrary) and speaks ACP framing over
//! the returned byte-stream handle — the same surface the uniffi Kotlin
//! bindings expose, without a binding generator. All functions are
//! panic-safe where possible: the workspace release profile sets panic="unwind"
//! (see core/Cargo.toml), so uniffi's catch_unwind turns a panic into a foreign
//! exception rather than aborting the process.
//!
//! Conventions:
//!   * Strings are malloc-allocated UTF-8, freed with `grc_string_free`.
//!   * `out_err` (`char **`) receives a malloc'd message on failure (freed
//!     with `grc_string_free`); left NULL on success.
//!   * The stream handle is an opaque pointer valid until `grc_stream_close`.

use std::ffi::{c_char, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use crate::{RoamError, RoamStream, card_fingerprint, identity_generate, identity_public_key, roam_connect};

// The canonical implementation lives in grouse-core's src/cabi_helpers.rs and
// is compiled into this crate from one source. A crate dependency cannot run
// this way (grouse-core depends on THIS crate), and this module is private, so
// an in-crate `#[path]` include is the only way to keep one implementation.
#[path = "../../grouse-core/src/cabi_helpers.rs"]
mod cabi_helpers;
use cabi_helpers::{c_param, c_str, set_err as set_c_str_err, string_free};

/// `RoamError`-typed adapter over the shared `set_err` (whose message argument
/// is a plain `&str`). The error text goes through the shared, NUL-sanitizing
/// `c_str`, so an error from a peer can no longer turn `out_err` into NULL.
fn set_err(out_err: *mut *mut c_char, e: &RoamError) {
    set_c_str_err(out_err, &e.to_string());
}

/// Free a string returned by any other grc_* function.
#[no_mangle]
pub extern "C" fn grc_string_free(s: *mut c_char) {
    unsafe { string_free(s) };
}

/// Generate a fresh iroh secret key, base64. Caller frees with grc_string_free.
#[no_mangle]
pub extern "C" fn grc_identity_generate() -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| c_str(&identity_generate()))).unwrap_or(std::ptr::null_mut())
}

/// Hex public key for a secret key. NULL on error (out_err set).
#[no_mangle]
pub extern "C" fn grc_identity_public_key(secret: *const c_char, out_err: *mut *mut c_char) -> *mut c_char {
    if !out_err.is_null() {
        unsafe { *out_err = std::ptr::null_mut() };
    }
    let Some(secret) = (unsafe { c_param(secret) }) else {
        set_err(out_err, &RoamError::Message("secret is null".into()));
        return std::ptr::null_mut();
    };
    catch_unwind(AssertUnwindSafe(|| match identity_public_key(secret) {
        Ok(k) => c_str(&k),
        Err(e) => {
            set_err(out_err, &e);
            std::ptr::null_mut()
        }
    }))
    .unwrap_or(std::ptr::null_mut())
}

/// Fingerprint of a connection card, for the pairing UI. NULL on error.
#[no_mangle]
pub extern "C" fn grc_card_fingerprint(card: *const c_char, out_err: *mut *mut c_char) -> *mut c_char {
    if !out_err.is_null() {
        unsafe { *out_err = std::ptr::null_mut() };
    }
    let Some(card) = (unsafe { c_param(card) }) else {
        set_err(out_err, &RoamError::Message("card is null".into()));
        return std::ptr::null_mut();
    };
    catch_unwind(AssertUnwindSafe(|| match card_fingerprint(card) {
        Ok(f) => c_str(&f),
        Err(e) => {
            set_err(out_err, &e);
            std::ptr::null_mut()
        }
    }))
    .unwrap_or(std::ptr::null_mut())
}

/// Dial a host from its card and complete the roam handshake. Blocking — call
/// off the UI thread. Returns the stream handle, or NULL with out_err set.
#[no_mangle]
pub extern "C" fn grc_roam_connect(
    secret: *const c_char,
    card: *const c_char,
    label: *const c_char,
    out_err: *mut *mut c_char,
) -> *mut c_void {
    if !out_err.is_null() {
        unsafe { *out_err = std::ptr::null_mut() };
    }
    let (Some(secret), Some(card)) = (unsafe { c_param(secret) }, unsafe { c_param(card) }) else {
        set_err(out_err, &RoamError::Message("secret/card is null".into()));
        return std::ptr::null_mut();
    };
    let label = unsafe { c_param(label) }.map(str::to_owned);
    catch_unwind(AssertUnwindSafe(|| match roam_connect(secret, card, label) {
        Ok(stream) => Arc::into_raw(stream) as *mut c_void,
        Err(e) => {
            set_err(out_err, &e);
            std::ptr::null_mut()
        }
    }))
    .unwrap_or(std::ptr::null_mut())
}

/// Blocking read of up to `cap` bytes into `buf`. Returns the byte count,
/// 0 = EOF, -1 = error (out_err set). The read blocks until at least one
/// chunk arrives, exactly like the uniffi surface.
#[no_mangle]
pub extern "C" fn grc_stream_read(
    h: *mut c_void,
    buf: *mut u8,
    cap: usize,
    out_err: *mut *mut c_char,
) -> i64 {
    if h.is_null() || buf.is_null() {
        return -1;
    }
    let stream = unsafe { &*(h as *const RoamStream) };
    if !out_err.is_null() {
        unsafe { *out_err = std::ptr::null_mut() };
    }
    catch_unwind(AssertUnwindSafe(|| {
        let buf = unsafe { std::slice::from_raw_parts_mut(buf, cap) };
        match stream.read(cap as i64) {
            Ok(bytes) => {
                let n = bytes.len().min(buf.len());
                buf[..n].copy_from_slice(&bytes[..n]);
                n as i64
            }
            Err(e) => {
                set_err(out_err, &e);
                -1
            }
        }
    }))
    .unwrap_or(-1)
}

/// Blocking write of the whole buffer. 1 = ok, 0 = error (out_err set).
#[no_mangle]
pub extern "C" fn grc_stream_write(
    h: *mut c_void,
    buf: *const u8,
    len: usize,
    out_err: *mut *mut c_char,
) -> i32 {
    if h.is_null() || (buf.is_null() && len > 0) {
        return 0;
    }
    let stream = unsafe { &*(h as *const RoamStream) };
    if !out_err.is_null() {
        unsafe { *out_err = std::ptr::null_mut() };
    }
    catch_unwind(AssertUnwindSafe(|| {
        let data = if len > 0 {
            unsafe { std::slice::from_raw_parts(buf, len) }.to_vec()
        } else {
            Vec::new()
        };
        match stream.write(data) {
            Ok(()) => 1,
            Err(e) => {
                set_err(out_err, &e);
                0
            }
        }
    }))
    .unwrap_or(0)
}

/// Interrupt a blocked `read()` (used by close paths): the next read returns
/// an error instead of waiting for the peer. Thread-safe.
#[no_mangle]
pub extern "C" fn grc_stream_cancel(h: *mut c_void) {
    if h.is_null() {
        return;
    }
    let stream = unsafe { &*(h as *const RoamStream) };
    catch_unwind(AssertUnwindSafe(|| stream.cancel())).ok();
}

/// Drop the write half (FIN to the peer); reads continue until EOF. Safe to
/// call from any thread; the handle stays valid until `grc_stream_free`.
#[no_mangle]
pub extern "C" fn grc_stream_shutdown(h: *mut c_void) {
    if h.is_null() {
        return;
    }
    let stream = unsafe { &*(h as *const RoamStream) };
    catch_unwind(AssertUnwindSafe(|| stream.shutdown())).ok();
}

/// Release the handle (decrements the Arc). MUST be called from the reader
/// thread after its in-flight `read` has returned — the handle becomes
/// dangling immediately, so nobody may touch it afterwards.
#[no_mangle]
pub extern "C" fn grc_stream_free(h: *mut c_void) {
    if h.is_null() {
        return;
    }
    unsafe { drop(Arc::from_raw(h as *const RoamStream)) };
}

#[cfg(test)]
mod capi_tests {
    use super::*;

    /// Peer-supplied text (an error message, a card label) is untrusted. The
    /// old roam-local `c_str` returned NULL for an embedded NUL, silently
    /// dropping the error; the shared one replaces it with U+FFFD instead.
    #[test]
    fn c_str_sanitizes_interior_nul() {
        let s = c_str("peer\0text");
        assert!(!s.is_null());
        assert_eq!(unsafe { c_param(s) }, Some("peer\u{FFFD}text"));
        grc_string_free(s);
    }
}
