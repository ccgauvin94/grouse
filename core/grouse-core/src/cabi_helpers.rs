// SPDX-License-Identifier: AGPL-3.0-or-later

//! The handful of C-ABI plumbing functions shared by the two ABI faces in this
//! workspace: `grouse-core/src/capi.rs` (`grouse_*`) and
//! `grouse-roam-core/src/capi.rs` (`grc_*`).
//!
//! Both files `mod cabi_helpers;` this ONE source; the roam side includes it
//! with an explicit `#[path]` pointing back here. A crate-level shared crate
//! is impossible in this direction: `grouse-core` already depends on
//! `grouse-roam-core`, so making roam depend on core would be a dependency
//! cycle, and roam's `capi` module is private (nothing can be re-exported
//! through it). Compiling one file into both crates keeps the null handling,
//! NUL sanitization and free semantics literally the same code.
//!
//! Nothing here is `#[no_mangle]`: each ABI keeps its own exported
//! `grouse_string_free` / `grc_string_free` symbol, both of which delegate to
//! [`string_free`].

use std::ffi::{CStr, CString, c_char};

/// malloc-allocated UTF-8 copy of `s`, freed with the ABI's `*_string_free`.
///
/// Server text is untrusted and can contain U+0000. `CString::new` rejects that
/// and used to return NULL, which made a callback silently SKIP the event (a
/// turn, a permission request, a tool result — all lost). Replace interior
/// NULs with U+FFFD instead: the callback always fires and the C string is
/// always well-formed. The common no-NUL path costs no extra allocation.
pub(crate) fn c_str(s: &str) -> *mut c_char {
    let encoded = if s.as_bytes().contains(&0) {
        CString::new(s.replace('\0', "\u{FFFD}").into_bytes())
    } else {
        CString::new(s.as_bytes())
    };
    encoded.map(CString::into_raw).unwrap_or(std::ptr::null_mut())
}

/// Borrow a UTF-8 C string; NULL and invalid UTF-8 are both "absent".
///
/// The caller must keep the pointer alive for the returned borrow; callback
/// payloads live until the callback returns (see each ABI's free contract).
pub(crate) unsafe fn c_param<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok()
}

/// Put a malloc'd copy of `msg` in `out_err`; leave it NULL on success paths.
pub(crate) fn set_err(out_err: *mut *mut c_char, msg: &str) {
    if !out_err.is_null() {
        unsafe { *out_err = c_str(msg) };
    }
}

/// Release a string returned by any `grouse_*` / `grc_*` function or handed to
/// a callback. The same allocator is used by both ABIs (`CString::into_raw`).
pub(crate) unsafe fn string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}
