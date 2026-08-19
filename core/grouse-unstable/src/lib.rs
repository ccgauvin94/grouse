// SPDX-License-Identifier: AGPL-3.0-or-later

//! grouse-unstable: the goose-fork `_goose/unstable/*` compatibility shim.
//!
//! The `GrouseUnstable` interface currently lives in `grouse-core` (next to the
//! stable `Core`) so both uniffi interfaces share one scaffolding unit and one
//! set of shared types. This crate is the retirement boundary: as the GDK
//! absorbs each unstable feature, its method leaves `GrouseUnstable` and,
//! eventually, the whole interface moves here or disappears.

pub use grouse_core::{GrouseUnstable, GrouseUnstableListener};

// ---------------------------------------------------------------------------
// Tests (X-2): lock in the shim's silent-param-drop contract from THIS crate
// boundary. The routing/connected behavior is exercised in grouse-core's own
// test harness (which can install a stub `RpcConn` via the pub(crate) spine
// registry); from here — a strictly downstream crate — we pin the disconnected
// contract: every one of the ~35 public intents is a clean no-op. Params are
// refused/dropped without breaking, and nothing is emitted or panics.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::{GrouseUnstable, GrouseUnstableListener};
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
        fn on_recipe_params(&self, _parameters: String) {
            self.events.lock().push("on_recipe_params".into());
        }
        fn on_elicitation(&self, _schema: String) {
            self.events.lock().push("on_elicitation".into());
        }
        fn on_compaction_status(&self, _message: String) {
            self.events.lock().push("on_compaction_status".into());
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
        fn on_session_probe(&self, _session_id: String, _updated_at: String, _message_count: i64) {
            self.events.lock().push("on_session_probe".into());
        }
        fn on_tool_result(&self, _text: String, _is_error: bool) {
            self.events.lock().push("on_tool_result".into());
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
        // Both items are reachable from this crate boundary and the constructor
        // runs its listener-registration without a connection or a panic.
        let listener = RecordingListener::default();
        let _g = GrouseUnstable::new(Box::new(listener.clone()));
        drop(listener);
    }

    /// With no connection installed in the spine registry, every one of the
    /// ~35 shim intents must be a clean no-op: caller-supplied params are
    /// silently dropped, unsupported routes are refused without an error/event,
    /// and — critically — nothing panics. This is the documented contract
    /// ("these intents no-op (or error) while disconnected"); a future change
    /// that starts dispatching or panicking off-connection breaks this test.
    #[test]
    fn all_intents_are_clean_noops_when_disconnected() {
        let listener = RecordingListener::default();
        let g = GrouseUnstable::new(Box::new(listener.clone()));

        // Session intents.
        g.steer("hello".into(), "run-9".into());
        g.export_session("s1".into());
        g.session_info("s1".into());
        g.session_project("s1".into(), Some("p1".into()));
        g.session_project("s1".into(), None);
        // Tools & extensions.
        g.list_tools("s1".into());
        g.session_extensions_list("s1".into());
        g.session_extensions_add("s1".into(), r#"{"name":"dev"}"#.into());
        g.session_extensions_remove("s1".into(), "dev".into());
        g.list_global_extensions();
        g.set_extension_enabled("dev".into(), true);
        g.add_extension(r#"{"name":"builtin://x"}"#.into(), true);
        // Sources (projects + skills).
        g.sources_list("project".into());
        g.sources_create("project".into(), "n".into(), "d".into(), "c".into());
        g.sources_delete("project".into(), "p".into());
        g.sources_update("project".into(), "p".into(), "n".into(), "d".into(), "c".into());
        // Config & models.
        g.config_read("model".into());
        g.config_upsert("mode".into(), "auto".into());
        g.supported_models("openai".into());
        g.providers_list();
        // MCP-App resources.
        g.resources_read("s1".into(), "charts/x".into(), "ext".into());
        // Recipes.
        g.recipes_list();
        g.recipes_schedule("r1".into(), Some("0 9 * * *".into()));
        g.recipes_schedule("r1".into(), None);
        g.recipes_save("r1".into(), r#"{"id":"r1"}"#.into());
        g.recipes_delete("r1".into());
        // Schedules.
        g.schedules_list();
        g.schedules_pause("j1".into());
        g.schedules_unpause("j1".into());
        g.schedules_run_now("j1".into());
        g.schedules_delete("j1".into());
        g.schedules_update("j1".into(), "0 9 * * *".into());
        // Working dir & direct tools.
        g.working_dir_update("s1".into(), "/tmp".into());
        g.tools_call("s1".into(), "shell__run".into(), r#"{"cmd":"echo hi"}"#.into());
        // Server-request answers (no pending request to answer -> clean no-op).
        g.respond_recipe_params("accept".into(), r#"{"k":"v"}"#.into());
        g.respond_elicitation("decline".into(), String::new());

        assert!(
            listener.events().is_empty(),
            "no connection installed -> every intent must be a silent no-op; got: {:?}",
            listener.events()
        );
    }
}
