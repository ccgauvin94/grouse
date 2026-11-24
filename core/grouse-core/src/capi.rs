// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(clippy::not_unsafe_ptr_arg_deref)]

//! C ABI for the KDE desktop client (thin client).
//!
//! The desktop app dlopens this cdylib (QLibrary) and drives the WHOLE
//! grouse-core surface through it — the same contract the uniffi Kotlin
//! bindings expose, without a binding generator. This is the single path from
//! QML to the wire; the desktop no longer carries its own ACP client.
//!
//! The C ABI mirrors `core/grouse-roam-core/src/capi.rs` conventions exactly:
//!   * Every public symbol is `#[no_mangle] pub extern "C"`, prefixed
//!     `grouse_` (NOT the roam `grc_` prefix — `grouse_` avoids ambiguity and
//!     matches this crate).
//!   * Every entry is wrapped in `std::panic::catch_unwind(AssertUnwindSafe(..))`
//!     and returns a NULL/`-1` sentinel on panic. This works because the
//!     workspace release profile sets `panic = "unwind"` (core/Cargo.toml),
//!     turning a panic into a foreign-error return instead of an abort.
//!   * Strings are malloc-allocated UTF-8, freed with `grouse_string_free`.
//!   * `out_err` (`char **`) receives a malloc'd message on failure (freed with
//!     the same free); left NULL on success.
//!   * The Core handle is an opaque `*mut c_void` created by `grouse_core_create`
//!     and released by `grouse_core_free`. It owns the stable `Core` AND the
//!     `GrouseUnstable` shim (CONTRACT §3 + §5).
//!   * Structured records/enums/Vecs cross the ABI as JSON C-strings; scalar
//!     strings pass through as-is. A C consumer MUST copy any string/JSON it
//!     needs inside a callback — the core frees it when the callback returns.
//!     Callbacks fire on the core's worker thread (the C++ bridge marshals
//!     onto Qt main).

use std::ffi::{c_char, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use serde::Serialize;

// One source file for the C-string plumbing shared with the roam ABI (see the
// module docs there for why a crate-level dependency cannot go this direction).
#[path = "cabi_helpers.rs"]
mod cabi_helpers;
pub(crate) use cabi_helpers::{c_param, c_str, set_err, string_free};

use crate::unstable::GrouseUnstable;
use crate::{
    ConfigOption, ConnectionStatus, Core, CoreListener, GrouseUnstableListener,
    PermissionOutcome, PermissionRequest, ProjectSummary, Prompt, RefreshMode, SendExpect,
    ServerConfig, SessionOwner, SessionSummary, TimelineRow, TranscriptOp,
};

/// Bumped whenever the listener table's layout or a symbol's signature changes.
/// The desktop refuses a library whose value it does not expect — a mismatch is
/// a callback called with the wrong arity (a garbage pointer), not a compile
/// error, so it must be caught at load time.
///
/// 16: `on_advisor_report` appended to the listener table, plus `grouse_advisor`
/// and `grouse_set_advisor` (2026-11-17, `644376c9`). That commit bumped the
/// desktop's expectation to 16 — correctly, since it changed the layout and added
/// two symbols — but left this constant at 15, so a build from the repo refused
/// its own core ("core library ABI 15, expected 16") and the app could not start.
/// The constant is the one that was wrong.
///
/// 17: four new symbols, no listener-table change — `grouse_connect_attach` and
/// `grouse_connected` (bindless reconnect + a wire-up check that is true without
/// a bound session), `grouse_unstable_tools_call_named` (goose requires
/// `extensionName`; the old three-argument export now fails loudly), and
/// `grouse_push_decide_run` (run-id-aware notification policy). A desktop built
/// for 17 calls these unconditionally, so an old `.so` must be refused at load:
/// the ABI gate, not a NULL check at first use, is what keeps it from crashing.
///
/// 18: `on_session_opened` appended to the listener table (the positive answer
/// to `open_session`, so no client has to infer success from a paint/`Window`),
/// plus two new getter symbols — `grouse_session_owner` and `grouse_all_sessions`
/// — for the merged-main-and-peers session view (2026-11-23). The new slot makes
/// the table 48 entries; both sides must bump together or every callback after
/// it would be read with the wrong offset.
///
/// 19: JSON record shapes only — `NotifyContext` gained `armed_owner` (the roam
/// peer label the client armed) and `NotifyDecision` gained `reply_session` (the
/// verified Reply target; `null` for briefings, tasks and unverifiable turns).
/// No symbol signature or listener-table layout changed, but a desktop built for
/// 19 must not load an old `.so` whose decisions have no `reply_session` key.
/// 20: request-correlated server forms and roam card pins. Two listener slots
/// were APPENDED (never inserted): `on_recipe_params_request` and
/// `on_elicitation_request`, each `(owner_json, request_id, payload)` — the old
/// single-string slots keep their offsets but are no longer fired by the core.
/// `respond_recipe_params` / `respond_elicitation` gained a leading
/// `request_id`; `PermissionRequest`'s JSON gained `session_id` and `owner`;
/// four `grouse_roam_card_pin*` exports expose the pin store the phone already
/// has through uniffi. The table is 50 entries. A desktop built for 19 would
/// read the new slots with the wrong arity and answer forms with the wrong key,
/// so the ABI gate must change with the core.
/// COORDINATOR: bump `kExpectedAbiVersion` in `clients/desktop/src/corebridge.cpp`
/// to 20 and land the desktop's owner/request-id handlers with this library.
///
/// 21: four new unstable config symbols — `grouse_unstable_config_read_all`,
/// `grouse_unstable_config_read_ex`, `grouse_unstable_config_upsert_ex` and
/// `grouse_unstable_config_remove` — expose goose's config RPCs from the ACP-only
/// surface (no filesystem/db/env access in the core). No listener slot or existing
/// signature changed, but a desktop built for 21 calls these unconditionally, so an
/// older `.so` must be refused at load.
/// COORDINATOR: bump `kExpectedAbiVersion` in `clients/desktop/src/corebridge.cpp`
/// to 21 with the client-side declarations.
pub const GROUSE_ABI_VERSION: u32 = 21;

/// The C ABI version this library implements (see [`GROUSE_ABI_VERSION`]).
#[no_mangle]
pub extern "C" fn grouse_abi_version() -> u32 {
    GROUSE_ABI_VERSION
}

/// Serialize `v` to a JSON C-string for a callback payload. A serialization
/// failure degrades to an empty string (never NULL), so a callback always
/// receives a valid (possibly empty) pointer.
fn c_json<T: Serialize>(v: &T) -> *mut c_char {
    let s = serde_json::to_string(v).unwrap_or_default();
    c_str(&s)
}

/// Parse a JSON C-string into `T`; NULL/empty/invalid input yields `None`.
fn c_json_in<T: serde::de::DeserializeOwned>(p: *const c_char) -> Option<T> {
    let s = unsafe { c_param(p) }?;
    serde_json::from_str(s).ok()
}

/// Free a string returned by any other `grouse_*` function.
#[no_mangle]
pub extern "C" fn grouse_string_free(s: *mut c_char) {
    unsafe { string_free(s) };
}

/// Decode a push payload into a `PushEnvelope` JSON record (see `notify.rs`). The
/// same function the Kotlin binding exports, so both clients share one parser.
/// Untrusted input crosses this boundary, so a panic is a JSON-`""` no-show,
/// never a panic across `extern "C"`.
#[no_mangle]
pub extern "C" fn grouse_push_parse(raw: *const c_char) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| match unsafe { c_param(raw) } {
        Some(raw) => c_json(&crate::notify::parse_push(raw)),
        None => c_str(""),
    }))
    .unwrap_or_else(|_| c_str(""))
}

/// A missing/malformed context is a no-show, never a crash: `app_visible: true`
/// is the safe side of the notification policy (CONTRACT §5.5).
fn push_context(context_json: *const c_char) -> crate::notify::NotifyContext {
    c_json_in(context_json).unwrap_or(crate::notify::NotifyContext {
        app_visible: true,
        armed_session: None,
        session_title: None,
        announced_session: None,
        announced_secs_ago: None,
        announce_any_turn: false,
        // A malformed context gets the strictest policy: no declared roam owner,
        // so a `roam:` turn cannot become a reply destination.
        armed_owner: None,
    })
}

/// Decide whether a decoded payload should become a notification, given what the
/// client knows (`NotifyContext` JSON: `app_visible`, `armed_session`,
/// `session_title`, `announced_session`, `announced_secs_ago`,
/// `announce_any_turn`, `armed_owner`). Returns a `NotifyDecision` JSON record
/// (`show`, `summary`, `body`, `reply_session`); a malformed context is a no-show
/// rather than a crash. CLIENTS: the notification's Reply action must use
/// `reply_session`, never the envelope's `session` — the latter is sender-supplied.
#[no_mangle]
pub extern "C" fn grouse_push_decide(envelope_json: *const c_char,
                                     context_json: *const c_char) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let envelope: crate::notify::PushEnvelope = match c_json_in(envelope_json) {
            Some(e) => e,
            None => return c_str(""),
        };
        c_json(&crate::notify::decide_notify(envelope, push_context(context_json)))
    }))
    .unwrap_or_else(|_| c_str(""))
}

/// As `grouse_push_decide`, with the run id the client announced the last time
/// it showed a notification (`Some(JSON string)`; a NULL pointer, JSON `null` or
/// an empty string all mean "unknown"). Run ids stop a genuine second turn in
/// the same chat from being swallowed as a duplicate delivery; they do NOT make
/// a sender-supplied session replyable — see `reply_session` in the decision.
#[no_mangle]
pub extern "C" fn grouse_push_decide_run(
    envelope_json: *const c_char,
    context_json: *const c_char,
    announced_run_json_or_null: *const c_char,
) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let envelope: crate::notify::PushEnvelope = match c_json_in(envelope_json) {
            Some(e) => e,
            None => return c_str(""),
        };
        let announced_run = c_json_in::<Option<String>>(announced_run_json_or_null)
            .unwrap_or_else(|| unsafe { c_param(announced_run_json_or_null) }.map(str::to_owned))
            .filter(|s| !s.is_empty());
        c_json(&crate::notify::decide_notify_run(
            envelope,
            push_context(context_json),
            announced_run,
        ))
    }))
    .unwrap_or_else(|_| c_str(""))
}

// ---------------------------------------------------------------------------
// Listener callback table (CONTRACT §3.2 + §5)
// ---------------------------------------------------------------------------

/// The listener callback table the desktop installs at `grouse_core_create`.
///
/// One function pointer per CONTRACT §3.2 (stable) and §5 (unstable) event
/// family, plus `on_error`. Every pointer is optional (the bridge may leave an
/// event it does not consume NULL). The stable `on_projects` (typed
/// `ProjectSummary` array) and the unstable raw-JSON `on_unstable_projects`
/// are distinct slots to disambiguate the two same-named interfaces.
///
/// All pointers take `user_data` first (the value passed to
/// `grouse_core_create`), then the event payload(s), exactly mirroring each
/// uniffi listener method's arity with records/enums/Vecs replaced by JSON
/// C-strings.
#[repr(C)]
#[derive(Default)]
pub struct GrouseCoreListener {
    // -- stable (CONTRACT §3.2) --
    /// `status` serialized as JSON (`ConnectionStatus`).
    pub on_status: Option<extern "C" fn(*mut c_void, *const c_char)>,
    /// `sessions` serialized as a JSON array of `SessionSummary`.
    pub on_sessions: Option<extern "C" fn(*mut c_void, *const c_char)>,
    /// `op` serialized as JSON (`TranscriptOp`) — the rich item stream
    /// (docs/TRANSCRIPT_MODEL.md).
    pub on_item: Option<extern "C" fn(*mut c_void, *const c_char)>,
    /// Context-window usage + cost: `(used, size, cost, currency)`.
    pub on_usage: Option<extern "C" fn(*mut c_void, i64, i64, f64, *const c_char)>,
    /// A turn finished; the C string is the server's stop reason.
    /// `(user_data, session_id, stop_reason)` — the session id is what routes a
    /// completion to its chat when more than one is streaming.
    pub on_run_ended: Option<extern "C" fn(*mut c_void, *const c_char, *const c_char)>,
    /// A sanitized, sentence-sized speakable chunk of the assistant's text
    /// (docs/TRANSCRIPT_MODEL.md) — feed it to a TTS engine.
    pub on_speak: Option<extern "C" fn(*mut c_void, *const c_char)>,
    /// `options` serialized as a JSON array of `ConfigOption`.
    pub on_config: Option<extern "C" fn(*mut c_void, *const c_char)>,
    /// `request` serialized as JSON (`PermissionRequest`). Since ABI 20 that
    /// JSON also carries `session_id` (the display id the approval belongs to)
    /// and `owner` ("main" or the roam peer's bare label), so a prompt can name
    /// the asking wire without parsing `roam:<label>:<id>` in the client.
    pub on_permission_request: Option<extern "C" fn(*mut c_void, *const c_char)>,
    pub on_session_touched:
        Option<extern "C" fn(*mut c_void, *const c_char, *const c_char, *const c_char)>,
    /// `projects` serialized as a JSON array of `ProjectSummary`.
    pub on_projects: Option<extern "C" fn(*mut c_void, *const c_char)>,
    pub on_roam_peer_status:
        Option<extern "C" fn(*mut c_void, *const c_char, *const c_char)>,
    pub on_roam_sessions:
        Option<extern "C" fn(*mut c_void, *const c_char, *const c_char)>,
    pub on_peer_new_session:
        Option<extern "C" fn(*mut c_void, *const c_char, *const c_char)>,
    pub on_active_run: Option<extern "C" fn(*mut c_void, *const c_char, *const c_char)>,
    /// `commands` serialized as a JSON array of strings.
    pub on_commands: Option<extern "C" fn(*mut c_void, *const c_char)>,
    // -- unstable raw-JSON families (CONTRACT §5) --
    pub on_export: Option<extern "C" fn(*mut c_void, *const c_char)>,
    /// Legacy single-payload slot. Kept for table order; the core fires
    /// `on_recipe_params_request` since ABI 20 (it needs the request id to
    /// answer the right server request).
    pub on_recipe_params: Option<extern "C" fn(*mut c_void, *const c_char)>,
    /// Legacy single-payload slot, retained for the same reason.
    pub on_elicitation: Option<extern "C" fn(*mut c_void, *const c_char)>,
    pub on_compaction_status: Option<extern "C" fn(*mut c_void, *const c_char)>,
    pub on_message_usage:
        Option<extern "C" fn(*mut c_void, u64, u64, u64, f64)>,
    /// Unstable `usage_update` — the context-window usage that keeps the bar
    /// live on roam-as-custom-notifications + the desktop. Same shape as the
    /// stable `on_usage` minus the currency (USD, `f64` only).
    pub on_unstable_usage: Option<extern "C" fn(*mut c_void, i64, i64, f64)>,
    pub on_app_resource: Option<extern "C" fn(*mut c_void, *const c_char, *const c_char)>,
    pub on_recipes: Option<extern "C" fn(*mut c_void, *const c_char)>,
    pub on_schedules: Option<extern "C" fn(*mut c_void, *const c_char)>,
    /// Unstable raw-JSON `sources/list` of type `project` (distinct from the
    /// stable typed `on_projects`).
    pub on_unstable_projects: Option<extern "C" fn(*mut c_void, *const c_char)>,
    pub on_skills: Option<extern "C" fn(*mut c_void, *const c_char)>,
    pub on_tools: Option<extern "C" fn(*mut c_void, *const c_char, *const c_char)>,
    pub on_extensions: Option<extern "C" fn(*mut c_void, *const c_char)>,
    pub on_session_extensions:
        Option<extern "C" fn(*mut c_void, *const c_char, *const c_char)>,
    pub on_config_value: Option<extern "C" fn(*mut c_void, *const c_char, *const c_char)>,
    pub on_supported_models:
        Option<extern "C" fn(*mut c_void, *const c_char, *const c_char)>,
    pub on_providers: Option<extern "C" fn(*mut c_void, *const c_char)>,
    pub on_session_probe:
        Option<extern "C" fn(*mut c_void, *const c_char, *const c_char, i64)>,
    pub on_tool_result: Option<extern "C" fn(*mut c_void, *const c_char, i32)>,
    pub on_error: Option<extern "C" fn(*mut c_void, *const c_char, *const c_char)>,
    /// `providers` serialized as a raw JSON map of dictation provider → status.
    pub on_dictation_config: Option<extern "C" fn(*mut c_void, *const c_char)>,
    /// A dictation transcript (the decoded text of the audio the UI sent).
    pub on_transcript: Option<extern "C" fn(*mut c_void, *const c_char)>,
    /// A session open/resume FAILED and nothing was substituted: `(session_id,
    /// message)` (grouse#19). Appended at the END so every earlier field keeps
    /// its offset — the desktop's `corebridge.h` mirrors this struct layout, and
    /// the two are always built together.
    pub on_session_open_failed:
        Option<extern "C" fn(*mut c_void, *const c_char, *const c_char)>,
    /// The My Day tab's task lists as JSON (`{"open": [...], "done": [...],
    /// "errors": {...}}`). Appended at the END for the same reason as
    /// `on_session_open_failed`; ABI 6.
    pub on_tasks: Option<extern "C" fn(*mut c_void, *const c_char)>,
    /// A full replay's progress: `(session_id, loaded, total, done)`. `loaded`
    /// counts MESSAGES the replay has ingested and `total` is the server's
    /// `messageCount` (0 when unknown) — the reload bar's two numbers. `done`
    /// fires once when the replay ends, whatever its length. Appended at the
    /// END; ABI 7.
    pub on_replay_progress:
        Option<extern "C" fn(*mut c_void, *const c_char, i64, i64, i32)>,
    /// A shared file's download as JSON (`FileEvent`: `progress` / `saved` /
    /// `failed`), keyed by the server-side path. Every variant also carries
    /// `session_id` (display id) and `request_id` (the fetch this event answers),
    /// so a client never has to guess the owner from the path alone. Appended at
    /// the END; ABI 9 (payload extended at ABI 18).
    pub on_file: Option<extern "C" fn(*mut c_void, *const c_char)>,
    /// The conversation a thread's chat lives in: `(label, session_id)`, the reply to
    /// `grouse_open_thread`. The CLIENT opens it — see the listener's own note for why
    /// the core no longer binds it. Appended at the END; ABI 11.
    pub on_thread_ready:
        Option<extern "C" fn(*mut c_void, *const c_char, *const c_char)>,
    /// The extension CATALOG of the server that owns a chat: `(session_id, extensions)`.
    /// Same shape as `on_extensions` (that one is the MAIN server's config.yaml); this
    /// one is asked on the chat's own connection, so a roam chat reports its PEER's
    /// catalog. Appended at the END; ABI 12.
    pub on_extension_catalog:
        Option<extern "C" fn(*mut c_void, *const c_char, *const c_char)>,
    /// What fills a chat's context window if it were prompted now: `(session_id,
    /// report)` with the report as raw JSON (`_goose/unstable/context/report`).
    /// Appended at the END; ABI 13.
    pub on_context_report:
        Option<extern "C" fn(*mut c_void, *const c_char, *const c_char)>,
    /// An area's timeline: `(label, rows)` with the merged rows as raw JSON
    /// (`Vec<TimelineRow>`). Appended at the END; ABI 14.
    pub on_thread_timeline:
        Option<extern "C" fn(*mut c_void, *const c_char, *const c_char)>,
    /// The surfacing-sensitivity readout, as the server's JSON. Appended at the END;
    /// ABI 15.
    pub on_sensitivity: Option<extern "C" fn(*mut c_void, *const c_char)>,
    /// The dev advisor's review of the turn that just ended: `(session_id, text)`.
    /// Appended at the END; ABI 16.
    pub on_advisor_report:
        Option<extern "C" fn(*mut c_void, *const c_char, *const c_char)>,
    /// A session was opened successfully and is now bound/displayed: `(session_id)`.
    /// The explicit counterpart of `on_session_open_failed` — a paint or a
    /// `Window` is NOT an open answer (a background fill sends those too).
    /// Appended at the END; ABI 18.
    pub on_session_opened: Option<extern "C" fn(*mut c_void, *const c_char)>,
    /// A recipe-parameter form with the reply key: `(owner_json, request_id,
    /// parameters_json)`. `owner_json` is a `SessionOwner` record (`kind`,
    /// `label`); `request_id` goes back as the first argument of
    /// `grouse_unstable_respond_recipe_params`. Appended at the END; ABI 20.
    pub on_recipe_params_request:
        Option<extern "C" fn(*mut c_void, *const c_char, *const c_char, *const c_char)>,
    /// An elicitation form with the reply key: `(owner_json, request_id,
    /// schema_json)`. Appended at the END; ABI 20.
    pub on_elicitation_request:
        Option<extern "C" fn(*mut c_void, *const c_char, *const c_char, *const c_char)>,
}

/// Bridges the core's typed listener traits to the installed C table. The raw
/// pointers are shared with the desktop for the Core's whole lifetime, so the
/// forwarder is Send+Sync (callbacks fire on the core's worker thread).
struct CoreCallbackForwarder {
    table: *const GrouseCoreListener,
    user_data: *mut c_void,
}

unsafe impl Send for CoreCallbackForwarder {}
unsafe impl Sync for CoreCallbackForwarder {}

/// Call a 1-string callback, allocating + freeing a UTF-8 C copy of `s`.
#[inline]
unsafe fn cb_str1(f: Option<extern "C" fn(*mut c_void, *const c_char)>, user_data: *mut c_void, s: &str) {
    if let Some(f) = f {
        let cs = c_str(s);
        if !cs.is_null() {
            f(user_data, cs);
            grouse_string_free(cs);
        }
    }
}

/// Call a 2-string callback.
#[inline]
unsafe fn cb_str2(
    f: Option<extern "C" fn(*mut c_void, *const c_char, *const c_char)>,
    user_data: *mut c_void,
    a: &str,
    b: &str,
) {
    if let Some(f) = f {
        let ca = c_str(a);
        let cb = c_str(b);
        if !ca.is_null() && !cb.is_null() {
            f(user_data, ca, cb);
        }
        if !ca.is_null() {
            grouse_string_free(ca);
        }
        if !cb.is_null() {
            grouse_string_free(cb);
        }
    }
}

/// Call a 3-string callback.
#[inline]
unsafe fn cb_str3(
    f: Option<extern "C" fn(*mut c_void, *const c_char, *const c_char, *const c_char)>,
    user_data: *mut c_void,
    a: &str,
    b: &str,
    c: &str,
) {
    if let Some(f) = f {
        let ca = c_str(a);
        let cb = c_str(b);
        let cc = c_str(c);
        if !ca.is_null() && !cb.is_null() && !cc.is_null() {
            f(user_data, ca, cb, cc);
        }
        if !ca.is_null() {
            grouse_string_free(ca);
        }
        if !cb.is_null() {
            grouse_string_free(cb);
        }
        if !cc.is_null() {
            grouse_string_free(cc);
        }
    }
}

/// Call a string+int callback (e.g. `on_tool_result(text, is_error)`).
#[inline]
unsafe fn cb_str_i32(
    f: Option<extern "C" fn(*mut c_void, *const c_char, i32)>,
    user_data: *mut c_void,
    s: &str,
    v: i32,
) {
    if let Some(f) = f {
        let cs = c_str(s);
        if !cs.is_null() {
            f(user_data, cs, v);
            grouse_string_free(cs);
        }
    }
}

/// Call a string+two ints+flag callback (`on_replay_progress`).
#[inline]
unsafe fn cb_replay_progress(
    f: Option<extern "C" fn(*mut c_void, *const c_char, i64, i64, i32)>,
    user_data: *mut c_void,
    session_id: &str,
    loaded: i64,
    total: i64,
    done: bool,
) {
    if let Some(f) = f {
        let cs = c_str(session_id);
        if !cs.is_null() {
            f(user_data, cs, loaded, total, if done { 1 } else { 0 });
            grouse_string_free(cs);
        }
    }
}

/// Call a string+string+int callback (`on_session_probe`).
#[inline]
unsafe fn cb_str2_i64(
    f: Option<extern "C" fn(*mut c_void, *const c_char, *const c_char, i64)>,
    user_data: *mut c_void,
    a: &str,
    b: &str,
    v: i64,
) {
    if let Some(f) = f {
        let ca = c_str(a);
        let cb = c_str(b);
        if !ca.is_null() && !cb.is_null() {
            f(user_data, ca, cb, v);
        }
        if !ca.is_null() {
            grouse_string_free(ca);
        }
        if !cb.is_null() {
            grouse_string_free(cb);
        }
    }
}

/// Dereference `self.table.<field>` to an `Option<fn>`.
macro_rules! table_fn {
    ($self:expr, $field:ident) => {
        (&*$self.table).$field
    };
}

impl CoreListener for CoreCallbackForwarder {
    fn on_status(&self, status: ConnectionStatus) {
        let p = c_json(&status);
        unsafe {
            let table = &*self.table;
            if let Some(f) = table.on_status {
                f(self.user_data, p);
            }
            grouse_string_free(p);
        }
    }
    fn on_sessions(&self, sessions: Vec<SessionSummary>) {
        let p = c_json(&sessions);
        unsafe {
            let table = &*self.table;
            if let Some(f) = table.on_sessions {
                f(self.user_data, p);
            }
            grouse_string_free(p);
        }
    }
    fn on_item(&self, op: TranscriptOp) {
        let p = c_json(&op);
        unsafe {
            let table = &*self.table;
            if let Some(f) = table.on_item {
                f(self.user_data, p);
            }
            grouse_string_free(p);
        }
    }
    fn on_usage(&self, used: i64, size: i64, cost: f64, currency: String) {
        unsafe {
            let table = &*self.table;
            if let Some(f) = table.on_usage {
                let cs = c_str(&currency);
                if !cs.is_null() {
                    f(self.user_data, used, size, cost, cs);
                    grouse_string_free(cs);
                }
            }
        }
    }
    fn on_run_ended(&self, session_id: String, stop_reason: String) {
        let sid = c_str(&session_id);
        let p = c_str(&stop_reason);
        unsafe {
            let table = &*self.table;
            if let Some(f) = table.on_run_ended {
                f(self.user_data, sid, p);
            }
            grouse_string_free(sid);
            grouse_string_free(p);
        }
    }
    fn on_speak(&self, text: String) {
        let p = c_str(&text);
        unsafe {
            let table = &*self.table;
            if let Some(f) = table.on_speak {
                f(self.user_data, p);
            }
            grouse_string_free(p);
        }
    }
    fn on_config(&self, options: Vec<ConfigOption>) {
        let p = c_json(&options);
        unsafe {
            let table = &*self.table;
            if let Some(f) = table.on_config {
                f(self.user_data, p);
            }
            grouse_string_free(p);
        }
    }
    fn on_permission_request(&self, request: PermissionRequest) {
        let p = c_json(&request);
        unsafe {
            let table = &*self.table;
            if let Some(f) = table.on_permission_request {
                f(self.user_data, p);
            }
            grouse_string_free(p);
        }
    }
    fn on_session_touched(&self, session_id: String, title: String, updated_at: String) {
        unsafe {
            cb_str3(table_fn!(self, on_session_touched), self.user_data, &session_id, &title, &updated_at);
        }
    }
    fn on_projects(&self, projects: Vec<ProjectSummary>) {
        let p = c_json(&projects);
        unsafe {
            let table = &*self.table;
            if let Some(f) = table.on_projects {
                f(self.user_data, p);
            }
            grouse_string_free(p);
        }
    }
    fn on_roam_peer_status(&self, label: String, status: String) {
        unsafe { cb_str2(table_fn!(self, on_roam_peer_status), self.user_data, &label, &status) };
    }
    fn on_roam_sessions(&self, label: String, sessions: Vec<SessionSummary>) {
        let p = c_json(&sessions);
        unsafe {
            let table = &*self.table;
            if let Some(f) = table.on_roam_sessions {
                let l = c_str(&label);
                if !l.is_null() {
                    f(self.user_data, l, p);
                    grouse_string_free(l);
                }
            }
            grouse_string_free(p);
        }
    }
    fn on_peer_new_session(&self, label: String, session_id: String) {
        unsafe { cb_str2(table_fn!(self, on_peer_new_session), self.user_data, &label, &session_id) };
    }
    fn on_active_run(&self, session_id: String, run_id: String) {
        unsafe { cb_str2(table_fn!(self, on_active_run), self.user_data, &session_id, &run_id) };
    }
    fn on_commands(&self, commands: Vec<String>) {
        let p = c_json(&commands);
        unsafe {
            let table = &*self.table;
            if let Some(f) = table.on_commands {
                f(self.user_data, p);
            }
            grouse_string_free(p);
        }
    }
    fn on_session_open_failed(&self, session_id: String, message: String) {
        unsafe {
            cb_str2(
                table_fn!(self, on_session_open_failed),
                self.user_data,
                &session_id,
                &message,
            )
        };
    }
    fn on_tasks(&self, tasks: String) {
        unsafe { cb_str1(table_fn!(self, on_tasks), self.user_data, &tasks) };
    }
    fn on_thread_ready(&self, label: String, session_id: String) {
        unsafe {
            cb_str2(
                table_fn!(self, on_thread_ready),
                self.user_data,
                &label,
                &session_id,
            )
        };
    }
    fn on_advisor_report(&self, session_id: String, text: String) {
        unsafe { cb_str2(table_fn!(self, on_advisor_report), self.user_data, &session_id, &text) };
    }
    fn on_session_opened(&self, session_id: String) {
        unsafe { cb_str1(table_fn!(self, on_session_opened), self.user_data, &session_id) };
    }
    fn on_file(&self, event: crate::attachments::FileEvent) {
        let p = c_json(&event);
        unsafe {
            let table = &*self.table;
            if let Some(f) = table.on_file {
                f(self.user_data, p);
            }
            grouse_string_free(p);
        }
    }
    fn on_thread_timeline(&self, label: String, rows: Vec<TimelineRow>) {
        // JSON, like every other record list over this ABI: the desktop already owns a
        // JSON parser for the item stream, and a per-row callback would be a new table
        // shape for one event.
        let json = serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string());
        unsafe { cb_str2(table_fn!(self, on_thread_timeline), self.user_data, &label, &json) };
    }
    fn on_sensitivity(&self, data: String) {
        unsafe { cb_str1(table_fn!(self, on_sensitivity), self.user_data, &data) };
    }
    fn on_replay_progress(&self, session_id: String, loaded: i64, total: i64, done: bool) {
        unsafe {
            cb_replay_progress(
                table_fn!(self, on_replay_progress),
                self.user_data,
                &session_id,
                loaded,
                total,
                done,
            )
        };
    }
}

impl GrouseUnstableListener for CoreCallbackForwarder {
    fn on_export(&self, data: String) {
        unsafe { cb_str1(table_fn!(self, on_export), self.user_data, &data) };
    }
    fn on_recipe_params(&self, owner: SessionOwner, request_id: String, parameters: String) {
        // The request id is the ONLY key that answers this exact server request;
        // the legacy single-string slot cannot carry it and is left unfired.
        let owner_json = serde_json::to_string(&owner).unwrap_or_default();
        unsafe {
            cb_str3(table_fn!(self, on_recipe_params_request), self.user_data,
                    &owner_json, &request_id, &parameters);
        }
    }
    fn on_elicitation(&self, owner: SessionOwner, request_id: String, schema: String) {
        let owner_json = serde_json::to_string(&owner).unwrap_or_default();
        unsafe {
            cb_str3(table_fn!(self, on_elicitation_request), self.user_data,
                    &owner_json, &request_id, &schema);
        }
    }
    fn on_compaction_status(&self, message: String) {
        unsafe { cb_str1(table_fn!(self, on_compaction_status), self.user_data, &message) };
    }
    fn on_usage(&self, used: i64, size: i64, cost: f64) {
        unsafe {
            let table = &*self.table;
            if let Some(f) = table.on_unstable_usage {
                f(self.user_data, used, size, cost);
            }
        }
    }
    fn on_message_usage(&self, output_tokens: u64, elapsed_ms: u64, time_to_first_token_ms: u64, cost: f64) {
        unsafe {
            let table = &*self.table;
            if let Some(f) = table.on_message_usage {
                f(self.user_data, output_tokens, elapsed_ms, time_to_first_token_ms, cost);
            }
        }
    }
    fn on_app_resource(&self, key: String, html: String) {
        unsafe { cb_str2(table_fn!(self, on_app_resource), self.user_data, &key, &html) };
    }
    fn on_recipes(&self, recipes: String) {
        unsafe { cb_str1(table_fn!(self, on_recipes), self.user_data, &recipes) };
    }
    fn on_schedules(&self, schedules: String) {
        unsafe { cb_str1(table_fn!(self, on_schedules), self.user_data, &schedules) };
    }
    fn on_projects(&self, projects: String) {
        unsafe { cb_str1(table_fn!(self, on_unstable_projects), self.user_data, &projects) };
    }
    fn on_skills(&self, skills: String) {
        unsafe { cb_str1(table_fn!(self, on_skills), self.user_data, &skills) };
    }
    fn on_tools(&self, session_id: String, tools: String) {
        unsafe { cb_str2(table_fn!(self, on_tools), self.user_data, &session_id, &tools) };
    }
    fn on_extensions(&self, extensions: String) {
        unsafe { cb_str1(table_fn!(self, on_extensions), self.user_data, &extensions) };
    }
    fn on_session_extensions(&self, session_id: String, extensions: String) {
        unsafe { cb_str2(table_fn!(self, on_session_extensions), self.user_data, &session_id, &extensions) };
    }
    fn on_extension_catalog(&self, session_id: String, extensions: String) {
        unsafe { cb_str2(table_fn!(self, on_extension_catalog), self.user_data, &session_id, &extensions) };
    }
    fn on_context_report(&self, session_id: String, report: String) {
        unsafe { cb_str2(table_fn!(self, on_context_report), self.user_data, &session_id, &report) };
    }
    fn on_config_value(&self, key: String, value: String) {
        unsafe { cb_str2(table_fn!(self, on_config_value), self.user_data, &key, &value) };
    }
    fn on_supported_models(&self, provider: String, models: String) {
        unsafe { cb_str2(table_fn!(self, on_supported_models), self.user_data, &provider, &models) };
    }
    fn on_providers(&self, providers: String) {
        unsafe { cb_str1(table_fn!(self, on_providers), self.user_data, &providers) };
    }
    fn on_session_probe(&self, session_id: String, updated_at: String, message_count: i64) {
        unsafe { cb_str2_i64(table_fn!(self, on_session_probe), self.user_data, &session_id, &updated_at, message_count) };
    }
    fn on_tool_result(&self, text: String, is_error: bool) {
        unsafe { cb_str_i32(table_fn!(self, on_tool_result), self.user_data, &text, is_error as i32) };
    }
    fn on_dictation_config(&self, providers: String) {
        unsafe { cb_str1(table_fn!(self, on_dictation_config), self.user_data, &providers) };
    }
    fn on_transcript(&self, text: String) {
        unsafe { cb_str1(table_fn!(self, on_transcript), self.user_data, &text) };
    }
    fn on_error(&self, method: String, message: String) {
        unsafe { cb_str2(table_fn!(self, on_error), self.user_data, &method, &message) };
    }
}

// ---------------------------------------------------------------------------
// Handle + lifecycle
// ---------------------------------------------------------------------------

/// The opaque handle: the stable Core and the unstable shim, kept alive
/// together for the handle's lifetime.
struct CoreHandle {
    core: Arc<Core>,
    unstable: Arc<GrouseUnstable>,
}

fn handle<'a>(h: *mut c_void) -> &'a CoreHandle {
    unsafe { &*(h as *const CoreHandle) }
}

/// Construct the core with the installed listener table. `cache_dir` NULL →
/// the core's default data dir. Returns the opaque handle, or NULL with
/// `out_err` set.
#[no_mangle]
pub extern "C" fn grouse_core_create(
    listener: *const GrouseCoreListener,
    user_data: *mut c_void,
    cache_dir: *const c_char,
    out_err: *mut *mut c_char,
) -> *mut c_void {
    if !out_err.is_null() {
        unsafe { *out_err = std::ptr::null_mut() };
    }
    if listener.is_null() {
        set_err(out_err, "listener table is null");
        return std::ptr::null_mut();
    }
    let cache_dir = unsafe { c_param(cache_dir) }
        .map(str::to_owned)
        .unwrap_or_default();
    catch_unwind(AssertUnwindSafe(|| {
        let core = Core::new(
            Box::new(CoreCallbackForwarder {
                table: listener,
                user_data,
            }),
            cache_dir,
        );
        let unstable = GrouseUnstable::new(Box::new(CoreCallbackForwarder {
            table: listener,
            user_data,
        }));
        Box::into_raw(Box::new(CoreHandle { core, unstable })) as *mut c_void
    }))
    .unwrap_or_else(|_| {
        set_err(out_err, "panic constructing grouse core");
        std::ptr::null_mut()
    })
}

/// Release the handle.
#[no_mangle]
pub extern "C" fn grouse_core_free(h: *mut c_void) {
    if h.is_null() {
        return;
    }
    unsafe { drop(Box::from_raw(h as *mut CoreHandle)) };
}

// ---------------------------------------------------------------------------
// Stable intents (CONTRACT §3.1) — fire-and-forget into the core's runtime.
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn grouse_connect(h: *mut c_void, config_json: *const c_char, out_err: *mut *mut c_char) {
    if !out_err.is_null() {
        unsafe { *out_err = std::ptr::null_mut() };
    }
    let Some(config) = c_json_in::<ServerConfig>(config_json) else {
        set_err(out_err, "invalid ServerConfig JSON");
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).core.connect(config))).unwrap_or_else(|_| {
        set_err(out_err, "panic in grouse_connect");
    });
}

/// Bring the MAIN wire up and bind NOTHING (see `Core::connect_attach`). For a
/// reconnect whose screen belongs to a roam peer — or a landing page with no
/// chat — resuming the last main chat would repaint it over the current one.
#[no_mangle]
pub extern "C" fn grouse_connect_attach(h: *mut c_void, config_json: *const c_char, out_err: *mut *mut c_char) {
    if !out_err.is_null() {
        unsafe { *out_err = std::ptr::null_mut() };
    }
    let Some(config) = c_json_in::<ServerConfig>(config_json) else {
        set_err(out_err, "invalid ServerConfig JSON");
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).core.connect_attach(config))).unwrap_or_else(|_| {
        set_err(out_err, "panic in grouse_connect_attach");
    });
}

#[no_mangle]
pub extern "C" fn grouse_disconnect(h: *mut c_void) {
    catch_unwind(AssertUnwindSafe(|| handle(h).core.disconnect())).ok();
}

#[no_mangle]
pub extern "C" fn grouse_new_session(
    h: *mut c_void,
    recipe_id: *const c_char,
    project_id: *const c_char,
    out_err: *mut *mut c_char,
) {
    if !out_err.is_null() {
        unsafe { *out_err = std::ptr::null_mut() };
    }
    let recipe_id = unsafe { c_param(recipe_id) }.map(str::to_owned);
    let project_id = unsafe { c_param(project_id) }.map(str::to_owned);
    catch_unwind(AssertUnwindSafe(|| {
        handle(h).core.new_session(recipe_id, project_id)
    }))
    .unwrap_or_else(|_| {
        set_err(out_err, "panic in grouse_new_session");
    });
}

/// `grouse_new_session`, plus a mode tag written at creation (`grouse.mode`:
/// `dev` / `plan`).
///
/// A separate symbol rather than a fourth argument: the C ABI is a hand-written
/// mirror of the uniffi surface and the deployed client links against the old
/// signature. `mode` may be null; an unknown value is dropped by the core, so a
/// typo cannot produce a tag nothing filters on.
#[no_mangle]
pub extern "C" fn grouse_new_session_with_mode(
    h: *mut c_void,
    recipe_id: *const c_char,
    project_id: *const c_char,
    mode: *const c_char,
    out_err: *mut *mut c_char,
) {
    if !out_err.is_null() {
        unsafe { *out_err = std::ptr::null_mut() };
    }
    let recipe_id = unsafe { c_param(recipe_id) }.map(str::to_owned);
    let project_id = unsafe { c_param(project_id) }.map(str::to_owned);
    let mode = unsafe { c_param(mode) }.map(str::to_owned);
    catch_unwind(AssertUnwindSafe(|| {
        handle(h).core.new_session_with_mode(recipe_id, project_id, mode)
    }))
    .unwrap_or_else(|_| {
        set_err(out_err, "panic in grouse_new_session_with_mode");
    });
}

#[no_mangle]
pub extern "C" fn grouse_open_session(h: *mut c_void, session_id: *const c_char) {
    let Some(session_id) = (unsafe { c_param(session_id) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).core.open_session(session_id))).ok();
}

/// Reload `session_id` (the client's "refresh chat" action). `full == 0` is a
/// QUICK refresh (freshness decides: suppress / bounded tail / full replay);
/// nonzero forces a full replay regardless (grouse#18). Either way the replay is
/// ingested silently and painted once, newest window first.
#[no_mangle]
pub extern "C" fn grouse_refresh_session(h: *mut c_void, session_id: *const c_char, full: i32) {
    let Some(session_id) = (unsafe { c_param(session_id) }).map(str::to_owned) else { return };
    // `full != 0` = RefreshMode::Full; the enum is a plain discriminant
    // (grouse#18). 0 = Quick.
    let mode = if full != 0 { RefreshMode::Full } else { RefreshMode::Quick };
    catch_unwind(AssertUnwindSafe(|| handle(h).core.refresh_session(session_id, mode))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_list_sessions(h: *mut c_void) {
    catch_unwind(AssertUnwindSafe(|| handle(h).core.list_sessions())).ok();
}

#[no_mangle]
pub extern "C" fn grouse_load_cached_transcript(h: *mut c_void, session_id: *const c_char) {
    let Some(session_id) = (unsafe { c_param(session_id) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).core.load_cached_transcript(session_id))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_flush_caches(h: *mut c_void) {
    catch_unwind(AssertUnwindSafe(|| handle(h).core.flush_caches())).ok();
}

#[no_mangle]
pub extern "C" fn grouse_send_prompt(h: *mut c_void, prompt_json: *const c_char, expect_json: *const c_char, out_err: *mut *mut c_char) {
    if !out_err.is_null() {
        unsafe { *out_err = std::ptr::null_mut() };
    }
    let Some(prompt) = c_json_in::<Prompt>(prompt_json) else {
        set_err(out_err, "invalid Prompt JSON");
        return;
    };
    // `expect` is optional; an invalid (or absent) SendExpect degrades to None
    // (the core accepts a prompt without a bound-session check).
    let expect: Option<SendExpect> = if expect_json.is_null() {
        None
    } else {
        c_json_in::<SendExpect>(expect_json)
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).core.send_prompt(prompt, expect))).unwrap_or_else(|_| {
        set_err(out_err, "panic in grouse_send_prompt");
    });
}

/// `send_prompt` with attachments, named as local paths.
///
/// The desktop shares this machine's filesystem, so it hands over paths and the
/// core opens them; that keeps the ABI addition to one symbol instead of a
/// callback vtable. The upload itself is identical to the uniffi path -- files
/// go to the server chunked and the prompt carries a `resource_link` each,
/// because goose drops a `resource` block whose contents are a blob.
///
/// `paths_json` is a JSON array of strings. `out_err` is set (and must be freed
/// with [`grouse_string_free`]) on failure; an unopenable path aborts the send.
#[no_mangle]
pub extern "C" fn grouse_send_prompt_with_paths(
    h: *mut c_void,
    prompt_json: *const c_char,
    paths_json: *const c_char,
    expect_json: *const c_char,
    out_err: *mut *mut c_char,
) {
    if !out_err.is_null() {
        unsafe { *out_err = std::ptr::null_mut() };
    }
    let Some(prompt) = c_json_in::<Prompt>(prompt_json) else {
        set_err(out_err, "invalid Prompt JSON");
        return;
    };
    let Some(paths) = c_json_in::<Vec<String>>(paths_json) else {
        set_err(out_err, "invalid paths JSON");
        return;
    };
    let expect: Option<SendExpect> = if expect_json.is_null() {
        None
    } else {
        c_json_in::<SendExpect>(expect_json)
    };
    catch_unwind(AssertUnwindSafe(|| {
        handle(h).core.send_prompt_with_paths(prompt, paths, expect)
    }))
    .unwrap_or_else(|_| {
        set_err(out_err, "panic in grouse_send_prompt_with_paths");
    });
}

/// Fetch a file the server shared into this client's cache.
///
/// The other half of an `ItemKind::File` row: the row carries a reference
/// (`files[0].path` + `sha256`), this pulls the bytes in chunks and reports on
/// `on_file`. Idempotent by digest, so a second tap costs no round trip.
#[no_mangle]
pub extern "C" fn grouse_fetch_file(
    h: *mut c_void,
    session_id: *const c_char,
    path: *const c_char,
    name: *const c_char,
    mime: *const c_char,
    sha256: *const c_char,
    out_err: *mut *mut c_char,
) {
    if !out_err.is_null() {
        unsafe { *out_err = std::ptr::null_mut() };
    }
    let (Some(session_id), Some(path), Some(name)) = (
        unsafe { c_param(session_id) }.map(str::to_string),
        unsafe { c_param(path) }.map(str::to_string),
        unsafe { c_param(name) }.map(str::to_string),
    ) else {
        set_err(out_err, "session_id, path and name are required");
        return;
    };
    let mime = unsafe { c_param(mime) }.unwrap_or_default().to_string();
    let sha256 = unsafe { c_param(sha256) }.unwrap_or_default().to_string();
    let core = handle(h);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        core.core.fetch_file(session_id, path, name, mime, sha256);
    }));
    if result.is_err() {
        set_err(out_err, "panic in grouse_fetch_file");
    }
}

#[no_mangle]
pub extern "C" fn grouse_cancel(h: *mut c_void) {
    catch_unwind(AssertUnwindSafe(|| handle(h).core.cancel())).ok();
}

/// Outbound image rescale (CONTRACT §8.1): every image the UI attaches is
/// downscaled to imgrescale::MAX_EDGE before it reaches the wire, the echoed
/// row and the transcript. A UI preference, persisted client-side; the core
/// applies it. Default ON.
#[no_mangle]
pub extern "C" fn grouse_set_image_rescale(h: *mut c_void, enabled: i32) {
    catch_unwind(AssertUnwindSafe(|| {
        handle(h).core.set_image_rescale(enabled != 0);
    }))
    .ok();
}

#[no_mangle]
pub extern "C" fn grouse_image_rescale(h: *mut c_void) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        handle(h).core.image_rescale() as i32
    }))
    .unwrap_or(1)
}

/// My Day (CONTRACT §3.3): the task lists come back on the `on_tasks` callback,
/// `state` is `done` | `dropped` | `expired`, and `resolution` is the one line
/// that keeps the next briefing from re-raising the item.
#[no_mangle]
pub extern "C" fn grouse_tasks_refresh(h: *mut c_void) {
    catch_unwind(AssertUnwindSafe(|| handle(h).core.tasks_refresh())).ok();
}

/// The active chat's BACKGROUND work lanes (CONTRACT §3.7): its delegate/sub-agent
/// runs, as a JSON array of `ActiveWork`. An ordinary tool call is a step of the turn,
/// not a lane, however long it runs. Empty array when nothing is open.
#[no_mangle]
pub extern "C" fn grouse_activity(h: *mut c_void) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let lanes = handle(h).core.activity();
        c_str(&serde_json::to_string(&lanes).unwrap_or_else(|_| "[]".to_string()))
    }))
    .unwrap_or(std::ptr::null_mut())
}

/// Paint the cached board (marked `cached: true` on `on_tasks`) before refreshing:
/// 1 when something was painted, 0 when there is no cache. The client calls this
/// on every open, and `grouse_tasks_refresh` only when it has a wire.
#[no_mangle]
pub extern "C" fn grouse_tasks_paint(h: *mut c_void) -> i32 {
    catch_unwind(AssertUnwindSafe(|| handle(h).core.tasks_paint() as i32)).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn grouse_task_close(
    h: *mut c_void,
    id: *const c_char,
    state: *const c_char,
    resolution: *const c_char,
) {
    // SAFETY: the caller owns these C strings for the duration of the call.
    let (id, state, resolution) = unsafe { (c_param(id), c_param(state), c_param(resolution)) };
    let (Some(id), Some(state), Some(resolution)) = (id, state, resolution) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| {
        handle(h)
            .core
            .task_close(id.to_string(), state.to_string(), resolution.to_string())
    }))
    .ok();
}

#[no_mangle]
pub extern "C" fn grouse_task_snooze(
    h: *mut c_void,
    id: *const c_char,
    until: *const c_char,
    reason: *const c_char,
) {
    let (id, until, reason) = unsafe { (c_param(id), c_param(until), c_param(reason)) };
    let (Some(id), Some(until), Some(reason)) = (id, until, reason) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| {
        handle(h)
            .core
            .task_snooze(id.to_string(), until.to_string(), reason.to_string())
    }))
    .ok();
}

#[no_mangle]
pub extern "C" fn grouse_task_react(h: *mut c_void, id: *const c_char, reaction: *const c_char) {
    let (id, reaction) = unsafe { (c_param(id), c_param(reaction)) };
    let (Some(id), Some(reaction)) = (id, reaction) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| {
        handle(h).core.task_react(id.to_string(), reaction.to_string())
    }))
    .ok();
}

/// The project slug thread chats are filed under (CONTRACT §3.6), e.g.
/// `"assistant"`. Caller frees with `grouse_string_free`.
///
/// Exported instead of documented as a literal so a client cannot file thread
/// chats under a slug the core's own session-list filter stops recognising: the
/// filing and the hiding are one rule, and they have to move together.
#[no_mangle]
pub extern "C" fn grouse_assistant_project() -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| c_str(crate::session::ASSISTANT_PROJECT)))
        .unwrap_or(std::ptr::null_mut())
}

/// Open (or create) a thread's own conversation — the mapping lives on the server,
/// so both clients reach the SAME session for a label (CONTRACT §3.6).
#[no_mangle]
pub extern "C" fn grouse_open_thread(h: *mut c_void, label: *const c_char) {
    let Some(label) = (unsafe { c_param(label) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).core.open_thread(label))).ok();
}

/// The thread label of the conversation on screen ("" when it is not a thread chat),
/// so a client needs no private copy of the map to know which area it is looking at.
#[no_mangle]
pub extern "C" fn grouse_current_thread_label(h: *mut c_void) -> *mut c_char {
    let out = catch_unwind(AssertUnwindSafe(|| handle(h).core.current_thread_label()))
        .unwrap_or_default();
    c_str(&out)
}

#[no_mangle]
pub extern "C" fn grouse_set_config_option(h: *mut c_void, config_id: *const c_char, value: *const c_char, out_err: *mut *mut c_char) {
    if !out_err.is_null() {
        unsafe { *out_err = std::ptr::null_mut() };
    }
    let (Some(config_id), Some(value)) = (
        (unsafe { c_param(config_id) }).map(str::to_owned),
        (unsafe { c_param(value) }).map(str::to_owned),
    ) else {
        set_err(out_err, "config_id/value is null");
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).core.set_config_option(config_id, value))).unwrap_or_else(|_| {
        set_err(out_err, "panic in grouse_set_config_option");
    });
}

#[no_mangle]
pub extern "C" fn grouse_rename_session(h: *mut c_void, id: *const c_char, title: *const c_char) {
    let (Some(id), Some(title)) = (
        (unsafe { c_param(id) }).map(str::to_owned),
        (unsafe { c_param(title) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).core.rename_session(id, title))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_archive_session(h: *mut c_void, id: *const c_char) {
    let Some(id) = (unsafe { c_param(id) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).core.archive_session(id))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unarchive_session(h: *mut c_void, id: *const c_char) {
    let Some(id) = (unsafe { c_param(id) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).core.unarchive_session(id))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_delete_session(h: *mut c_void, id: *const c_char) {
    let Some(id) = (unsafe { c_param(id) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).core.delete_session(id))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_respond_permission(h: *mut c_void, tool_call_id: *const c_char, outcome_json: *const c_char) {
    let Some(tool_call_id) = (unsafe { c_param(tool_call_id) }).map(str::to_owned) else { return };
    let Some(outcome) = c_json_in::<PermissionOutcome>(outcome_json) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).core.respond_permission(tool_call_id, outcome))).ok();
}

// ---------------------------------------------------------------------------
// Stable getters (CONTRACT §3.3) — synchronous JSON snapshots, NULL on error.
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn grouse_status(h: *mut c_void) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| c_json(&handle(h).core.status()))).unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn grouse_ready(h: *mut c_void) -> i32 {
    catch_unwind(AssertUnwindSafe(|| handle(h).core.ready() as i32)).unwrap_or(0)
}

/// Is the MAIN wire up? Unlike `grouse_ready` this is true after an attach,
/// where no session is bound — it answers "may I reuse this connection?".
#[no_mangle]
pub extern "C" fn grouse_connected(h: *mut c_void) -> i32 {
    catch_unwind(AssertUnwindSafe(|| handle(h).core.connected() as i32)).unwrap_or(0)
}

/// Rewrite each tool run's label with a short model-written line (issue #13).
#[no_mangle]
pub extern "C" fn grouse_set_run_summaries(h: *mut c_void, on: i32) {
    catch_unwind(AssertUnwindSafe(|| handle(h).core.set_run_summaries(on != 0))).ok();
}

/// Whether the model rewrites run labels (issue #13).
#[no_mangle]
pub extern "C" fn grouse_run_summaries(h: *mut c_void) -> i32 {
    catch_unwind(AssertUnwindSafe(|| handle(h).core.run_summaries() as i32)).unwrap_or(0)
}

/// Review every dev-session turn on a hidden session (the Dev settings switch).
#[no_mangle]
pub extern "C" fn grouse_set_advisor(h: *mut c_void, on: i32) {
    catch_unwind(AssertUnwindSafe(|| handle(h).core.set_advisor(on != 0))).ok();
}

/// Whether dev-session turns are reviewed (the switch's state, for a picker that
/// re-reads it on open).
#[no_mangle]
pub extern "C" fn grouse_advisor(h: *mut c_void) -> i32 {
    catch_unwind(AssertUnwindSafe(|| handle(h).core.advisor() as i32)).unwrap_or(0)
}

/// NULL when no session is active (the caller treats it as "none").
#[no_mangle]
pub extern "C" fn grouse_active_session_id(h: *mut c_void) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        handle(h)
            .core
            .active_session_id()
            .map(|s| c_str(&s))
            .unwrap_or(std::ptr::null_mut())
    }))
    .unwrap_or(std::ptr::null_mut())
}

/// The core's turn log (see `Core::turn_log`): one plain-text line per
/// lifecycle event, panics last. Debug output, not a record — the UI must gate
/// it behind an explicit switch (both clients default it OFF), because it is
/// unescaped internal state and says nothing a normal user should read.
#[no_mangle]
pub extern "C" fn grouse_turn_log(h: *mut c_void) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| c_str(&handle(h).core.turn_log())))
        .unwrap_or(std::ptr::null_mut())
}

/// One chat's turn state for a UI that has gone quiet (`Core::turn_diag`):
/// whose turn is live, the run id, and how long the wire has been idle.
#[no_mangle]
pub extern "C" fn grouse_turn_diag(h: *mut c_void, session_id: *const c_char) -> *mut c_char {
    let Some(sid) = (unsafe { c_param(session_id) }) else {
        return std::ptr::null_mut();
    };
    catch_unwind(AssertUnwindSafe(|| c_str(&handle(h).core.turn_diag(sid.to_owned()))))
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn grouse_sessions(h: *mut c_void) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| c_json(&handle(h).core.sessions()))).unwrap_or(std::ptr::null_mut())
}

/// Which connection owns a display id, as JSON `{"kind": "main"|"roam",
/// "label": ""|<peer label>}`. A nullptr argument or a panic is a NULL return;
/// the caller must free a returned string with `grouse_string_free`.
#[no_mangle]
pub extern "C" fn grouse_session_owner(h: *mut c_void, session_id: *const c_char) -> *mut c_char {
    let Some(sid) = (unsafe { c_param(session_id) }).map(str::to_owned) else {
        return std::ptr::null_mut();
    };
    catch_unwind(AssertUnwindSafe(|| c_json(&handle(h).core.session_owner(sid))))
        .unwrap_or(std::ptr::null_mut())
}

/// The merged session snapshot — `Vec<SessionSummary>` as JSON: every known
/// session across the MAIN wire and the roam peers, with the client-side
/// `roam:<label>:` display ids. Unlike `grouse_sessions` (main wire only), this
/// is the list a sidebar should render; the core resolves each row's owner once
/// instead of every client re-parsing the prefix.
#[no_mangle]
pub extern "C" fn grouse_all_sessions(h: *mut c_void) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| c_json(&handle(h).core.all_sessions())))
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn grouse_transcript(h: *mut c_void) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| c_json(&handle(h).core.transcript()))).unwrap_or(std::ptr::null_mut())
}

/// The rich item snapshot (docs/TRANSCRIPT_MODEL.md): a JSON array of `Item`.
/// The desktop rebuilds its model from this on Clear, so charts / MCP apps /
/// toolgroups survive a replayed or cache-painted transcript.
#[no_mangle]
pub extern "C" fn grouse_transcript_rich(h: *mut c_void) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| c_json(&handle(h).core.rich_transcript())))
        .unwrap_or(std::ptr::null_mut())
}

/// One item by id, as a JSON `Item`; NULL when the id is not held.
#[no_mangle]
pub extern "C" fn grouse_item(h: *mut c_void, id: *const c_char) -> *mut c_char {
    let Some(id) = (unsafe { c_param(id) }).map(str::to_owned) else {
        return std::ptr::null_mut();
    };
    catch_unwind(AssertUnwindSafe(|| {
        handle(h)
            .core
            .item(id)
            .map(|item| c_json(&item))
            .unwrap_or(std::ptr::null_mut())
    }))
    .unwrap_or(std::ptr::null_mut())
}

/// The pagination cursor, as a JSON `TranscriptWindow`.
#[no_mangle]
pub extern "C" fn grouse_window(h: *mut c_void) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| c_json(&handle(h).core.window())))
        .unwrap_or(std::ptr::null_mut())
}

/// Extend the window backward by `count` items (cache-first; see
/// `Core::load_older`). Outcomes arrive as `on_item` ops.
#[no_mangle]
pub extern "C" fn grouse_load_older(h: *mut c_void, count: u32) {
    catch_unwind(AssertUnwindSafe(|| handle(h).core.load_older(count))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_config(h: *mut c_void) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| c_json(&handle(h).core.config()))).unwrap_or(std::ptr::null_mut())
}

// ---------------------------------------------------------------------------
// Roam intents (CONTRACT §6)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn grouse_roam_connect(h: *mut c_void, card: *const c_char, label: *const c_char) {
    let (Some(card), Some(label)) = (
        (unsafe { c_param(card) }).map(str::to_owned),
        (unsafe { c_param(label) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).core.roam_connect(card, label))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_roam_disconnect(h: *mut c_void, label: *const c_char) {
    let Some(label) = (unsafe { c_param(label) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).core.roam_disconnect(label))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_roam_open_session(h: *mut c_void, label: *const c_char, session_id: *const c_char) {
    let (Some(label), Some(session_id)) = (
        (unsafe { c_param(label) }).map(str::to_owned),
        (unsafe { c_param(session_id) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).core.roam_open_session(label, session_id))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_roam_new_session(h: *mut c_void, label: *const c_char) {
    let Some(label) = (unsafe { c_param(label) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).core.roam_new_session(label))).ok();
}

/// New chat on a roam peer in a caller-chosen working dir (long-press on the
/// new-chat button). NULL/empty `cwd` falls back to the config cwd.
/// A roam peer's new chat, in a mode (`grouse.mode`: dev / chat / null).
#[no_mangle]
pub extern "C" fn grouse_roam_new_session_with_mode(
    h: *mut c_void,
    label: *const c_char,
    cwd: *const c_char,
    mode: *const c_char,
) {
    let Some(label) = (unsafe { c_param(label) }).map(str::to_owned) else { return };
    let cwd = unsafe { c_param(cwd) }.map(str::to_owned).unwrap_or_default();
    let mode = unsafe { c_param(mode) }.map(str::to_owned);
    catch_unwind(AssertUnwindSafe(|| {
        handle(h).core.roam_new_session_with_mode(label, cwd, mode)
    }))
    .ok();
}

#[no_mangle]
pub extern "C" fn grouse_roam_new_session_cwd(
    h: *mut c_void,
    label: *const c_char,
    cwd: *const c_char,
) {
    let (Some(label), cwd) = (
        (unsafe { c_param(label) }).map(str::to_owned),
        (unsafe { c_param(cwd) }).map(str::to_owned).unwrap_or_default(),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).core.roam_new_session_in(label, cwd))).ok();
}

// ---------------------------------------------------------------------------
// Roam identity helpers (grouse-roam-core, bundled inside this cdylib)
// ---------------------------------------------------------------------------

/// Generate a fresh iroh secret key, base64. Caller frees with grouse_string_free.
#[no_mangle]
pub extern "C" fn grouse_identity_generate() -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| c_str(&grouse_roam_core::identity_generate())))
        .unwrap_or(std::ptr::null_mut())
}

/// Set the persisted roam identity so wire dials use the key the UI advertises.
/// Blank/null leaves the core's own identity in place.
#[no_mangle]
pub extern "C" fn grouse_set_roam_identity(h: *mut c_void, secret: *const c_char) {
    let s = (unsafe { c_param(secret) }).map(str::to_owned).unwrap_or_default();
    catch_unwind(AssertUnwindSafe(|| handle(h).core.set_roam_identity(s))).ok();
}

/// Hex public key for a secret key. NULL on error (`out_err` set).
#[no_mangle]
pub extern "C" fn grouse_identity_public_key(secret: *const c_char, out_err: *mut *mut c_char) -> *mut c_char {
    if !out_err.is_null() {
        unsafe { *out_err = std::ptr::null_mut() };
    }
    let Some(secret) = (unsafe { c_param(secret) }).map(str::to_owned) else {
        set_err(out_err, "secret is null");
        return std::ptr::null_mut();
    };
    catch_unwind(AssertUnwindSafe(|| match grouse_roam_core::identity_public_key(&secret) {
        Ok(k) => c_str(&k),
        Err(e) => {
            set_err(out_err, &e.to_string());
            std::ptr::null_mut()
        }
    }))
    .unwrap_or(std::ptr::null_mut())
}

/// Fingerprint of a connection card, for the pairing UI. NULL on error.
#[no_mangle]
pub extern "C" fn grouse_card_fingerprint(card: *const c_char, out_err: *mut *mut c_char) -> *mut c_char {
    if !out_err.is_null() {
        unsafe { *out_err = std::ptr::null_mut() };
    }
    let Some(card) = (unsafe { c_param(card) }).map(str::to_owned) else {
        set_err(out_err, "card is null");
        return std::ptr::null_mut();
    };
    catch_unwind(AssertUnwindSafe(|| match grouse_roam_core::card_fingerprint(&card) {
        Ok(f) => c_str(&f),
        Err(e) => {
            set_err(out_err, &e.to_string());
            std::ptr::null_mut()
        }
    }))
    .unwrap_or(std::ptr::null_mut())
}

// ---------------------------------------------------------------------------
// Roam card pins (ABI 20) — the same Core getters/intents the phone reaches
// through uniffi. The core still verifies the pin at dial time; these exports
// exist so a UI can SHOW and manage the pin store instead of guessing it.
// ---------------------------------------------------------------------------

/// One label's pinned card fingerprint as a JSON string (`"..."`) or an EMPTY
/// string when the label was never enrolled — a client's "no pin" check is a
/// simple empty QString, and a JSON `null` would be read back as the literal
/// text "null". Free with `grouse_string_free`.
#[no_mangle]
pub extern "C" fn grouse_roam_card_pin(h: *mut c_void, label: *const c_char) -> *mut c_char {
    let label = (unsafe { c_param(label) }).map(str::to_owned).unwrap_or_default();
    catch_unwind(AssertUnwindSafe(|| match handle(h).core.roam_card_pin(label) {
        Some(fingerprint) => c_json(&fingerprint),
        None => c_str(""),
    }))
    .unwrap_or_else(|_| c_str(""))
}

/// All `{label: fingerprint}` pins as one JSON object string. Free with
/// `grouse_string_free`.
#[no_mangle]
pub extern "C" fn grouse_roam_card_pins(h: *mut c_void) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| c_str(&handle(h).core.roam_card_pins())))
        .unwrap_or_else(|_| c_str("{}"))
}

/// Enroll/replace a label's pin after the user confirmed a re-shared card.
/// Errors go to `out_err` (free with `grouse_string_free`); the core refuses a
/// dial whose card no longer matches.
#[no_mangle]
pub extern "C" fn grouse_roam_card_pin_enroll(
    h: *mut c_void,
    label: *const c_char,
    fingerprint: *const c_char,
    out_err: *mut *mut c_char,
) {
    if !out_err.is_null() {
        unsafe { *out_err = std::ptr::null_mut() };
    }
    let (Some(label), Some(fingerprint)) = (
        (unsafe { c_param(label) }).map(str::to_owned),
        (unsafe { c_param(fingerprint) }).map(str::to_owned),
    ) else {
        set_err(out_err, "label and fingerprint are required");
        return;
    };
    if let Err(e) = catch_unwind(AssertUnwindSafe(|| {
        handle(h).core.roam_card_pin_enroll(label, fingerprint)
    }))
    .unwrap_or_else(|_| Err("panic in grouse_roam_card_pin_enroll".to_string()))
    {
        set_err(out_err, &e);
    }
}

/// Forget a label's pin so the next dial re-enrolls that peer. Errors go to
/// `out_err` (free with `grouse_string_free`).
#[no_mangle]
pub extern "C" fn grouse_roam_card_pin_clear(
    h: *mut c_void,
    label: *const c_char,
    out_err: *mut *mut c_char,
) {
    if !out_err.is_null() {
        unsafe { *out_err = std::ptr::null_mut() };
    }
    let Some(label) = (unsafe { c_param(label) }).map(str::to_owned) else {
        set_err(out_err, "label is required");
        return;
    };
    if let Err(e) = catch_unwind(AssertUnwindSafe(|| handle(h).core.roam_card_pin_clear(label)))
        .unwrap_or_else(|_| Err("panic in grouse_roam_card_pin_clear".to_string()))
    {
        set_err(out_err, &e);
    }
}

// ---------------------------------------------------------------------------
// Unstable intents (CONTRACT §5) — mirror the GrouseUnstable uniffi surface.
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn grouse_unstable_steer(h: *mut c_void, text: *const c_char, expected_run_id: *const c_char) {
    let (Some(text), Some(expected_run_id)) = (
        (unsafe { c_param(text) }).map(str::to_owned),
        (unsafe { c_param(expected_run_id) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.steer(text, expected_run_id))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_export_session(h: *mut c_void, session_id: *const c_char) {
    let Some(session_id) = (unsafe { c_param(session_id) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.export_session(session_id))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_session_info(h: *mut c_void, id: *const c_char) {
    let Some(id) = (unsafe { c_param(id) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.session_info(id))).ok();
}

/// What fills a session's context window if it were prompted now — the reply
/// arrives on `on_context_report` as raw JSON. A server older than goose 1.58
/// answers "method not found" and NOTHING is emitted (see the shim's note).
#[no_mangle]
pub extern "C" fn grouse_unstable_context_report(h: *mut c_void, id: *const c_char) {
    let Some(id) = (unsafe { c_param(id) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.context_report(id))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_session_project(h: *mut c_void, id: *const c_char, project_id: *const c_char) {
    let Some(id) = (unsafe { c_param(id) }).map(str::to_owned) else { return };
    let project_id = (unsafe { c_param(project_id) }).map(str::to_owned);
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.session_project(id, project_id))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_list_tools(h: *mut c_void, session_id: *const c_char) {
    let Some(session_id) = (unsafe { c_param(session_id) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.list_tools(session_id))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_session_extensions_list(h: *mut c_void, sid: *const c_char) {
    let Some(sid) = (unsafe { c_param(sid) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.session_extensions_list(sid))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_session_extensions_add(h: *mut c_void, sid: *const c_char, extension: *const c_char) {
    let (Some(sid), Some(extension)) = (
        (unsafe { c_param(sid) }).map(str::to_owned),
        (unsafe { c_param(extension) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.session_extensions_add(sid, extension))).ok();
}

#[no_mangle]
/// `extension_key` is the session list's `extensionKey` (= global `configKey`),
/// NOT the display name — see CONTRACT.md.
pub extern "C" fn grouse_unstable_session_extensions_remove(h: *mut c_void, sid: *const c_char, extension_key: *const c_char) {
    let (Some(sid), Some(extension_key)) = (
        (unsafe { c_param(sid) }).map(str::to_owned),
        (unsafe { c_param(extension_key) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.session_extensions_remove(sid, extension_key))).ok();
}

/// The extension catalog of the server that OWNS this chat (`config/extensions/list`
/// on the session's connection), delivered on `on_extension_catalog`. Distinct from
/// `grouse_unstable_list_global_extensions`, which always asks the MAIN connection
/// (the settings screen's defaults) — see CONTRACT §5.
#[no_mangle]
pub extern "C" fn grouse_unstable_extension_catalog(h: *mut c_void, sid: *const c_char) {
    let Some(sid) = (unsafe { c_param(sid) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.extension_catalog(sid))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_list_global_extensions(h: *mut c_void) {
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.list_global_extensions())).ok();
}

#[no_mangle]
/// `config_key` is the global list's `configKey`, not the display name (goose renamed
/// the wire param; see CONTRACT.md).
pub extern "C" fn grouse_unstable_set_extension_enabled(h: *mut c_void, config_key: *const c_char, enabled: i32) {
    let Some(config_key) = (unsafe { c_param(config_key) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.set_extension_enabled(config_key, enabled != 0))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_add_extension(h: *mut c_void, extension: *const c_char, enabled: i32) {
    let Some(extension) = (unsafe { c_param(extension) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.add_extension(extension, enabled != 0))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_sources_list(h: *mut c_void, source_type: *const c_char) {
    let Some(source_type) = (unsafe { c_param(source_type) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.sources_list(source_type))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_sources_create(h: *mut c_void, source_type: *const c_char, name: *const c_char, description: *const c_char, content: *const c_char) {
    let (Some(source_type), Some(name), Some(description), Some(content)) = (
        (unsafe { c_param(source_type) }).map(str::to_owned),
        (unsafe { c_param(name) }).map(str::to_owned),
        (unsafe { c_param(description) }).map(str::to_owned),
        (unsafe { c_param(content) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.sources_create(source_type, name, description, content))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_sources_delete(h: *mut c_void, source_type: *const c_char, path: *const c_char) {
    let (Some(source_type), Some(path)) = (
        (unsafe { c_param(source_type) }).map(str::to_owned),
        (unsafe { c_param(path) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.sources_delete(source_type, path))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_sources_update(h: *mut c_void, source_type: *const c_char, path: *const c_char, name: *const c_char, description: *const c_char, content: *const c_char) {
    let (Some(source_type), Some(path), Some(name), Some(description), Some(content)) = (
        (unsafe { c_param(source_type) }).map(str::to_owned),
        (unsafe { c_param(path) }).map(str::to_owned),
        (unsafe { c_param(name) }).map(str::to_owned),
        (unsafe { c_param(description) }).map(str::to_owned),
        (unsafe { c_param(content) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.sources_update(source_type, path, name, description, content))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_config_read(h: *mut c_void, key: *const c_char) {
    let Some(key) = (unsafe { c_param(key) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.config_read(key))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_config_read_all(h: *mut c_void) {
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.config_read_all())).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_config_read_ex(h: *mut c_void, key: *const c_char, is_secret: i32) {
    let Some(key) = (unsafe { c_param(key) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.config_read_ex(key, is_secret != 0))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_config_upsert(h: *mut c_void, key: *const c_char, value: *const c_char) {
    let (Some(key), Some(value)) = (
        (unsafe { c_param(key) }).map(str::to_owned),
        (unsafe { c_param(value) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.config_upsert(key, value))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_config_upsert_ex(h: *mut c_void, key: *const c_char, value: *const c_char, is_secret: i32) {
    let (Some(key), Some(value)) = (
        (unsafe { c_param(key) }).map(str::to_owned),
        (unsafe { c_param(value) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.config_upsert_ex(key, value, is_secret != 0))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_config_remove(h: *mut c_void, key: *const c_char, is_secret: i32) {
    let Some(key) = (unsafe { c_param(key) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.config_remove(key, is_secret != 0))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_supported_models(h: *mut c_void, provider_id: *const c_char) {
    let Some(provider_id) = (unsafe { c_param(provider_id) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.supported_models(provider_id))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_providers_list(h: *mut c_void) {
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.providers_list())).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_resources_read(h: *mut c_void, sid: *const c_char, uri: *const c_char, ext: *const c_char) {
    let (Some(sid), Some(uri), Some(ext)) = (
        (unsafe { c_param(sid) }).map(str::to_owned),
        (unsafe { c_param(uri) }).map(str::to_owned),
        (unsafe { c_param(ext) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.resources_read(sid, uri, ext))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_recipes_list(h: *mut c_void) {
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.recipes_list())).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_recipes_schedule(h: *mut c_void, id: *const c_char, cron_schedule: *const c_char) {
    let Some(id) = (unsafe { c_param(id) }).map(str::to_owned) else { return };
    let cron = (unsafe { c_param(cron_schedule) }).map(str::to_owned);
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.recipes_schedule(id, cron))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_recipes_save(h: *mut c_void, id: *const c_char, recipe: *const c_char) {
    let (Some(id), Some(recipe)) = (
        (unsafe { c_param(id) }).map(str::to_owned),
        (unsafe { c_param(recipe) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.recipes_save(id, recipe))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_recipes_delete(h: *mut c_void, id: *const c_char) {
    let Some(id) = (unsafe { c_param(id) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.recipes_delete(id))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_schedules_list(h: *mut c_void) {
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.schedules_list())).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_schedules_pause(h: *mut c_void, schedule_id: *const c_char) {
    let Some(schedule_id) = (unsafe { c_param(schedule_id) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.schedules_pause(schedule_id))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_schedules_unpause(h: *mut c_void, schedule_id: *const c_char) {
    let Some(schedule_id) = (unsafe { c_param(schedule_id) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.schedules_unpause(schedule_id))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_schedules_run_now(h: *mut c_void, schedule_id: *const c_char) {
    let Some(schedule_id) = (unsafe { c_param(schedule_id) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.schedules_run_now(schedule_id))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_schedules_delete(h: *mut c_void, schedule_id: *const c_char) {
    let Some(schedule_id) = (unsafe { c_param(schedule_id) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.schedules_delete(schedule_id))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_schedules_update(h: *mut c_void, schedule_id: *const c_char, cron: *const c_char) {
    let (Some(schedule_id), Some(cron)) = (
        (unsafe { c_param(schedule_id) }).map(str::to_owned),
        (unsafe { c_param(cron) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.schedules_update(schedule_id, cron))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_working_dir_update(h: *mut c_void, sid: *const c_char, dir: *const c_char) {
    let (Some(sid), Some(dir)) = (
        (unsafe { c_param(sid) }).map(str::to_owned),
        (unsafe { c_param(dir) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.working_dir_update(sid, dir))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_tools_call(h: *mut c_void, sid: *const c_char, name: *const c_char, args_json: *const c_char) {
    let (Some(sid), Some(name), Some(args_json)) = (
        (unsafe { c_param(sid) }).map(str::to_owned),
        (unsafe { c_param(name) }).map(str::to_owned),
        (unsafe { c_param(args_json) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.tools_call(sid, name, args_json))).ok();
}

/// The usable `tools/call`: `extension_name` is goose's REQUIRED
/// `extensionName`, which the deprecated three-argument call had no way to
/// send. The reply arrives on `on_tool_result` as JSON `(text, isError)`.
#[no_mangle]
pub extern "C" fn grouse_unstable_tools_call_named(
    h: *mut c_void,
    sid: *const c_char,
    extension_name: *const c_char,
    name: *const c_char,
    args_json: *const c_char,
) {
    let (Some(sid), Some(extension_name), Some(name), Some(args)) = (
        (unsafe { c_param(sid) }).map(str::to_owned),
        (unsafe { c_param(extension_name) }).map(str::to_owned),
        (unsafe { c_param(name) }).map(str::to_owned),
        (unsafe { c_param(args_json) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| {
        handle(h)
            .unstable
            .tools_call_named(sid, extension_name, name, args)
    }))
    .ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_respond_recipe_params(
    h: *mut c_void,
    request_id: *const c_char,
    action: *const c_char,
    values_json: *const c_char,
) {
    let (Some(request_id), Some(action), Some(values_json)) = (
        (unsafe { c_param(request_id) }).map(str::to_owned),
        (unsafe { c_param(action) }).map(str::to_owned),
        (unsafe { c_param(values_json) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| {
        handle(h).unstable.respond_recipe_params(request_id, action, values_json)
    }))
    .ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_respond_elicitation(
    h: *mut c_void,
    request_id: *const c_char,
    action: *const c_char,
    content_json: *const c_char,
) {
    let (Some(request_id), Some(action), Some(content_json)) = (
        (unsafe { c_param(request_id) }).map(str::to_owned),
        (unsafe { c_param(action) }).map(str::to_owned),
        (unsafe { c_param(content_json) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| {
        handle(h).unstable.respond_elicitation(request_id, action, content_json)
    }))
    .ok();
}

// ---------------------------------------------------------------------------
// Core-side ABI boundary tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod capi_tests {
    use super::*;
    use parking_lot::Mutex;

    /// Serializes tests that touch crate-global spine state.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Records every event the bridge would receive, as a JSON string.
    #[derive(Default)]
    struct RecordingBridge {
        events: Mutex<Vec<String>>,
    }

    extern "C" fn record_event(user_data: *mut c_void, payload: *const c_char) {
        let bridge = unsafe { &*(user_data as *const RecordingBridge) };
        let s = unsafe { c_param(payload) }.unwrap_or("").to_string();
        bridge.events.lock().push(s);
    }

    #[test]
    fn create_handle_getters_and_free_are_sound() {
        let _guard = TEST_LOCK.lock();
        let recording = RecordingBridge::default();
        let user_data = &recording as *const RecordingBridge as *mut c_void;
        let table = GrouseCoreListener {
            on_sessions: Some(record_event),
            ..Default::default()
        };

        let h = grouse_core_create(&table, user_data, std::ptr::null(), std::ptr::null_mut());
        assert!(!h.is_null(), "core handle must be created");

        // Getter surface is sound on a fresh core.
        assert_eq!(grouse_ready(h), 0);
        let sessions = grouse_sessions(h);
        assert!(!sessions.is_null());
        grouse_string_free(sessions);
        let transcript = grouse_transcript(h);
        assert!(!transcript.is_null());
        grouse_string_free(transcript);
        let config = grouse_config(h);
        assert!(!config.is_null());
        grouse_string_free(config);
        let status = grouse_status(h);
        assert!(!status.is_null());
        grouse_string_free(status);
        // No session is active yet.
        assert!(grouse_active_session_id(h).is_null());

        grouse_core_free(h);
    }

    #[test]
    fn null_listener_table_rejected_with_out_err() {
        let mut err: *mut c_char = std::ptr::null_mut();
        let h = grouse_core_create(std::ptr::null(), std::ptr::null_mut(), std::ptr::null(), &mut err);
        assert!(h.is_null());
        assert!(!err.is_null(), "out_err must be populated");
        let msg = unsafe { c_param(err) }.unwrap_or("").to_string();
        assert!(!msg.is_empty());
        grouse_string_free(err);
    }

    #[test]
    fn listener_table_is_defaultable() {
        let t = GrouseCoreListener::default();
        assert!(t.on_status.is_none() && t.on_error.is_none());
    }

    /// The slug a client files thread chats under is the SAME one the list filter
    /// hides them by; a handle of its own is not needed to read it.
    #[test]
    fn assistant_project_is_exported_without_a_handle() {
        let p = grouse_assistant_project();
        assert!(!p.is_null());
        assert_eq!(unsafe { c_param(p) }.unwrap_or(""), crate::session::ASSISTANT_PROJECT);
        assert_eq!(unsafe { c_param(p) }.unwrap_or(""), "assistant");
        grouse_string_free(p);
    }

    #[test]
    fn string_round_trips_through_grouse_string_free() {
        let s = c_str("hello grouse");
        assert!(!s.is_null());
        let back = unsafe { c_param(s) }.unwrap_or("");
        assert_eq!(back, "hello grouse");
        grouse_string_free(s);
    }

    /// The missing C ABI test from the audit: install a recording callback in
    /// EVERY slot of the ABI-20 table, fire every slot, and require each one to
    /// deliver a valid C string (or valid JSON, or a numeric payload). Server
    /// text carries a literal U+0000 on the raw-string paths — the old
    /// `CString::new`-returns-NULL behaviour silently dropped those events.
    mod listener_slots {
        use super::*;

        /// ABI 20's table has 50 entries. The literal below is exhaustive, so
        /// adding a slot breaks this test at compile time; this catches the
        /// other direction (a case that never got written).
        const SLOT_COUNT: usize = 50;

        #[derive(Clone, Copy)]
        enum Payload {
            /// An exact, NUL-sanitized raw string.
            Text(&'static str),
            /// A parseable JSON document containing this marker.
            Json(&'static str),
            /// One of the numeric-only callbacks (recorded as a tagged line).
            Numeric,
        }

        struct SlotCase {
            name: &'static str,
            fire: fn(&CoreCallbackForwarder, &GrouseCoreListener, *mut c_void),
            check: fn(&[String], &'static str),
        }

        fn expect(events: &[String], name: &str, want: &[Payload]) {
            assert_eq!(
                events.len(), want.len(),
                "{name}: expected {} callback arguments, got {events:?}", want.len()
            );
            for (i, (got, expected)) in events.iter().zip(want).enumerate() {
                match expected {
                    Payload::Text(text) => {
                        assert_eq!(got, text, "{name}: argument {i}");
                        assert!(!got.contains('\0'), "{name}: argument {i} is not NUL-sanitized");
                    }
                    Payload::Json(marker) => {
                        let value: serde_json::Value = serde_json::from_str(got)
                            .unwrap_or_else(|e| panic!("{name}: arg {i} is not JSON ({e}): {got:?}"));
                        assert!(
                            value.to_string().contains(marker),
                            "{name}: argument {i} lost its marker: {got:?}"
                        );
                    }
                    Payload::Numeric => assert!(
                        got.starts_with("i64:") || got.starts_with("u64:") || got.starts_with("i32:"),
                        "{name}: argument {i} is not a numeric payload: {got:?}"
                    ),
                }
            }
        }

        fn push(user_data: *mut c_void, value: String) {
            let bridge = unsafe { &*(user_data as *const RecordingBridge) };
            bridge.events.lock().push(value);
        }

        /// Decode with the same `c_param` the bridge uses. NULL/invalid UTF-8
        /// becomes a sentinel an assertion will catch, never a panic across
        /// the `extern "C"` boundary.
        fn push_strings(user_data: *mut c_void, ptrs: &[*const c_char]) {
            for p in ptrs {
                push(user_data, unsafe { c_param(*p) }.unwrap_or("<null>").to_string());
            }
        }

        extern "C" fn rec_1(ud: *mut c_void, a: *const c_char) {
            push_strings(ud, &[a]);
        }
        extern "C" fn rec_2(ud: *mut c_void, a: *const c_char, b: *const c_char) {
            push_strings(ud, &[a, b]);
        }
        extern "C" fn rec_3(ud: *mut c_void, a: *const c_char, b: *const c_char, c: *const c_char) {
            push_strings(ud, &[a, b, c]);
        }
        extern "C" fn rec_usage(
            ud: *mut c_void, used: i64, size: i64, cost: f64, currency: *const c_char,
        ) {
            push(ud, format!("i64:{used},{size}:f64:{cost}"));
            push_strings(ud, &[currency]);
        }
        extern "C" fn rec_replay(
            ud: *mut c_void, session: *const c_char, loaded: i64, total: i64, done: i32,
        ) {
            push(ud, format!("i64:{loaded},{total}:i32:{done}"));
            push_strings(ud, &[session]);
        }
        extern "C" fn rec_probe(
            ud: *mut c_void, a: *const c_char, b: *const c_char, count: i64,
        ) {
            push(ud, format!("i64:{count}"));
            push_strings(ud, &[a, b]);
        }
        extern "C" fn rec_str_i32(ud: *mut c_void, text: *const c_char, error: i32) {
            push(ud, format!("i32:{error}"));
            push_strings(ud, &[text]);
        }
        extern "C" fn rec_nums_u64(
            ud: *mut c_void, a: u64, b: u64, c: u64, cost: f64,
        ) {
            push(ud, format!("u64:{a},{b},{c}:f64:{cost}"));
        }
        extern "C" fn rec_nums_i64(ud: *mut c_void, a: i64, b: i64, cost: f64) {
            push(ud, format!("i64:{a},{b}:f64:{cost}"));
        }

        fn sessions(marker: &str) -> Vec<SessionSummary> {
            vec![SessionSummary {
                id: marker.into(),
                title: "title".into(),
                updated_at: String::new(),
                last_message_snippet: None,
                project_id: None,
                message_count: 0,
                model: String::new(),
                has_recipe: false,
                has_new: false,
                mode: None,
                archived: false,
            }]
        }

        fn owner() -> SessionOwner {
            SessionOwner { kind: "main".into(), label: String::new() }
        }

        /// The two ABI-20 slots that are intentionally never fired by the
        /// forwarder (it uses the request-id variants) still have to be
        /// callable at their pinned offsets, so fire their function pointers
        /// directly with a NUL-bearing string.
        fn fire_direct(
            name: &'static str,
            f: Option<extern "C" fn(*mut c_void, *const c_char)>,
            ud: *mut c_void,
        ) {
            let f = f.unwrap_or_else(|| panic!("{name}: slot is NULL"));
            let s = c_str(&format!("slot_{name}\u{0}tail"));
            f(ud, s);
            grouse_string_free(s);
        }

        const CASES: &[SlotCase] = &[
            SlotCase { name: "on_status", fire: |l, _, _| CoreListener::on_status(l,
                ConnectionStatus::Error { message: "slot_on_status\u{0}tail".into() }),
                check: |e, n| expect(e, n, &[Payload::Json("slot_on_status")]) },
            SlotCase { name: "on_sessions", fire: |l, _, _| CoreListener::on_sessions(l,
                sessions("slot_on_sessions")),
                check: |e, n| expect(e, n, &[Payload::Json("slot_on_sessions")]) },
            SlotCase { name: "on_item", fire: |l, _, _| CoreListener::on_item(l,
                TranscriptOp::AppendText { id: "slot_on_item".into(), chunk: "a\u{0}b".into() }),
                check: |e, n| expect(e, n, &[Payload::Json("slot_on_item")]) },
            SlotCase { name: "on_usage", fire: |l, _, _| CoreListener::on_usage(
                l, 7, 8, 1.5, "USD\u{0}tail".into()),
                check: |e, n| expect(e, n, &[Payload::Numeric, Payload::Text("USD\u{FFFD}tail")]) },
            SlotCase { name: "on_run_ended", fire: |l, _, _| CoreListener::on_run_ended(
                l, "slot_on_run_ended".into(), "stop\u{0}tail".into()),
                check: |e, n| expect(e, n, &[Payload::Text("slot_on_run_ended"),
                    Payload::Text("stop\u{FFFD}tail")]) },
            SlotCase { name: "on_speak", fire: |l, _, _| CoreListener::on_speak(
                l, "slot_on_speak\u{0}tail".into()),
                check: |e, n| expect(e, n, &[Payload::Text("slot_on_speak\u{FFFD}tail")]) },
            SlotCase { name: "on_config", fire: |l, _, _| CoreListener::on_config(l,
                vec![ConfigOption { id: "slot_on_config".into(), value: "v".into(),
                    name: "n".into(), choices: Vec::new() }]),
                check: |e, n| expect(e, n, &[Payload::Json("slot_on_config")]) },
            SlotCase { name: "on_permission_request", fire: |l, _, _|
                CoreListener::on_permission_request(l, PermissionRequest {
                    tool_call_id: "slot_on_permission_request".into(), title: "t".into(),
                    detail: "d".into(), options: Vec::new(), session_id: "s".into(),
                    owner: "main".into() }),
                check: |e, n| expect(e, n, &[Payload::Json("slot_on_permission_request")]) },
            SlotCase { name: "on_session_touched", fire: |l, _, _|
                CoreListener::on_session_touched(l, "session".into(),
                    "slot_on_session_touched\u{0}tail".into(), "stamp".into()),
                check: |e, n| expect(e, n, &[Payload::Text("session"),
                    Payload::Text("slot_on_session_touched\u{FFFD}tail"), Payload::Text("stamp")]) },
            SlotCase { name: "on_projects", fire: |l, _, _| CoreListener::on_projects(l,
                vec![ProjectSummary { path: "slot_on_projects".into(), name: "n".into(),
                    description: None }]),
                check: |e, n| expect(e, n, &[Payload::Json("slot_on_projects")]) },
            SlotCase { name: "on_roam_peer_status", fire: |l, _, _|
                CoreListener::on_roam_peer_status(l, "peer".into(),
                    "slot_on_roam_peer_status\u{0}tail".into()),
                check: |e, n| expect(e, n, &[Payload::Text("peer"),
                    Payload::Text("slot_on_roam_peer_status\u{FFFD}tail")]) },
            SlotCase { name: "on_roam_sessions", fire: |l, _, _|
                CoreListener::on_roam_sessions(l, "peer".into(), sessions("slot_on_roam_sessions")),
                check: |e, n| expect(e, n, &[Payload::Text("peer"),
                    Payload::Json("slot_on_roam_sessions")]) },
            SlotCase { name: "on_peer_new_session", fire: |l, _, _|
                CoreListener::on_peer_new_session(l, "peer".into(),
                    "slot_on_peer_new_session".into()),
                check: |e, n| expect(e, n, &[Payload::Text("peer"),
                    Payload::Text("slot_on_peer_new_session")]) },
            SlotCase { name: "on_active_run", fire: |l, _, _| CoreListener::on_active_run(
                l, "session".into(), "slot_on_active_run".into()),
                check: |e, n| expect(e, n, &[Payload::Text("session"),
                    Payload::Text("slot_on_active_run")]) },
            SlotCase { name: "on_commands", fire: |l, _, _| CoreListener::on_commands(
                l, vec!["slot_on_commands\u{0}tail".into()]),
                check: |e, n| expect(e, n, &[Payload::Json("slot_on_commands")]) },
            SlotCase { name: "on_export", fire: |l, _, _| GrouseUnstableListener::on_export(
                l, "slot_on_export\u{0}tail".into()),
                check: |e, n| expect(e, n, &[Payload::Text("slot_on_export\u{FFFD}tail")]) },
            SlotCase { name: "on_recipe_params", fire: |_, t, ud| fire_direct(
                "on_recipe_params", t.on_recipe_params, ud),
                check: |e, n| expect(e, n, &[Payload::Text("slot_on_recipe_params\u{FFFD}tail")]) },
            SlotCase { name: "on_elicitation", fire: |_, t, ud| fire_direct(
                "on_elicitation", t.on_elicitation, ud),
                check: |e, n| expect(e, n, &[Payload::Text("slot_on_elicitation\u{FFFD}tail")]) },
            SlotCase { name: "on_compaction_status", fire: |l, _, _|
                GrouseUnstableListener::on_compaction_status(l,
                    "slot_on_compaction_status\u{0}tail".into()),
                check: |e, n| expect(e, n,
                    &[Payload::Text("slot_on_compaction_status\u{FFFD}tail")]) },
            SlotCase { name: "on_message_usage", fire: |l, _, _|
                GrouseUnstableListener::on_message_usage(l, 1, 2, 3, 4.5),
                check: |e, n| expect(e, n, &[Payload::Numeric]) },
            SlotCase { name: "on_unstable_usage", fire: |l, _, _|
                GrouseUnstableListener::on_usage(l, 1, 2, 4.5),
                check: |e, n| expect(e, n, &[Payload::Numeric]) },
            SlotCase { name: "on_app_resource", fire: |l, _, _|
                GrouseUnstableListener::on_app_resource(l, "key".into(),
                    "slot_on_app_resource\u{0}tail".into()),
                check: |e, n| expect(e, n, &[Payload::Text("key"),
                    Payload::Text("slot_on_app_resource\u{FFFD}tail")]) },
            SlotCase { name: "on_recipes", fire: |l, _, _| GrouseUnstableListener::on_recipes(
                l, r#"{"marker":"slot_on_recipes","text":"a\u0000b"}"#.into()),
                check: |e, n| expect(e, n, &[Payload::Json("slot_on_recipes")]) },
            SlotCase { name: "on_schedules", fire: |l, _, _| GrouseUnstableListener::on_schedules(
                l, r#"{"marker":"slot_on_schedules"}"#.into()),
                check: |e, n| expect(e, n, &[Payload::Json("slot_on_schedules")]) },
            SlotCase { name: "on_unstable_projects", fire: |l, _, _|
                GrouseUnstableListener::on_projects(l,
                    r#"[{"name":"slot_on_unstable_projects"}]"#.into()),
                check: |e, n| expect(e, n, &[Payload::Json("slot_on_unstable_projects")]) },
            SlotCase { name: "on_skills", fire: |l, _, _| GrouseUnstableListener::on_skills(
                l, "slot_on_skills\u{0}tail".into()),
                check: |e, n| expect(e, n, &[Payload::Text("slot_on_skills\u{FFFD}tail")]) },
            SlotCase { name: "on_tools", fire: |l, _, _| GrouseUnstableListener::on_tools(
                l, "main".into(), r#"[{"name":"slot_on_tools"}]"#.into()),
                check: |e, n| expect(e, n, &[Payload::Text("main"),
                    Payload::Json("slot_on_tools")]) },
            SlotCase { name: "on_extensions", fire: |l, _, _|
                GrouseUnstableListener::on_extensions(l,
                    r#"{"a":"slot_on_extensions"}"#.into()),
                check: |e, n| expect(e, n, &[Payload::Json("slot_on_extensions")]) },
            SlotCase { name: "on_session_extensions", fire: |l, _, _|
                GrouseUnstableListener::on_session_extensions(l, "main".into(),
                    r#"{"a":"slot_on_session_extensions"}"#.into()),
                check: |e, n| expect(e, n, &[Payload::Text("main"),
                    Payload::Json("slot_on_session_extensions")]) },
            SlotCase { name: "on_config_value", fire: |l, _, _|
                GrouseUnstableListener::on_config_value(l, "mode".into(),
                    "slot_on_config_value\u{0}tail".into()),
                check: |e, n| expect(e, n, &[Payload::Text("mode"),
                    Payload::Text("slot_on_config_value\u{FFFD}tail")]) },
            SlotCase { name: "on_supported_models", fire: |l, _, _|
                GrouseUnstableListener::on_supported_models(l, "provider".into(),
                    r#"["slot_on_supported_models"]"#.into()),
                check: |e, n| expect(e, n, &[Payload::Text("provider"),
                    Payload::Json("slot_on_supported_models")]) },
            SlotCase { name: "on_providers", fire: |l, _, _|
                GrouseUnstableListener::on_providers(l,
                    r#"["slot_on_providers"]"#.into()),
                check: |e, n| expect(e, n, &[Payload::Json("slot_on_providers")]) },
            SlotCase { name: "on_session_probe", fire: |l, _, _|
                GrouseUnstableListener::on_session_probe(l, "session".into(),
                    "slot_on_session_probe\u{0}tail".into(), 42),
                check: |e, n| expect(e, n, &[Payload::Numeric, Payload::Text("session"),
                    Payload::Text("slot_on_session_probe\u{FFFD}tail")]) },
            SlotCase { name: "on_tool_result", fire: |l, _, _|
                GrouseUnstableListener::on_tool_result(l,
                    "slot_on_tool_result\u{0}tail".into(), true),
                check: |e, n| expect(e, n, &[Payload::Numeric,
                    Payload::Text("slot_on_tool_result\u{FFFD}tail")]) },
            SlotCase { name: "on_error", fire: |l, _, _| GrouseUnstableListener::on_error(
                l, "method".into(), "slot_on_error\u{0}tail".into()),
                check: |e, n| expect(e, n, &[Payload::Text("method"),
                    Payload::Text("slot_on_error\u{FFFD}tail")]) },
            SlotCase { name: "on_dictation_config", fire: |l, _, _|
                GrouseUnstableListener::on_dictation_config(l,
                    r#"{"provider":"slot_on_dictation_config"}"#.into()),
                check: |e, n| expect(e, n, &[Payload::Json("slot_on_dictation_config")]) },
            SlotCase { name: "on_transcript", fire: |l, _, _|
                GrouseUnstableListener::on_transcript(l,
                    "slot_on_transcript\u{0}tail".into()),
                check: |e, n| expect(e, n, &[Payload::Text("slot_on_transcript\u{FFFD}tail")]) },
            SlotCase { name: "on_session_open_failed", fire: |l, _, _|
                CoreListener::on_session_open_failed(l, "session".into(),
                    "slot_on_session_open_failed\u{0}tail".into()),
                check: |e, n| expect(e, n, &[Payload::Text("session"),
                    Payload::Text("slot_on_session_open_failed\u{FFFD}tail")]) },
            SlotCase { name: "on_tasks", fire: |l, _, _| CoreListener::on_tasks(
                l, "slot_on_tasks\u{0}tail".into()),
                check: |e, n| expect(e, n, &[Payload::Text("slot_on_tasks\u{FFFD}tail")]) },
            SlotCase { name: "on_replay_progress", fire: |l, _, _|
                CoreListener::on_replay_progress(l, "slot_on_replay_progress".into(),
                    3, 9, true),
                check: |e, n| expect(e, n, &[Payload::Numeric,
                    Payload::Text("slot_on_replay_progress")]) },
            SlotCase { name: "on_file", fire: |l, _, _| CoreListener::on_file(l,
                crate::attachments::FileEvent::Failed {
                    path: "slot_on_file".into(), name: "f".into(),
                    message: "boom".into(), session_id: "s".into(), request_id: "r".into(),
                }),
                check: |e, n| expect(e, n, &[Payload::Json("slot_on_file")]) },
            SlotCase { name: "on_thread_ready", fire: |l, _, _|
                CoreListener::on_thread_ready(l, "area".into(), "slot_on_thread_ready".into()),
                check: |e, n| expect(e, n, &[Payload::Text("area"),
                    Payload::Text("slot_on_thread_ready")]) },
            SlotCase { name: "on_extension_catalog", fire: |l, _, _|
                GrouseUnstableListener::on_extension_catalog(l, "main".into(),
                    r#"{"a":"slot_on_extension_catalog"}"#.into()),
                check: |e, n| expect(e, n, &[Payload::Text("main"),
                    Payload::Json("slot_on_extension_catalog")]) },
            SlotCase { name: "on_context_report", fire: |l, _, _|
                GrouseUnstableListener::on_context_report(l, "main".into(),
                    r#"{"text":"slot_on_context_report"}"#.into()),
                check: |e, n| expect(e, n, &[Payload::Text("main"),
                    Payload::Json("slot_on_context_report")]) },
            SlotCase { name: "on_thread_timeline", fire: |l, _, _|
                CoreListener::on_thread_timeline(l, "area".into(),
                    vec![TimelineRow { text: "slot_on_thread_timeline".into(),
                        ..Default::default() }]),
                check: |e, n| expect(e, n, &[Payload::Text("area"),
                    Payload::Json("slot_on_thread_timeline")]) },
            SlotCase { name: "on_sensitivity", fire: |l, _, _| CoreListener::on_sensitivity(
                l, "slot_on_sensitivity\u{0}tail".into()),
                check: |e, n| expect(e, n, &[Payload::Text("slot_on_sensitivity\u{FFFD}tail")]) },
            SlotCase { name: "on_advisor_report", fire: |l, _, _|
                CoreListener::on_advisor_report(l, "session".into(),
                    "slot_on_advisor_report\u{0}tail".into()),
                check: |e, n| expect(e, n, &[Payload::Text("session"),
                    Payload::Text("slot_on_advisor_report\u{FFFD}tail")]) },
            SlotCase { name: "on_session_opened", fire: |l, _, _|
                CoreListener::on_session_opened(l,
                    "slot_on_session_opened\u{0}tail".into()),
                check: |e, n| expect(e, n,
                    &[Payload::Text("slot_on_session_opened\u{FFFD}tail")]) },
            SlotCase { name: "on_recipe_params_request", fire: |l, _, _|
                GrouseUnstableListener::on_recipe_params(l, owner(),
                    "req_slot_on_recipe_params_request".into(),
                    r#"{"marker":"slot_on_recipe_params_request"}"#.into()),
                check: |e, n| expect(e, n, &[Payload::Json("main"),
                    Payload::Text("req_slot_on_recipe_params_request"),
                    Payload::Json("slot_on_recipe_params_request")]) },
            SlotCase { name: "on_elicitation_request", fire: |l, _, _|
                GrouseUnstableListener::on_elicitation(l, owner(),
                    "req_slot_on_elicitation_request".into(),
                    r#"{"marker":"slot_on_elicitation_request"}"#.into()),
                check: |e, n| expect(e, n, &[Payload::Json("main"),
                    Payload::Text("req_slot_on_elicitation_request"),
                    Payload::Json("slot_on_elicitation_request")]) },
        ];

        #[test]
        fn every_listener_slot_delivers_a_valid_payload() {
            let bridge = RecordingBridge::default();
            let user_data = &bridge as *const RecordingBridge as *mut c_void;
            // Exhaustive literal (no `..Default::default()`): the compiler
            // enforces one recorder per slot, and the case count below keeps
            // the table and this test in lockstep.
            let table = GrouseCoreListener {
                on_status: Some(rec_1),
                on_sessions: Some(rec_1),
                on_item: Some(rec_1),
                on_usage: Some(rec_usage),
                on_run_ended: Some(rec_2),
                on_speak: Some(rec_1),
                on_config: Some(rec_1),
                on_permission_request: Some(rec_1),
                on_session_touched: Some(rec_3),
                on_projects: Some(rec_1),
                on_roam_peer_status: Some(rec_2),
                on_roam_sessions: Some(rec_2),
                on_peer_new_session: Some(rec_2),
                on_active_run: Some(rec_2),
                on_commands: Some(rec_1),
                on_export: Some(rec_1),
                on_recipe_params: Some(rec_1),
                on_elicitation: Some(rec_1),
                on_compaction_status: Some(rec_1),
                on_message_usage: Some(rec_nums_u64),
                on_unstable_usage: Some(rec_nums_i64),
                on_app_resource: Some(rec_2),
                on_recipes: Some(rec_1),
                on_schedules: Some(rec_1),
                on_unstable_projects: Some(rec_1),
                on_skills: Some(rec_1),
                on_tools: Some(rec_2),
                on_extensions: Some(rec_1),
                on_session_extensions: Some(rec_2),
                on_config_value: Some(rec_2),
                on_supported_models: Some(rec_2),
                on_providers: Some(rec_1),
                on_session_probe: Some(rec_probe),
                on_tool_result: Some(rec_str_i32),
                on_error: Some(rec_2),
                on_dictation_config: Some(rec_1),
                on_transcript: Some(rec_1),
                on_session_open_failed: Some(rec_2),
                on_tasks: Some(rec_1),
                on_replay_progress: Some(rec_replay),
                on_file: Some(rec_1),
                on_thread_ready: Some(rec_2),
                on_extension_catalog: Some(rec_2),
                on_context_report: Some(rec_2),
                on_thread_timeline: Some(rec_2),
                on_sensitivity: Some(rec_1),
                on_advisor_report: Some(rec_2),
                on_session_opened: Some(rec_1),
                on_recipe_params_request: Some(rec_3),
                on_elicitation_request: Some(rec_3),
            };
            assert_eq!(CASES.len(), SLOT_COUNT, "every ABI slot needs a case");
            let forwarder = CoreCallbackForwarder { table: &table, user_data };
            for case in CASES {
                bridge.events.lock().clear();
                (case.fire)(&forwarder, &table, user_data);
                let events = std::mem::take(&mut *bridge.events.lock());
                (case.check)(&events, case.name);
            }
        }
    }
}
