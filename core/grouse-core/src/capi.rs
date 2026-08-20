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

use std::ffi::{c_char, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use serde::Serialize;

use crate::unstable::GrouseUnstable;
use crate::{
    ConfigOption, ConnectionStatus, Core, CoreListener, GrouseUnstableListener,
    PermissionOutcome, PermissionRequest, ProjectSummary, Prompt, SendExpect, ServerConfig,
    SessionSummary, StreamEvent, TranscriptEvent,
};

/// malloc-allocated UTF-8 copy of `s`; NULL when the input contains an interior
/// NUL byte.
fn c_str(s: &str) -> *mut c_char {
    CString::new(s)
        .map(CString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

unsafe fn c_param<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok()
}

fn set_err(out_err: *mut *mut c_char, msg: &str) {
    if !out_err.is_null() {
        unsafe { *out_err = c_str(msg) };
    }
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
    if !s.is_null() {
        unsafe { drop(CString::from_raw(s)) };
    }
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
    /// `event` serialized as JSON (`TranscriptEvent`).
    pub on_transcript: Option<extern "C" fn(*mut c_void, *const c_char)>,
    /// `event` serialized as JSON (`StreamEvent`).
    pub on_stream: Option<extern "C" fn(*mut c_void, *const c_char)>,
    /// `options` serialized as a JSON array of `ConfigOption`.
    pub on_config: Option<extern "C" fn(*mut c_void, *const c_char)>,
    /// `request` serialized as JSON (`PermissionRequest`).
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
    pub on_recipe_params: Option<extern "C" fn(*mut c_void, *const c_char)>,
    pub on_elicitation: Option<extern "C" fn(*mut c_void, *const c_char)>,
    pub on_compaction_status: Option<extern "C" fn(*mut c_void, *const c_char)>,
    pub on_message_usage:
        Option<extern "C" fn(*mut c_void, u64, u64, u64, f64)>,
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
    fn on_transcript(&self, event: TranscriptEvent) {
        let p = c_json(&event);
        unsafe {
            let table = &*self.table;
            if let Some(f) = table.on_transcript {
                f(self.user_data, p);
            }
            grouse_string_free(p);
        }
    }
    fn on_stream(&self, event: StreamEvent) {
        let p = c_json(&event);
        unsafe {
            let table = &*self.table;
            if let Some(f) = table.on_stream {
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
}

impl GrouseUnstableListener for CoreCallbackForwarder {
    fn on_export(&self, data: String) {
        unsafe { cb_str1(table_fn!(self, on_export), self.user_data, &data) };
    }
    fn on_recipe_params(&self, parameters: String) {
        unsafe { cb_str1(table_fn!(self, on_recipe_params), self.user_data, &parameters) };
    }
    fn on_elicitation(&self, schema: String) {
        unsafe { cb_str1(table_fn!(self, on_elicitation), self.user_data, &schema) };
    }
    fn on_compaction_status(&self, message: String) {
        unsafe { cb_str1(table_fn!(self, on_compaction_status), self.user_data, &message) };
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

#[no_mangle]
pub extern "C" fn grouse_disconnect(h: *mut c_void) {
    catch_unwind(AssertUnwindSafe(|| handle(h).core.disconnect())).ok();
}

#[no_mangle]
pub extern "C" fn grouse_new_session(h: *mut c_void, recipe_id: *const c_char, out_err: *mut *mut c_char) {
    if !out_err.is_null() {
        unsafe { *out_err = std::ptr::null_mut() };
    }
    let recipe_id = unsafe { c_param(recipe_id) }.map(str::to_owned);
    catch_unwind(AssertUnwindSafe(|| handle(h).core.new_session(recipe_id))).unwrap_or_else(|_| {
        set_err(out_err, "panic in grouse_new_session");
    });
}

#[no_mangle]
pub extern "C" fn grouse_open_session(h: *mut c_void, session_id: *const c_char) {
    let Some(session_id) = (unsafe { c_param(session_id) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).core.open_session(session_id))).ok();
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

#[no_mangle]
pub extern "C" fn grouse_cancel(h: *mut c_void) {
    catch_unwind(AssertUnwindSafe(|| handle(h).core.cancel())).ok();
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

#[no_mangle]
pub extern "C" fn grouse_sessions(h: *mut c_void) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| c_json(&handle(h).core.sessions()))).unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn grouse_transcript(h: *mut c_void) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| c_json(&handle(h).core.transcript()))).unwrap_or(std::ptr::null_mut())
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

// ---------------------------------------------------------------------------
// Roam identity helpers (grouse-roam-core, bundled inside this cdylib)
// ---------------------------------------------------------------------------

/// Generate a fresh iroh secret key, base64. Caller frees with grouse_string_free.
#[no_mangle]
pub extern "C" fn grouse_identity_generate() -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| c_str(&grouse_roam_core::identity_generate())))
        .unwrap_or(std::ptr::null_mut())
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
pub extern "C" fn grouse_unstable_session_extensions_remove(h: *mut c_void, sid: *const c_char, name: *const c_char) {
    let (Some(sid), Some(name)) = (
        (unsafe { c_param(sid) }).map(str::to_owned),
        (unsafe { c_param(name) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.session_extensions_remove(sid, name))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_list_global_extensions(h: *mut c_void) {
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.list_global_extensions())).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_set_extension_enabled(h: *mut c_void, name: *const c_char, enabled: i32) {
    let Some(name) = (unsafe { c_param(name) }).map(str::to_owned) else { return };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.set_extension_enabled(name, enabled != 0))).ok();
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

#[no_mangle]
pub extern "C" fn grouse_unstable_respond_recipe_params(h: *mut c_void, action: *const c_char, values_json: *const c_char) {
    let (Some(action), Some(values_json)) = (
        (unsafe { c_param(action) }).map(str::to_owned),
        (unsafe { c_param(values_json) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.respond_recipe_params(action, values_json))).ok();
}

#[no_mangle]
pub extern "C" fn grouse_unstable_respond_elicitation(h: *mut c_void, action: *const c_char, content_json: *const c_char) {
    let (Some(action), Some(content_json)) = (
        (unsafe { c_param(action) }).map(str::to_owned),
        (unsafe { c_param(content_json) }).map(str::to_owned),
    ) else {
        return;
    };
    catch_unwind(AssertUnwindSafe(|| handle(h).unstable.respond_elicitation(action, content_json))).ok();
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

    #[test]
    fn string_round_trips_through_grouse_string_free() {
        let s = c_str("hello grouse");
        assert!(!s.is_null());
        let back = unsafe { c_param(s) }.unwrap_or("");
        assert_eq!(back, "hello grouse");
        grouse_string_free(s);
    }
}
