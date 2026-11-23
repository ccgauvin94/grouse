// SPDX-License-Identifier: AGPL-3.0-or-later

//! grouse-unstable: the goose-fork `_goose/unstable/*` compatibility shim.
//!
//! This crate owns the two pieces that make it a retirement boundary rather
//! than a decorative `pub use`:
//!
//! * [`declare_grouse_unstable_listener`] is the single declaration of the
//!   `GrouseUnstableListener` callback interface. `grouse-core` invokes it where
//!   the trait used to be declared, so the uniffi metadata is still emitted in
//!   the `grouse_core` namespace (uniffi namespaces are per-crate: exporting the
//!   trait directly from here would move it to `uniffi.grouse_unstable` and
//!   break every Kotlin/Swift import). `grouse-core` therefore materializes the
//!   trait at the old declaration site and this crate re-exports that one type,
//!   so `grouse_core::GrouseUnstableListener` and
//!   `grouse_unstable::GrouseUnstableListener` remain the same trait.
//! * [`UNSTABLE_INTENTS`] is the inventory of every public `GrouseUnstable`
//!   method. The disconnected no-op test below is generated from the SAME list,
//!   so adding an intent here automatically adds its contract check.
//!
//! Implementations stay in `grouse-core` (`unstable.rs`). As the GDK absorbs
//! each unstable feature, its method leaves this trait and this inventory, and
//! eventually this crate disappears.

/// Declare the unstable callback interface. Invoked by `grouse-core`'s `lib.rs`
/// at the old declaration site: the tokens (including `#[uniffi::export]`) are
/// expanded there, which keeps the generated bindings byte-compatible with the
/// hand-written trait they replace. Do not invoke this here — that would emit
/// a second, different trait in a different uniffi namespace.
#[macro_export]
macro_rules! declare_grouse_unstable_listener {
    () => {
        /// Unstable events (CONTRACT §5). Retiring with `grouse-unstable`.
        #[uniffi::export(callback_interface)]
        pub trait GrouseUnstableListener: Send + Sync {
            fn on_export(&self, data: String);
            /// A recipe's declared parameters, keyed by the SERVER's request id. The
            /// owner is the `SessionOwner` record of the wire that asked, so the answer
            /// goes to that connection rather than whichever chat is on screen; pass
            /// `request_id` back as the first argument of `respond_recipe_params`.
            fn on_recipe_params(&self, owner: SessionOwner, request_id: String, parameters: String);
            /// An `elicitation/create` form, keyed the same way; answer with
            /// `respond_elicitation(request_id, action, content)`.
            fn on_elicitation(&self, owner: SessionOwner, request_id: String, schema: String);
            fn on_compaction_status(&self, message: String);
            /// A context-window update that arrived over the unstable channel
            /// (`usage_update`): used/limit tokens plus the accumulated session cost.
            /// Roam transports ONLY these custom notifications, so this is how a roam
            /// session's context bar stays live; the core already gates each callback
            /// to the displayed session.
            fn on_usage(&self, used: i64, size: i64, cost: f64);
            fn on_message_usage(
                &self,
                output_tokens: u64,
                elapsed_ms: u64,
                time_to_first_token_ms: u64,
                cost: f64,
            );
            fn on_app_resource(&self, key: String, html: String);
            // List replies ride raw-JSON strings (the shim's on_export convention):
            // the UI keeps its existing parsers. Added with the GrouseUnstable
            // implementation (CONTRACT §5, flagged: the unstable surface is retiring).
            fn on_recipes(&self, recipes: String);
            fn on_schedules(&self, schedules: String);
            fn on_projects(&self, projects: String);
            fn on_skills(&self, skills: String);
            fn on_tools(&self, session_id: String, tools: String);
            fn on_extensions(&self, extensions: String);
            fn on_session_extensions(&self, session_id: String, extensions: String);
            /// The extension CATALOG of the server that owns this chat (same shape as
            /// `on_extensions`, which is the MAIN server's config.yaml for the settings screen).
            /// Keyed by session: a roam chat's catalog lives on its peer, and a chat's sheet
            /// needs the rows it does NOT have to offer them.
            fn on_extension_catalog(&self, session_id: String, extensions: String);
            fn on_config_value(&self, key: String, value: String);
            fn on_supported_models(&self, provider: String, models: String);
            /// The server's provider inventory (`_goose/unstable/providers/list`): the
            /// catalog, which entries are configured, and each one's models. Raw JSON
            /// array of ProviderInventoryEntryDto.
            fn on_providers(&self, providers: String);
            fn on_session_probe(&self, session_id: String, updated_at: String, message_count: i64);
            /// What fills a chat's context window if it were prompted right now
            /// (`_goose/unstable/context/report`, goose ≥1.58), as raw JSON: the model and
            /// its limit, `totalTokens`, `segments` (category / label / source / tokenCount /
            /// contentPreview / parts) and `autoCompactionPending`. It is the SERVER's own
            /// attribution of the next request and makes no model call, so it is a better
            /// answer than the donut's provider-billed last-request number — and it will not
            /// agree with it (they measure different requests). Session-scoped, routed to the
            /// owning peer.
            fn on_context_report(&self, session_id: String, report: String);
            fn on_tool_result(&self, text: String, is_error: bool);
            /// Dictation provider status (`_goose/unstable/dictation/config`): which
            /// speech-to-text providers the server has configured, as a raw JSON map of
            /// provider name → status entry (the shim's list-reply convention).
            fn on_dictation_config(&self, providers: String);
            /// A dictation transcript (`_goose/unstable/dictation/transcribe`): the text
            /// decoded from the audio clip the UI sent; empty when the clip was silent.
            fn on_transcript(&self, text: String);
            fn on_error(&self, method: String, message: String);
        }
    };
}

/// One entry per public `GrouseUnstable` INTENT (the constructor is not one).
///
/// The name and the call that exercises it live in the SAME list, so there is
/// no second hand-maintained switch that can fall behind. `UNSTABLE_INTENTS`
/// and the no-op harness are both generated from this one macro invocation.
macro_rules! unstable_intents {
    ($(($name:literal, $call:expr)),+ $(,)?) => {
        /// Every unstable intent that crosses the FFI, by method name.
        /// `grouse-core` re-exports this as `crate::UNSTABLE_INTENTS`.
        pub const UNSTABLE_INTENTS: &[&str] = &[$($name), +];

        #[cfg(test)]
        fn invoke_noop_intent(g: &grouse_core::GrouseUnstable, name: &str) {
            match name {
                $($name => {
                    // Bind first: calling a literal closure in place trips
                    // clippy::redundant_closure_call under -D warnings.
                    let call: fn(&grouse_core::GrouseUnstable) = $call;
                    call(g)
                },)+
                // An inventory entry with no case is a test that silently
                // stopped covering an intent. Fail instead.
                other => panic!("UNSTABLE_INTENTS entry `{other}` has no no-op case"),
            }
        }
    };
}

unstable_intents! {
    // Session intents.
    ("steer", |g: &grouse_core::GrouseUnstable| g.steer("hello".into(), "run-9".into())),
    ("export_session", |g: &grouse_core::GrouseUnstable| g.export_session("s1".into())),
    ("session_info", |g: &grouse_core::GrouseUnstable| g.session_info("s1".into())),
    ("context_report", |g: &grouse_core::GrouseUnstable| g.context_report("s1".into())),
    ("session_project", |g: &grouse_core::GrouseUnstable| {
        g.session_project("s1".into(), Some("p1".into()));
        g.session_project("s1".into(), None);
    }),
    // Tools & extensions.
    ("list_tools", |g: &grouse_core::GrouseUnstable| g.list_tools("s1".into())),
    ("session_extensions_list", |g: &grouse_core::GrouseUnstable| g.session_extensions_list("s1".into())),
    ("extension_catalog", |g: &grouse_core::GrouseUnstable| g.extension_catalog("s1".into())),
    ("session_extensions_add", |g: &grouse_core::GrouseUnstable| g.session_extensions_add("s1".into(), r#"{"name":"dev"}"#.into())),
    ("session_extensions_remove", |g: &grouse_core::GrouseUnstable| g.session_extensions_remove("s1".into(), "dev".into())),
    ("list_global_extensions", |g: &grouse_core::GrouseUnstable| g.list_global_extensions()),
    ("set_extension_enabled", |g: &grouse_core::GrouseUnstable| g.set_extension_enabled("dev".into(), true)),
    ("add_extension", |g: &grouse_core::GrouseUnstable| g.add_extension(r#"{"name":"builtin://x"}"#.into(), true)),
    // Sources (projects + skills).
    ("sources_list", |g: &grouse_core::GrouseUnstable| g.sources_list("project".into())),
    ("sources_create", |g: &grouse_core::GrouseUnstable| g.sources_create("project".into(), "n".into(), "d".into(), "c".into())),
    ("sources_delete", |g: &grouse_core::GrouseUnstable| g.sources_delete("project".into(), "p".into())),
    ("sources_update", |g: &grouse_core::GrouseUnstable| g.sources_update("project".into(), "p".into(), "n".into(), "d".into(), "c".into())),
    // Config & models.
    ("config_read", |g: &grouse_core::GrouseUnstable| g.config_read("model".into())),
    ("config_upsert", |g: &grouse_core::GrouseUnstable| g.config_upsert("mode".into(), "auto".into())),
    ("supported_models", |g: &grouse_core::GrouseUnstable| g.supported_models("openai".into())),
    ("providers_list", |g: &grouse_core::GrouseUnstable| g.providers_list()),
    // MCP-App resources.
    ("resources_read", |g: &grouse_core::GrouseUnstable| g.resources_read("s1".into(), "charts/x".into(), "ext".into())),
    // Recipes.
    ("recipes_list", |g: &grouse_core::GrouseUnstable| g.recipes_list()),
    ("recipes_schedule", |g: &grouse_core::GrouseUnstable| {
        g.recipes_schedule("r1".into(), Some("0 9 * * *".into()));
        g.recipes_schedule("r1".into(), None);
    }),
    ("recipes_save", |g: &grouse_core::GrouseUnstable| g.recipes_save("r1".into(), r#"{"id":"r1"}"#.into())),
    ("recipes_delete", |g: &grouse_core::GrouseUnstable| g.recipes_delete("r1".into())),
    // Schedules.
    ("schedules_list", |g: &grouse_core::GrouseUnstable| g.schedules_list()),
    ("schedules_pause", |g: &grouse_core::GrouseUnstable| g.schedules_pause("j1".into())),
    ("schedules_unpause", |g: &grouse_core::GrouseUnstable| g.schedules_unpause("j1".into())),
    ("schedules_run_now", |g: &grouse_core::GrouseUnstable| g.schedules_run_now("j1".into())),
    ("schedules_delete", |g: &grouse_core::GrouseUnstable| g.schedules_delete("j1".into())),
    ("schedules_update", |g: &grouse_core::GrouseUnstable| g.schedules_update("j1".into(), "0 9 * * *".into())),
    // Working dir & direct tools.
    ("working_dir_update", |g: &grouse_core::GrouseUnstable| g.working_dir_update("s1".into(), "/tmp".into())),
    ("tools_call", |g: &grouse_core::GrouseUnstable| g.tools_call("s1".into(), "shell__run".into(), r#"{"cmd":"echo hi"}"#.into())),
    ("tools_call_named", |g: &grouse_core::GrouseUnstable| g.tools_call_named("s1".into(), "shell".into(), "shell__run".into(), r#"{"cmd":"echo hi"}"#.into())),
    // Dictation (speech-to-text).
    ("dictation_config", |g: &grouse_core::GrouseUnstable| g.dictation_config()),
    ("dictation_transcribe", |g: &grouse_core::GrouseUnstable| g.dictation_transcribe("AAAA".into(), "audio/wav".into(), "local".into())),
    // Server-request answers (no pending request to answer -> clean no-op).
    ("respond_recipe_params", |g: &grouse_core::GrouseUnstable| g.respond_recipe_params("test-req".into(), "accept".into(), r#"{"k":"v"}"#.into())),
    ("respond_elicitation", |g: &grouse_core::GrouseUnstable| g.respond_elicitation("test-req".into(), "decline".into(), String::new())),
}

// The concrete shim and the trait it consumes stay in grouse-core; this is the
// single Rust type both paths name, so no client's imports change.
#[cfg(test)]
pub use grouse_core::{GrouseUnstable, GrouseUnstableListener};

// ---------------------------------------------------------------------------
// Tests (X-2): lock in the shim's silent-param-drop contract from THIS crate
// boundary. Routing/connected behavior is exercised in grouse-core's own test
// harness; from here — a downstream crate — we pin the disconnected contract:
// EVERY inventoried intent is a clean no-op, with no events and no panics.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::{GrouseUnstable, GrouseUnstableListener, UNSTABLE_INTENTS};
    use parking_lot::Mutex;
    use std::sync::Arc;

    /// A listener that records every callback it receives. Clonable: the clone
    /// handed to `GrouseUnstable` shares the same backing store as the copy the
    /// test keeps for inspection.
    #[derive(Default, Clone)]
    struct RecordingListener {
        events: Arc<Mutex<Vec<String>>>,
    }

    impl RecordingListener {
        fn events(&self) -> Vec<String> {
            self.events.lock().clone()
        }
    }

    impl GrouseUnstableListener for RecordingListener {
        fn on_export(&self, _data: String) {
            self.events.lock().push("on_export".into());
        }
        fn on_recipe_params(&self, _owner: grouse_core::SessionOwner, _request_id: String, _parameters: String) {
            self.events.lock().push("on_recipe_params".into());
        }
        fn on_elicitation(&self, _owner: grouse_core::SessionOwner, _request_id: String, _schema: String) {
            self.events.lock().push("on_elicitation".into());
        }
        fn on_compaction_status(&self, _message: String) {
            self.events.lock().push("on_compaction_status".into());
        }
        fn on_usage(&self, _used: i64, _size: i64, _cost: f64) {
            self.events.lock().push("on_usage".into());
        }
        fn on_message_usage(&self, _o: u64, _e: u64, _t: u64, _c: f64) {
            self.events.lock().push("on_message_usage".into());
        }
        fn on_app_resource(&self, _key: String, _html: String) {
            self.events.lock().push("on_app_resource".into());
        }
        fn on_recipes(&self, _recipes: String) {
            self.events.lock().push("on_recipes".into());
        }
        fn on_schedules(&self, _schedules: String) {
            self.events.lock().push("on_schedules".into());
        }
        fn on_projects(&self, _projects: String) {
            self.events.lock().push("on_projects".into());
        }
        fn on_skills(&self, _skills: String) {
            self.events.lock().push("on_skills".into());
        }
        fn on_tools(&self, _session_id: String, _tools: String) {
            self.events.lock().push("on_tools".into());
        }
        fn on_extensions(&self, _extensions: String) {
            self.events.lock().push("on_extensions".into());
        }
        fn on_extension_catalog(&self, _session_id: String, _extensions: String) {
            self.events.lock().push("on_extension_catalog".into());
        }
        fn on_session_extensions(&self, _session_id: String, _extensions: String) {
            self.events.lock().push("on_session_extensions".into());
        }
        fn on_config_value(&self, _key: String, _value: String) {
            self.events.lock().push("on_config_value".into());
        }
        fn on_supported_models(&self, _provider: String, _models: String) {
            self.events.lock().push("on_supported_models".into());
        }
        fn on_providers(&self, _providers: String) {
            self.events.lock().push("on_providers".into());
        }
        fn on_context_report(&self, _session_id: String, _report: String) {}
        fn on_session_probe(&self, _session_id: String, _updated_at: String, _message_count: i64) {
            self.events.lock().push("on_session_probe".into());
        }
        fn on_tool_result(&self, _text: String, _is_error: bool) {
            self.events.lock().push("on_tool_result".into());
        }
        fn on_dictation_config(&self, _providers: String) {
            self.events.lock().push("on_dictation_config".into());
        }
        fn on_transcript(&self, _text: String) {
            self.events.lock().push("on_transcript".into());
        }
        fn on_error(&self, _method: String, _message: String) {
            self.events.lock().push("on_error".into());
        }
    }

    /// The re-export contract survives: both `GrouseUnstable` and the
    /// `GrouseUnstableListener` trait are constructible/implementable from this
    /// crate, and the constructor registers its listeners without a connection.
    #[test]
    fn re_exported_types_are_usable() {
        let listener = RecordingListener::default();
        let _g = GrouseUnstable::new(Box::new(listener.clone()));
        drop(listener);
    }

    /// With no connection installed in the spine registry, every inventoried
    /// shim intent must be a clean no-op: params are silently dropped,
    /// unsupported routes are refused without an error/event, and nothing
    /// panics. The list and the calls are generated together above, so a new
    /// intent added to the inventory is covered without editing this test.
    #[test]
    fn all_intents_are_clean_noops_when_disconnected() {
        let listener = RecordingListener::default();
        let g = GrouseUnstable::new(Box::new(listener.clone()));

        for name in UNSTABLE_INTENTS {
            super::invoke_noop_intent(&g, name);
        }

        assert!(
            listener.events().is_empty(),
            "no connection installed -> every intent must be a silent no-op; got: {:?}",
            listener.events()
        );
    }

    /// The old test called 35-ish methods from a hand-written body; five
    /// (`context_report`, `extension_catalog`, `tools_call_named`,
    /// `dictation_config`, `dictation_transcribe`) were missing. Keep that
    /// count visible here so a future split of the inventory is noticed.
    #[test]
    fn manifest_covers_every_public_intent() {
        assert_eq!(
            UNSTABLE_INTENTS.len(),
            39,
            "grouse-core gained or lost an unstable intent; update the inventory macro"
        );
    }
}
