//! GrouseUnstable: the goose-fork `_goose/unstable/...` shim (CONTRACT §5), marked for
//! retirement as GDK absorbs each feature.
//!
//! Every method is `conn.rpc(method, params)` plus a reply handler, mirroring the desktop's
//! `AcpClient::response()` dispatch table exactly (src/acpclient.cpp): list replies emit the
//! payload on the listener; mutation replies carry no useful body, so the handler re-requests
//! the list and emits that — the UI reflects server state, never a local patch.
//!
//! Wire params are the union of the desktop + Android shapes (contract inventory §1.1): the
//! Android-only methods `recipes/save`, `schedules/delete`, `session/working-dir/update` and
//! `tools/call` are part of the CONTRACT surface and are implemented here with Android's params.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::spine::{self, PendingRequest, RpcConn};
use crate::GrouseUnstableListener;

/// The unstable `_goose/unstable/...` shim (CONTRACT §5), retiring with `grouse-unstable`.
///
/// Constructed independently of [`crate::Core`]; the live connection is resolved per call
/// through `spine::current_conn()`, so these intents no-op (or error) while disconnected —
/// exactly like the desktop's guarded `rpc` calls.
#[derive(uniffi::Object)]
pub struct GrouseUnstable {
    listener: Arc<dyn GrouseUnstableListener>,
    /// Deregisters the server-request listener when this object goes away.
    _requests: spine::ServerRequestListenerGuard,
    /// Deregisters the custom-notification listener when this object goes away.
    _notifications: spine::NotificationListenerGuard,
}

#[uniffi::export]
impl GrouseUnstable {
    #[uniffi::constructor]
    pub fn new(listener: Box<dyn GrouseUnstableListener>) -> Arc<Self> {
        let listener: Arc<dyn GrouseUnstableListener> = Arc::from(listener);
        // Server → client requests (recipe-params / elicitation) arrive at the spine; it
        // parks the responder and fans out the raw request here so the UI can opt into
        // answering via respond_recipe_params / respond_elicitation (CONTRACT §5).
        let _requests = spine::register_server_request_listener({
            let listener = Arc::clone(&listener);
            Arc::new(move |method: &str, params: Value| {
                Self::dispatch_server_request(&*listener, method, params);
            })
        });
        // Goose-custom notifications (gated on `customNotifications`): `status_message` →
        // compaction status, `message_usage` → per-message tok/s + cost (CONTRACT §5).
        let _notifications = spine::register_notification_listener({
            let listener = Arc::clone(&listener);
            Arc::new(move |method: &str, params: Value| {
                Self::dispatch_notification(&*listener, method, params);
            })
        });
        Arc::new(Self { listener, _requests, _notifications })
    }

    // -----------------------------------------------------------------------
    // Session intents
    // -----------------------------------------------------------------------

    /// Inject text into the RUNNING turn. The server validates `expected_run_id` so a run
    /// that ended between typing and sending fails loudly instead of starting a stray turn.
    /// The reply streams back as chunks (nothing to do here).
    pub fn steer(&self, text: String, expected_run_id: String) {
        let Some(conn) = spine::current_conn() else { return };
        let Some(sid) = conn.active_session_id() else {
            // Desktop parity: steering without a bound session is a user-visible failure.
            self.listener.on_error(
                "_goose/unstable/session/steer".to_string(),
                "not ready — no session".to_string(),
            );
            return;
        };
        self.call(
            &*conn,
            "_goose/unstable/session/steer",
            json!({
                "sessionId": sid,
                "prompt": [{"type": "text", "text": text}],
                "expectedRunId": expected_run_id,
            }),
        );
    }

    /// Export a session transcript; the payload arrives on `on_export`.
    pub fn export_session(&self, session_id: String) {
        let Some(conn) = spine::current_conn() else { return };
        if let Some(result) =
            self.call(&*conn, "_goose/unstable/session/export", json!({"sessionId": session_id}))
        {
            let data = result.get("data").and_then(Value::as_str).unwrap_or_default();
            self.listener.on_export(data.to_string());
        }
    }

    /// Cheap metadata probe (`{session}` with `updatedAt` + `_meta.messageCount`), used by
    /// resync and resume-cwd resolution; surfaces as `on_session_probe`.
    pub fn session_info(&self, session_id: String) {
        let Some(conn) = spine::current_conn() else { return };
        if let Some(result) =
            self.call(&*conn, "_goose/unstable/session/info", json!({"sessionId": session_id}))
        {
            let session = result.get("session");
            let updated_at =
                session.and_then(|s| s.get("updatedAt")).and_then(Value::as_str).unwrap_or_default();
            let message_count = session
                .and_then(|s| s.get("_meta"))
                .and_then(|m| m.get("messageCount"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            self.listener.on_session_probe(session_id, updated_at.to_string(), message_count);
        }
    }

    /// Move a session between projects (`None` un-files — an explicit JSON null, never
    /// an omitted key); re-lists projects so the sidebar reflects the change.
    pub fn session_project(&self, session_id: String, project_id: Option<String>) {
        let Some(conn) = spine::current_conn() else { return };
        self.call(
            &*conn,
            "_goose/unstable/session/project/update",
            json!({"sessionId": session_id, "projectId": project_id}),
        );
        self.relist_projects(&*conn);
    }

    // -----------------------------------------------------------------------
    // Tools & extensions
    // -----------------------------------------------------------------------

    /// List the session's active tools (`extension__tool` names); surfaces as `on_tools`.
    pub fn list_tools(&self, session_id: String) {
        let Some(conn) = spine::current_conn() else { return };
        if let Some(result) =
            self.call(&*conn, "_goose/unstable/tools/list", json!({"sessionId": session_id}))
        {
            let tools = result.get("tools").cloned().unwrap_or(Value::Null);
            self.listener.on_tools(session_id, tools.to_string());
        }
    }

    /// List the session's extensions; surfaces as `on_session_extensions`.
    pub fn session_extensions_list(&self, session_id: String) {
        let Some(conn) = spine::current_conn() else { return };
        if let Some(result) = self.call(
            &*conn,
            "_goose/unstable/session/extensions/list",
            json!({"sessionId": session_id}),
        ) {
            let extensions = result.get("extensions").cloned().unwrap_or(Value::Null);
            self.listener.on_session_extensions(session_id, extensions.to_string());
        }
    }

    /// Enable an extension for THIS session. The add reply is the moment the session's tool
    /// set is current, so refresh tools + session extensions from here (desktop behavior).
    pub fn session_extensions_add(&self, session_id: String, extension: String) {
        let Ok(extension) = serde_json::from_str::<Value>(&extension) else { return };
        let Some(conn) = spine::current_conn() else { return };
        self.call(
            &*conn,
            "_goose/unstable/session/extensions/add",
            json!({"sessionId": session_id, "extension": extension}),
        );
        self.relist_tools(&*conn, &session_id);
        self.relist_session_extensions(&*conn, &session_id);
    }

    /// Remove an extension from THIS session. The paired add re-lists (desktop behavior).
    pub fn session_extensions_remove(&self, session_id: String, name: String) {
        let Some(conn) = spine::current_conn() else { return };
        self.call(
            &*conn,
            "_goose/unstable/session/extensions/remove",
            json!({"sessionId": session_id, "name": name}),
        );
    }

    /// List GLOBAL extension defaults (config.yaml); surfaces as `on_extensions`.
    pub fn list_global_extensions(&self) {
        let Some(conn) = spine::current_conn() else { return };
        self.relist_global_extensions(&*conn);
    }

    /// Toggle a GLOBAL extension; re-lists so the UI reflects the new enabled state.
    pub fn set_extension_enabled(&self, name: String, enabled: bool) {
        let Some(conn) = spine::current_conn() else { return };
        self.call(
            &*conn,
            "_goose/unstable/config/extensions/set-enabled",
            json!({"name": name, "enabled": enabled}),
        );
        self.relist_global_extensions(&*conn);
    }

    /// Add a GLOBAL extension (defaults for NEW sessions); re-lists.
    pub fn add_extension(&self, extension: String, enabled: bool) {
        let Ok(extension) = serde_json::from_str::<Value>(&extension) else { return };
        let Some(conn) = spine::current_conn() else { return };
        self.call(
            &*conn,
            "_goose/unstable/config/extensions/add",
            json!({"extension": extension, "enabled": enabled}),
        );
        self.relist_global_extensions(&*conn);
    }

    // -----------------------------------------------------------------------
    // Sources (projects + skills)
    // -----------------------------------------------------------------------

    /// List projects or skills (`sources/list` backs both; the reply carries no tag, so the
    /// handler branches on the requested `source_type`). Surfaces as `on_projects` /
    /// `on_skills`.
    pub fn sources_list(&self, source_type: String) {
        let Some(conn) = spine::current_conn() else { return };
        if let Some(result) =
            self.call(&*conn, "_goose/unstable/sources/list", json!({"type": source_type}))
        {
            let sources = result.get("sources").cloned().unwrap_or(Value::Null).to_string();
            if source_type == "skill" {
                self.listener.on_skills(sources);
            } else {
                self.listener.on_projects(sources);
            }
        }
    }

    /// Create a project or skill (`target.scope: "global"`); re-lists the touched family.
    pub fn sources_create(
        &self,
        source_type: String,
        name: String,
        description: String,
        content: String,
    ) {
        let Some(conn) = spine::current_conn() else { return };
        self.call(
            &*conn,
            "_goose/unstable/sources/create",
            json!({
                "type": source_type,
                "name": name,
                "description": description,
                "content": content,
                "target": {"scope": "global"},
            }),
        );
        self.relist_sources(&*conn, &source_type);
    }

    /// Delete a project or skill by its source PATH; re-lists the touched family.
    pub fn sources_delete(&self, source_type: String, path: String) {
        let Some(conn) = spine::current_conn() else { return };
        self.call(
            &*conn,
            "_goose/unstable/sources/delete",
            json!({"type": source_type, "path": path}),
        );
        self.relist_sources(&*conn, &source_type);
    }

    /// Update a project or skill (whole-source replace); re-lists the touched family.
    pub fn sources_update(
        &self,
        source_type: String,
        path: String,
        name: String,
        description: String,
        content: String,
    ) {
        let Some(conn) = spine::current_conn() else { return };
        self.call(
            &*conn,
            "_goose/unstable/sources/update",
            json!({
                "type": source_type,
                "path": path,
                "name": name,
                "description": description,
                "content": content,
            }),
        );
        self.relist_sources(&*conn, &source_type);
    }

    // -----------------------------------------------------------------------
    // Config & models
    // -----------------------------------------------------------------------

    /// Read one global config.yaml value; surfaces as `on_config_value(key, value)`.
    pub fn config_read(&self, key: String) {
        let Some(conn) = spine::current_conn() else { return };
        if let Some(result) = self.call(&*conn, "_goose/unstable/config/read", json!({"key": key}))
        {
            let key = result.get("key").and_then(Value::as_str).unwrap_or_default();
            let value = result.get("value").and_then(Value::as_str).unwrap_or_default();
            self.listener.on_config_value(key.to_string(), value.to_string());
        }
    }

    /// Upsert one global config.yaml value (new sessions only). The reply is empty; nothing
    /// to reflect — the caller re-reads if it wants confirmation (desktop behavior).
    pub fn config_upsert(&self, key: String, value: String) {
        let Some(conn) = spine::current_conn() else { return };
        self.call(
            &*conn,
            "_goose/unstable/config/upsert",
            json!({"key": key, "value": value}),
        );
    }

    /// Live model list for a provider; surfaces as `on_supported_models(provider, models)`.
    pub fn supported_models(&self, provider: String) {
        let Some(conn) = spine::current_conn() else { return };
        if let Some(result) = self.call(
            &*conn,
            "_goose/unstable/providers/supported-models/list",
            json!({"providerId": provider}),
        ) {
            let provider = result.get("providerId").and_then(Value::as_str).unwrap_or_default();
            let models = result.get("models").cloned().unwrap_or(Value::Null);
            self.listener.on_supported_models(provider.to_string(), models.to_string());
        }
    }

    // -----------------------------------------------------------------------
    // MCP-App resources
    // -----------------------------------------------------------------------

    /// Fetch an MCP-App HTML template. The `app_key` mirrors the desktop's
    /// `extension|uri` convention so the UI can match the bubble that asked for it.
    /// A failed fetch clears the in-flight marker with empty html (never an error bubble).
    pub fn resources_read(&self, session_id: String, uri: String, extension: String) {
        let Some(conn) = spine::current_conn() else { return };
        let app_key = format!("{extension}|{uri}");
        match conn.rpc(
            "_goose/unstable/resources/read",
            json!({"sessionId": session_id, "uri": uri, "extensionName": extension}),
        ) {
            Ok(result) => {
                let mut html = String::new();
                if let Some(contents) =
                    result.get("result").and_then(|r| r.get("contents")).and_then(Value::as_array)
                {
                    for entry in contents {
                        if let Some(text) = entry.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                html = text.to_string();
                                break;
                            }
                        }
                    }
                }
                self.listener.on_app_resource(app_key, html);
            }
            Err(_) => self.listener.on_app_resource(app_key, String::new()),
        }
    }

    // -----------------------------------------------------------------------
    // Recipes
    // -----------------------------------------------------------------------

    /// List saved recipes; surfaces as `on_recipes`.
    pub fn recipes_list(&self) {
        let Some(conn) = spine::current_conn() else { return };
        self.relist_recipes(&*conn);
    }

    /// Schedule a recipe (`cron_schedule` omitted → explicit null = unschedule); re-lists
    /// schedules AND recipes (a schedule row appears/disappears in both).
    pub fn recipes_schedule(&self, id: String, cron_schedule: Option<String>) {
        let Some(conn) = spine::current_conn() else { return };
        self.call(
            &*conn,
            "_goose/unstable/recipes/schedule",
            json!({"id": id, "cron_schedule": cron_schedule}),
        );
        self.relist_schedules(&*conn);
        self.relist_recipes(&*conn);
    }

    /// Overwrite a saved recipe; `recipe` must be a COMPLETE recipe DTO (the UI edits the
    /// raw listed shape — anything modelled would silently drop fields); re-lists.
    pub fn recipes_save(&self, id: String, recipe: String) {
        let Ok(recipe) = serde_json::from_str::<Value>(&recipe) else { return };
        let Some(conn) = spine::current_conn() else { return };
        self.call(
            &*conn,
            "_goose/unstable/recipes/save",
            json!({"id": id, "recipe": recipe}),
        );
        self.relist_recipes(&*conn);
    }

    /// Delete a recipe; re-lists.
    pub fn recipes_delete(&self, id: String) {
        let Some(conn) = spine::current_conn() else { return };
        self.call(&*conn, "_goose/unstable/recipes/delete", json!({"id": id}));
        self.relist_recipes(&*conn);
    }

    // -----------------------------------------------------------------------
    // Schedules
    // -----------------------------------------------------------------------

    /// List schedule jobs; surfaces as `on_schedules`.
    pub fn schedules_list(&self) {
        let Some(conn) = spine::current_conn() else { return };
        self.relist_schedules(&*conn);
    }

    /// Pause a job; re-lists.
    pub fn schedules_pause(&self, schedule_id: String) {
        self.schedule_mutation("pause", schedule_id);
    }

    /// Unpause a job; re-lists.
    pub fn schedules_unpause(&self, schedule_id: String) {
        self.schedule_mutation("unpause", schedule_id);
    }

    /// Run a job now. This BLOCKS server-side for the whole run — the caller must not run it
    /// on the UI thread (desktop's own warning).
    pub fn schedules_run_now(&self, schedule_id: String) {
        self.schedule_mutation("run-now", schedule_id);
    }

    /// Delete a job; re-lists.
    pub fn schedules_delete(&self, schedule_id: String) {
        self.schedule_mutation("delete", schedule_id);
    }

    /// Change a job's cron (recipe content is untouched); re-lists.
    pub fn schedules_update(&self, schedule_id: String, cron: String) {
        let Some(conn) = spine::current_conn() else { return };
        self.call(
            &*conn,
            "_goose/unstable/schedules/update",
            json!({"scheduleId": schedule_id, "cron": cron}),
        );
        self.relist_schedules(&*conn);
    }

    // -----------------------------------------------------------------------
    // Working dir & direct tools
    // -----------------------------------------------------------------------

    /// Sanctioned working-directory rewrite for a session (Android-only wire method).
    /// Optimistic: nothing to reflect on the reply.
    pub fn working_dir_update(&self, session_id: String, dir: String) {
        let Some(conn) = spine::current_conn() else { return };
        self.call(
            &*conn,
            "_goose/unstable/session/working-dir/update",
            json!({"sessionId": session_id, "workingDir": dir}),
        );
    }

    /// Invoke a tool DIRECTLY — no model turn, deterministic. The concatenated text content
    /// blocks (or the error text) arrive on `on_tool_result`.
    pub fn tools_call(&self, session_id: String, name: String, args: String) {
        let Ok(args) = serde_json::from_str::<Value>(&args) else { return };
        let Some(conn) = spine::current_conn() else { return };
        match conn.rpc(
            "_goose/unstable/tools/call",
            json!({"sessionId": session_id, "name": name, "arguments": args}),
        ) {
            Ok(result) => {
                let mut text = String::new();
                if let Some(contents) = result.get("content").and_then(Value::as_array) {
                    for entry in contents {
                        if let Some(t) = entry.get("text").and_then(Value::as_str) {
                            text.push_str(t);
                        }
                    }
                }
                self.listener.on_tool_result(text, false);
            }
            Err(e) => self.listener.on_tool_result(format!("{e}"), true),
        }
    }

    // -----------------------------------------------------------------------
    // Server-request answers (CONTRACT §5)
    // -----------------------------------------------------------------------

    /// Answer a pending `recipe/request-params` request. `values` is JSON mapping key →
    /// string; single-slot per family (the CONTRACT signatures carry no request key).
    pub fn respond_recipe_params(&self, action: String, values: String) {
        spine::answer_request(
            PendingRequest::RecipeParams,
            Self::recipe_params_answer(&action, &values),
        );
    }

    /// Answer a pending `elicitation/create` request: `accept` with a content object,
    /// `decline` / `cancel` without one.
    pub fn respond_elicitation(&self, action: String, content: String) {
        spine::answer_request(PendingRequest::Elicitation, Self::elicitation_answer(&action, &content));
    }
}

impl GrouseUnstable {
    /// Fan out a server→client request to the listener: recipe-params and elicitation
    /// ride the raw JSON so the UI can render a form (CONTRACT §5).
    fn dispatch_server_request(listener: &dyn GrouseUnstableListener, method: &str, params: Value) {
        match method {
            "_goose/unstable/session/recipe/request-params" => {
                let parameters = params.get("parameters").cloned().unwrap_or(Value::Null);
                listener.on_recipe_params(parameters.to_string());
            }
            "elicitation/create" => {
                let schema = params.get("requestedSchema").cloned().unwrap_or(Value::Null);
                listener.on_elicitation(schema.to_string());
            }
            _ => {}
        }
    }

    /// Fan out a goose-custom notification: `status_message` → compaction status,
    /// `message_usage` → per-message tok/s + cost. The desktop tolerates both the
    /// `{update: {...}}` wrapper and the direct/raw form.
    fn dispatch_notification(listener: &dyn GrouseUnstableListener, method: &str, params: Value) {
        if method != "_goose/unstable/session/update" {
            return;
        }
        let update = match params.get("update") {
            Some(Value::Object(_)) => params.get("update").expect("checked above"),
            _ => &params,
        };
        match update.get("sessionUpdate").and_then(Value::as_str) {
            Some("status_message") => {
                let message = update
                    .get("status")
                    .and_then(|s| s.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !message.is_empty() {
                    listener.on_compaction_status(message.to_string());
                }
            }
            Some("message_usage") => {
                if let Some(usage) = update.get("usage") {
                    let output_tokens = usage.get("outputTokens").and_then(Value::as_u64).unwrap_or(0);
                    let elapsed_ms = usage.get("elapsedMs").and_then(Value::as_u64).unwrap_or(0);
                    let time_to_first_token_ms =
                        usage.get("timeToFirstTokenMs").and_then(Value::as_u64).unwrap_or(0);
                    let cost = usage.get("cost").and_then(Value::as_f64).unwrap_or(0.0);
                    listener.on_message_usage(output_tokens, elapsed_ms, time_to_first_token_ms, cost);
                }
            }
            _ => {}
        }
    }

    /// `{action: <action>, values: <values-json>}` — values default to `{}` when the UI
    /// passed unparseable JSON (the server wants a map, not a string).
    fn recipe_params_answer(action: &str, values: &str) -> Value {
        let values = serde_json::from_str::<Value>(values)
            .unwrap_or_else(|_| Value::Object(Default::default()));
        json!({"action": action, "values": values})
    }

    /// `{action: <action>}` plus `content` when the UI passed one (decline/cancel carry
    /// neither, matching the Android shapes).
    fn elicitation_answer(action: &str, content: &str) -> Value {
        let mut result = json!({"action": action});
        if !content.trim().is_empty() {
            if let Ok(content) = serde_json::from_str::<Value>(content) {
                result["content"] = content;
            }
        }
        result
    }
}

// ---------------------------------------------------------------------------
// Reply helpers: `rpc` + the re-list pattern, one re-list per list family.
// ---------------------------------------------------------------------------

impl GrouseUnstable {
    /// Run one RPC; on error emit `on_error(method, message)` and return `None`.
    fn call(&self, conn: &dyn RpcConn, method: &str, params: Value) -> Option<Value> {
        match conn.rpc(method, params) {
            Ok(result) => Some(result),
            Err(e) => {
                self.listener.on_error(method.to_string(), format!("{e}"));
                None
            }
        }
    }

    fn relist_projects(&self, conn: &dyn RpcConn) {
        if let Some(result) = self.call(conn, "_goose/unstable/sources/list", json!({"type": "project"})) {
            let projects = result.get("sources").cloned().unwrap_or(Value::Null);
            self.listener.on_projects(projects.to_string());
        }
    }

    fn relist_skills(&self, conn: &dyn RpcConn) {
        if let Some(result) = self.call(conn, "_goose/unstable/sources/list", json!({"type": "skill"})) {
            let skills = result.get("sources").cloned().unwrap_or(Value::Null);
            self.listener.on_skills(skills.to_string());
        }
    }

    fn relist_sources(&self, conn: &dyn RpcConn, source_type: &str) {
        if source_type == "skill" {
            self.relist_skills(conn);
        } else {
            self.relist_projects(conn);
        }
    }

    fn relist_recipes(&self, conn: &dyn RpcConn) {
        if let Some(result) = self.call(conn, "_goose/unstable/recipes/list", json!({})) {
            let recipes = result.get("recipes").cloned().unwrap_or(Value::Null);
            self.listener.on_recipes(recipes.to_string());
        }
    }

    fn relist_schedules(&self, conn: &dyn RpcConn) {
        if let Some(result) = self.call(conn, "_goose/unstable/schedules/list", json!({})) {
            let schedules = result.get("jobs").cloned().unwrap_or(Value::Null);
            self.listener.on_schedules(schedules.to_string());
        }
    }

    fn relist_tools(&self, conn: &dyn RpcConn, session_id: &str) {
        if let Some(result) =
            self.call(conn, "_goose/unstable/tools/list", json!({"sessionId": session_id}))
        {
            let tools = result.get("tools").cloned().unwrap_or(Value::Null);
            self.listener.on_tools(session_id.to_string(), tools.to_string());
        }
    }

    fn relist_session_extensions(&self, conn: &dyn RpcConn, session_id: &str) {
        if let Some(result) = self.call(
            conn,
            "_goose/unstable/session/extensions/list",
            json!({"sessionId": session_id}),
        ) {
            let extensions = result.get("extensions").cloned().unwrap_or(Value::Null);
            self.listener.on_session_extensions(session_id.to_string(), extensions.to_string());
        }
    }

    fn relist_global_extensions(&self, conn: &dyn RpcConn) {
        if let Some(result) = self.call(conn, "_goose/unstable/config/extensions/list", json!({})) {
            let extensions = result.get("extensions").cloned().unwrap_or(Value::Null);
            self.listener.on_extensions(extensions.to_string());
        }
    }

    fn schedule_mutation(&self, op: &str, schedule_id: String) {
        let Some(conn) = spine::current_conn() else { return };
        self.call(
            &*conn,
            &format!("_goose/unstable/schedules/{op}"),
            json!({"scheduleId": schedule_id}),
        );
        self.relist_schedules(&*conn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    /// Serializes tests that touch `spine::current_conn()` — a crate-global registry, so
    /// concurrent tests would race each other's stubs.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    // -----------------------------------------------------------------------
    // Stub Conn (the rpc seam) + recording listener
    // -----------------------------------------------------------------------

    /// A scripted `RpcConn`: replies are consumed FIFO; every call is recorded.
    #[derive(Debug)]
    struct StubConn {
        calls: Mutex<Vec<(String, Value)>>,
        script: Mutex<Vec<Result<Value, spine::AcpError>>>,
        session: Mutex<Option<String>>,
    }

    impl StubConn {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                script: Mutex::new(Vec::new()),
                session: Mutex::new(None),
            })
        }

        fn script(&self, replies: Vec<Result<Value, spine::AcpError>>) {
            self.script.lock().extend(replies);
        }

        fn set_session(&self, session_id: &str) {
            *self.session.lock() = Some(session_id.to_string());
        }

        fn calls(&self) -> Vec<(String, Value)> {
            self.calls.lock().clone()
        }
    }

    impl RpcConn for StubConn {
        fn rpc(&self, method: &str, params: Value) -> Result<Value, spine::AcpError> {
            self.calls.lock().push((method.to_string(), params));
            let mut script = self.script.lock();
            if script.is_empty() {
                return Ok(Value::Null);
            }
            script.remove(0)
        }

        fn active_session_id(&self) -> Option<String> {
            self.session.lock().clone()
        }
    }

    /// One recorded listener call, in (method, args) form.
    #[derive(Debug, Clone, PartialEq)]
    enum Ev {
        Export(String),
        RecipeParams(String),
        Elicitation(String),
        CompactionStatus(String),
        MessageUsage(u64, u64, u64, f64),
        AppResource(String, String),
        Recipes(String),
        Schedules(String),
        Projects(String),
        Skills(String),
        Tools(String, String),
        Extensions(String),
        SessionExtensions(String, String),
        ConfigValue(String, String),
        SupportedModels(String, String),
        SessionProbe(String, String, i64),
        ToolResult(String, bool),
        Error(String, String),
    }

    #[derive(Default)]
    struct RecordingListener {
        events: Mutex<Vec<Ev>>,
    }

    impl RecordingListener {
        fn events(&self) -> Vec<Ev> {
            self.events.lock().clone()
        }
    }

    impl GrouseUnstableListener for Arc<RecordingListener> {
        fn on_export(&self, data: String) {
            self.events.lock().push(Ev::Export(data));
        }
        fn on_recipe_params(&self, parameters: String) {
            self.events.lock().push(Ev::RecipeParams(parameters));
        }
        fn on_elicitation(&self, schema: String) {
            self.events.lock().push(Ev::Elicitation(schema));
        }
        fn on_compaction_status(&self, message: String) {
            self.events.lock().push(Ev::CompactionStatus(message));
        }
        fn on_message_usage(&self, output_tokens: u64, elapsed_ms: u64, ttft: u64, cost: f64) {
            self.events
                .lock()
                .push(Ev::MessageUsage(output_tokens, elapsed_ms, ttft, cost));
        }
        fn on_app_resource(&self, key: String, html: String) {
            self.events.lock().push(Ev::AppResource(key, html));
        }
        fn on_recipes(&self, recipes: String) {
            self.events.lock().push(Ev::Recipes(recipes));
        }
        fn on_schedules(&self, schedules: String) {
            self.events.lock().push(Ev::Schedules(schedules));
        }
        fn on_projects(&self, projects: String) {
            self.events.lock().push(Ev::Projects(projects));
        }
        fn on_skills(&self, skills: String) {
            self.events.lock().push(Ev::Skills(skills));
        }
        fn on_tools(&self, session_id: String, tools: String) {
            self.events.lock().push(Ev::Tools(session_id, tools));
        }
        fn on_extensions(&self, extensions: String) {
            self.events.lock().push(Ev::Extensions(extensions));
        }
        fn on_session_extensions(&self, session_id: String, extensions: String) {
            self.events
                .lock()
                .push(Ev::SessionExtensions(session_id, extensions));
        }
        fn on_config_value(&self, key: String, value: String) {
            self.events.lock().push(Ev::ConfigValue(key, value));
        }
        fn on_supported_models(&self, provider: String, models: String) {
            self.events
                .lock()
                .push(Ev::SupportedModels(provider, models));
        }
        fn on_session_probe(&self, session_id: String, updated_at: String, message_count: i64) {
            self.events
                .lock()
                .push(Ev::SessionProbe(session_id, updated_at, message_count));
        }
        fn on_tool_result(&self, text: String, is_error: bool) {
            self.events.lock().push(Ev::ToolResult(text, is_error));
        }
        fn on_error(&self, method: String, message: String) {
            self.events.lock().push(Ev::Error(method, message));
        }
    }

    /// Install a stub conn in the registry and build a GrouseUnstable over a recording
    /// listener; returns (unstable, listener).
    fn harness(stub: Arc<StubConn>) -> (Arc<GrouseUnstable>, Arc<RecordingListener>) {
        spine::set_current_conn(Some(stub));
        let rec = Arc::new(RecordingListener::default());
        let unstable = GrouseUnstable::new(Box::new(rec.clone()));
        (unstable, rec)
    }

    fn err() -> spine::AcpError {
        spine::AcpError::new(-32602, "boom")
    }

    /// Assert the recorded call sequence matches (method, params) pairs.
    fn assert_calls(stub: &StubConn, expected: &[(&str, Value)]) {
        let calls: Vec<(String, Value)> = stub.calls();
        let expected: Vec<(String, Value)> = expected
            .iter()
            .map(|(m, p)| (m.to_string(), p.clone()))
            .collect();
        assert_eq!(calls, expected);
    }

    // -----------------------------------------------------------------------
    // Reply / re-list patterns
    // -----------------------------------------------------------------------

    #[test]
    fn recipes_list_emits_payload() {
        let _guard = TEST_LOCK.lock();
        let stub = StubConn::new();
        stub.script(vec![Ok(json!({
            "recipes": [{"id": "r1", "recipe": {"title": "T"}, "schedule_cron": null}]
        }))]);
        let (g, rec) = harness(stub.clone());

        g.recipes_list();

        match rec.events().as_slice() {
            [Ev::Recipes(payload)] => {
                let parsed: Value = serde_json::from_str(payload).unwrap();
                assert_eq!(parsed, json!([{"id": "r1", "recipe": {"title": "T"}, "schedule_cron": null}]));
            }
            other => panic!("expected one on_recipes, got {other:?}"),
        }
        assert_calls(&stub, &[("_goose/unstable/recipes/list", json!({}))]);
    }

    #[test]
    fn recipes_delete_relists() {
        let _guard = TEST_LOCK.lock();
        let stub = StubConn::new();
        stub.script(vec![
            Ok(Value::Null), // delete reply
            Ok(json!({"recipes": []})), // re-list reply
        ]);
        let (g, rec) = harness(stub.clone());

        g.recipes_delete("r1".to_string());

        assert_eq!(
            rec.events(),
            vec![Ev::Recipes("[]".to_string())],
            "the mutation reply carries no body; the re-list is what the UI sees"
        );
        assert_calls(
            &stub,
            &[
                ("_goose/unstable/recipes/delete", json!({"id": "r1"})),
                ("_goose/unstable/recipes/list", json!({})),
            ],
        );
    }

    #[test]
    fn recipes_schedule_relists_schedules_and_recipes_with_explicit_null() {
        let _guard = TEST_LOCK.lock();
        let stub = StubConn::new();
        stub.script(vec![
            Ok(Value::Null), // schedule reply
            Ok(json!({"jobs": [{"id": "j1", "cron": "0 9 * * *"}]})),
            Ok(json!({"recipes": [{"id": "r1"}]})),
        ]);
        let (g, rec) = harness(stub.clone());

        g.recipes_schedule("r1".to_string(), None);

        assert_eq!(rec.events(), vec![Ev::Schedules("[{\"id\":\"j1\",\"cron\":\"0 9 * * *\"}]".to_string()), Ev::Recipes("[{\"id\":\"r1\"}]".to_string())]);
        assert_calls(
            &stub,
            &[
                ("_goose/unstable/recipes/schedule", json!({"id": "r1", "cron_schedule": null})),
                ("_goose/unstable/schedules/list", json!({})),
                ("_goose/unstable/recipes/list", json!({})),
            ],
        );
    }

    #[test]
    fn sources_list_dispatches_project_vs_skill() {
        let _guard = TEST_LOCK.lock();
        let stub = StubConn::new();
        stub.script(vec![Ok(json!({
            "sources": [{"name": "Project", "path": "projects/p.md", "description": "d"}]
        }))]);
        let (g, rec) = harness(stub.clone());

        g.sources_list("project".to_string());
        assert_eq!(
            rec.events(),
            vec![Ev::Projects("[{\"name\":\"Project\",\"path\":\"projects/p.md\",\"description\":\"d\"}]".to_string())],
            "sources/list type=project must surface as on_projects"
        );

        stub.script(vec![Ok(json!({
            "sources": [{"name": "Skill", "path": "skills/s.md", "writable": true}]
        }))]);
        g.sources_list("skill".to_string());
        assert_eq!(
            rec.events().last(),
            Some(&Ev::Skills("[{\"name\":\"Skill\",\"path\":\"skills/s.md\",\"writable\":true}]".to_string())),
            "sources/list type=skill must surface as on_skills"
        );

        // Both list calls share one wire method; only the type param differs.
        assert_calls(
            &stub,
            &[
                ("_goose/unstable/sources/list", json!({"type": "project"})),
                ("_goose/unstable/sources/list", json!({"type": "skill"})),
            ],
        );
    }

    #[test]
    fn sources_create_project_relists_projects_with_global_target() {
        let _guard = TEST_LOCK.lock();
        let stub = StubConn::new();
        stub.script(vec![
            Ok(Value::Null), // create reply
            Ok(json!({"sources": [{"name": "New", "path": "projects/new.md"}]})),
        ]);
        let (g, rec) = harness(stub.clone());

        g.sources_create("project".to_string(), "New".to_string(), "desc".to_string(), String::new());

        assert_eq!(
            rec.events(),
            vec![Ev::Projects("[{\"name\":\"New\",\"path\":\"projects/new.md\"}]".to_string())]
        );
        assert_calls(
            &stub,
            &[
                (
                    "_goose/unstable/sources/create",
                    json!({"type": "project", "name": "New", "description": "desc", "content": "", "target": {"scope": "global"}}),
                ),
                ("_goose/unstable/sources/list", json!({"type": "project"})),
            ],
        );
    }

    #[test]
    fn sources_delete_skill_relists_skills() {
        let _guard = TEST_LOCK.lock();
        let stub = StubConn::new();
        stub.script(vec![
            Ok(Value::Null), // delete reply
            Ok(json!({"sources": []})),
        ]);
        let (g, rec) = harness(stub.clone());

        g.sources_delete("skill".to_string(), "skills/old.md".to_string());

        assert_eq!(rec.events(), vec![Ev::Skills("[]".to_string())]);
        assert_calls(
            &stub,
            &[
                ("_goose/unstable/sources/delete", json!({"type": "skill", "path": "skills/old.md"})),
                ("_goose/unstable/sources/list", json!({"type": "skill"})),
            ],
        );
    }

    #[test]
    fn session_project_update_relists_projects_and_unfiles_with_null() {
        let _guard = TEST_LOCK.lock();
        let stub = StubConn::new();
        stub.script(vec![
            Ok(Value::Null), // project/update reply
            Ok(json!({"sources": []})),
        ]);
        let (g, _rec) = harness(stub.clone());

        g.session_project("s1".to_string(), None);

        // Empty project id must travel as an explicit JSON null, never an omitted key.
        assert_calls(
            &stub,
            &[
                ("_goose/unstable/session/project/update", json!({"sessionId": "s1", "projectId": null})),
                ("_goose/unstable/sources/list", json!({"type": "project"})),
            ],
        );
    }

    #[test]
    fn config_read_emits_value_and_upsert_is_silent() {
        let _guard = TEST_LOCK.lock();
        let stub = StubConn::new();
        stub.script(vec![
            Ok(json!({"key": "model", "value": "claude-3-7-sonnet"})),
            Ok(Value::Null), // upsert reply
        ]);
        let (g, rec) = harness(stub.clone());

        g.config_read("model".to_string());
        g.config_upsert("mode".to_string(), "auto".to_string());

        assert_eq!(rec.events(), vec![Ev::ConfigValue("model".to_string(), "claude-3-7-sonnet".to_string())]);
        assert_calls(
            &stub,
            &[
                ("_goose/unstable/config/read", json!({"key": "model"})),
                ("_goose/unstable/config/upsert", json!({"key": "mode", "value": "auto"})),
            ],
        );
    }

    #[test]
    fn schedules_pause_and_run_now_relist_schedules() {
        let _guard = TEST_LOCK.lock();
        let stub = StubConn::new();
        stub.script(vec![
            Ok(Value::Null), // pause reply
            Ok(json!({"jobs": [{"id": "j1", "paused": true}]})),
            Ok(Value::Null), // run-now reply
            Ok(json!({"jobs": []})),
        ]);
        let (g, rec) = harness(stub.clone());

        g.schedules_pause("j1".to_string());
        g.schedules_run_now("j1".to_string());

        assert_eq!(
            rec.events(),
            vec![
                Ev::Schedules("[{\"id\":\"j1\",\"paused\":true}]".to_string()),
                Ev::Schedules("[]".to_string()),
            ]
        );
        assert_calls(
            &stub,
            &[
                ("_goose/unstable/schedules/pause", json!({"scheduleId": "j1"})),
                ("_goose/unstable/schedules/list", json!({})),
                ("_goose/unstable/schedules/run-now", json!({"scheduleId": "j1"})),
                ("_goose/unstable/schedules/list", json!({})),
            ],
        );
    }

    #[test]
    fn set_extension_enabled_and_add_relist_global_extensions() {
        let _guard = TEST_LOCK.lock();
        let stub = StubConn::new();
        stub.script(vec![
            Ok(Value::Null), // set-enabled reply
            Ok(json!({"extensions": [{"extension": {"name": "dev"}, "enabled": false}]})),
            Ok(Value::Null), // add reply
            Ok(json!({"extensions": []})),
        ]);
        let (g, rec) = harness(stub.clone());

        g.set_extension_enabled("dev".to_string(), false);
        g.add_extension(r#"{"name": "builtin://x"}"#.to_string(), true);

        assert_eq!(
            rec.events(),
            vec![
                Ev::Extensions("[{\"extension\":{\"name\":\"dev\"},\"enabled\":false}]".to_string()),
                Ev::Extensions("[]".to_string()),
            ]
        );
        assert_calls(
            &stub,
            &[
                ("_goose/unstable/config/extensions/set-enabled", json!({"name": "dev", "enabled": false})),
                ("_goose/unstable/config/extensions/list", json!({})),
                ("_goose/unstable/config/extensions/add", json!({"extension": {"name": "builtin://x"}, "enabled": true})),
                ("_goose/unstable/config/extensions/list", json!({})),
            ],
        );
    }

    #[test]
    fn session_extensions_add_relists_tools_and_extensions() {
        let _guard = TEST_LOCK.lock();
        let stub = StubConn::new();
        stub.script(vec![
            Ok(Value::Null), // add reply
            Ok(json!({"tools": [{"name": "shell__run"}]})),
            Ok(json!({"extensions": [{"name": "dev"}]})),
        ]);
        let (g, rec) = harness(stub.clone());

        g.session_extensions_add("s1".to_string(), r#"{"name": "dev"}"#.to_string());

        assert_eq!(
            rec.events(),
            vec![
                Ev::Tools("s1".to_string(), "[{\"name\":\"shell__run\"}]".to_string()),
                Ev::SessionExtensions("s1".to_string(), "[{\"name\":\"dev\"}]".to_string()),
            ]
        );
        assert_calls(
            &stub,
            &[
                ("_goose/unstable/session/extensions/add", json!({"sessionId": "s1", "extension": {"name": "dev"}})),
                ("_goose/unstable/tools/list", json!({"sessionId": "s1"})),
                ("_goose/unstable/session/extensions/list", json!({"sessionId": "s1"})),
            ],
        );
    }

    #[test]
    fn list_tools_emits_names_and_session_extensions_list_emits_payload() {
        let _guard = TEST_LOCK.lock();
        let stub = StubConn::new();
        stub.script(vec![
            Ok(json!({"tools": [{"name": "a__b"}, {"name": "c__d"}]})),
            Ok(json!({"extensions": [{"name": "e1"}, {"server": {"name": "e2"}}]})),
        ]);
        let (g, rec) = harness(stub.clone());

        g.list_tools("s1".to_string());
        g.session_extensions_list("s1".to_string());

        assert_eq!(
            rec.events(),
            vec![
                Ev::Tools("s1".to_string(), "[{\"name\":\"a__b\"},{\"name\":\"c__d\"}]".to_string()),
                Ev::SessionExtensions("s1".to_string(), "[{\"name\":\"e1\"},{\"server\":{\"name\":\"e2\"}}]".to_string()),
            ]
        );
    }

    // -----------------------------------------------------------------------
    // Session intents & direct tools
    // -----------------------------------------------------------------------

    #[test]
    fn steer_uses_active_session_and_expected_run_id() {
        let _guard = TEST_LOCK.lock();
        let stub = StubConn::new();
        stub.set_session("s1");
        stub.script(vec![Ok(Value::Null)]);
        let (g, _rec) = harness(stub.clone());

        g.steer("hello".to_string(), "run-9".to_string());

        assert_calls(
            &stub,
            &[(
                "_goose/unstable/session/steer",
                json!({"sessionId": "s1", "prompt": [{"type": "text", "text": "hello"}], "expectedRunId": "run-9"}),
            )],
        );
    }

    #[test]
    fn steer_without_conn_or_session_is_a_noop() {
        let _guard = TEST_LOCK.lock();
        spine::set_current_conn(None);
        let rec = Arc::new(RecordingListener::default());
        let g = GrouseUnstable::new(Box::new(rec.clone()));

        g.steer("hello".to_string(), "run-9".to_string());

        assert!(rec.events().is_empty(), "no conn -> nothing sent, no event");
    }

    #[test]
    fn steer_without_session_emits_not_ready_error() {
        let _guard = TEST_LOCK.lock();
        let stub = StubConn::new(); // no session bound
        let (g, rec) = harness(stub.clone());

        g.steer("hello".to_string(), "run-9".to_string());

        assert_eq!(
            rec.events(),
            vec![Ev::Error(
                "_goose/unstable/session/steer".to_string(),
                "not ready — no session".to_string()
            )]
        );
        assert!(stub.calls().is_empty(), "nothing may hit the wire without a session");
    }

    #[test]
    fn export_and_session_info_emit_payloads() {
        let _guard = TEST_LOCK.lock();
        let stub = StubConn::new();
        stub.script(vec![
            Ok(json!({"data": "{\"messages\":[]}"})),
            Ok(json!({"session": {"updatedAt": "2026-01-01", "_meta": {"messageCount": 3}}})),
        ]);
        let (g, rec) = harness(stub.clone());

        g.export_session("s1".to_string());
        g.session_info("s1".to_string());

        assert_eq!(
            rec.events(),
            vec![
                Ev::Export("{\"messages\":[]}".to_string()),
                Ev::SessionProbe("s1".to_string(), "2026-01-01".to_string(), 3),
            ]
        );
    }

    #[test]
    fn resources_read_extracts_first_text_and_clears_marker_on_error() {
        let _guard = TEST_LOCK.lock();
        let stub = StubConn::new();
        stub.script(vec![
            Ok(json!({"result": {"contents": [
                {"uri": "u", "mimeType": "text/html", "text": ""},
                {"uri": "u", "mimeType": "text/html", "text": "<html>app</html>"}
            ]}})),
            Err(err()),
        ]);
        let (g, rec) = harness(stub.clone());

        g.resources_read("s1".to_string(), "charts/sankey".to_string(), "autovisualiser".to_string());
        g.resources_read("s1".to_string(), "charts/radar".to_string(), "autovisualiser".to_string());

        assert_eq!(
            rec.events(),
            vec![
                Ev::AppResource("autovisualiser|charts/sankey".to_string(), "<html>app</html>".to_string()),
                // Failed fetch clears the in-flight marker with empty html — no error bubble.
                Ev::AppResource("autovisualiser|charts/radar".to_string(), String::new()),
            ]
        );
    }

    #[test]
    fn tools_call_emits_result_or_error() {
        let _guard = TEST_LOCK.lock();
        let stub = StubConn::new();
        stub.script(vec![
            Ok(json!({"content": [{"type": "text", "text": "out1"}, {"type": "text", "text": "out2"}]})),
            Err(err()),
        ]);
        let (g, rec) = harness(stub.clone());

        g.tools_call("s1".to_string(), "shell__run".to_string(), r#"{"cmd":"echo hi"}"#.to_string());
        g.tools_call("s1".to_string(), "shell__run".to_string(), r#"{"cmd":"boom"}"#.to_string());

        assert_eq!(
            rec.events(),
            vec![
                Ev::ToolResult("out1out2".to_string(), false),
                Ev::ToolResult("boom".to_string(), true),
            ]
        );
        assert_calls(
            &stub,
            &[
                ("_goose/unstable/tools/call", json!({"sessionId": "s1", "name": "shell__run", "arguments": {"cmd": "echo hi"}})),
                ("_goose/unstable/tools/call", json!({"sessionId": "s1", "name": "shell__run", "arguments": {"cmd": "boom"}})),
            ],
        );
    }

    #[test]
    fn rpc_failure_emits_on_error_for_foreground_intents() {
        let _guard = TEST_LOCK.lock();
        let stub = StubConn::new();
        stub.set_session("s1");
        stub.script(vec![Err(err())]);
        let (g, rec) = harness(stub.clone());

        g.steer("hello".to_string(), "run-9".to_string());

        match rec.events().as_slice() {
            [Ev::Error(method, message)] => {
                assert_eq!(method, "_goose/unstable/session/steer");
                assert!(message.contains("boom"), "error text must reach the UI: {message}");
            }
            other => panic!("expected one on_error, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Server-request answers (pure builders — the parked responder lives in spine)
    // -----------------------------------------------------------------------

    #[test]
    fn recipe_params_request_forwards_parameters() {
        let _guard = TEST_LOCK.lock();
        let rec = Arc::new(RecordingListener::default());

        GrouseUnstable::dispatch_server_request(
            &rec,
            "_goose/unstable/session/recipe/request-params",
            json!({"parameters": [{"key": "topic", "default": "goose", "input_type": "string"}]}),
        );

        assert_eq!(
            rec.events(),
            vec![Ev::RecipeParams(
                "[{\"key\":\"topic\",\"default\":\"goose\",\"input_type\":\"string\"}]".to_string()
            )],
            "the raw parameters array rides on_recipe_params so the UI can render a form"
        );
    }

    #[test]
    fn elicitation_request_forwards_schema() {
        let _guard = TEST_LOCK.lock();
        let rec = Arc::new(RecordingListener::default());

        GrouseUnstable::dispatch_server_request(
            &rec,
            "elicitation/create",
            json!({"mode": "form", "requestedSchema": {"title": "Ask", "properties": {}}}),
        );

        assert_eq!(
            rec.events(),
            vec![Ev::Elicitation(
                "{\"title\":\"Ask\",\"properties\":{}}".to_string()
            )],
            "the requestedSchema rides on_elicitation"
        );
    }

    #[test]
    fn goose_custom_notifications_forward_status_and_usage() {
        let _guard = TEST_LOCK.lock();
        let rec = Arc::new(RecordingListener::default());

        GrouseUnstable::dispatch_notification(
            &rec,
            "_goose/unstable/session/update",
            json!({"update": {"sessionUpdate": "status_message", "status": {"message": "compacting history…"}}}),
        );
        GrouseUnstable::dispatch_notification(
            &rec,
            "_goose/unstable/session/update",
            json!({"update": {"sessionUpdate": "message_usage", "usage": {
                "outputTokens": 120, "elapsedMs": 3400, "timeToFirstTokenMs": 220, "cost": 0.0042
            }}}),
        );
        // Direct/raw form (no {update: ...} wrapper) is tolerated, matching the desktop.
        GrouseUnstable::dispatch_notification(
            &rec,
            "_goose/unstable/session/update",
            json!({"sessionUpdate": "status_message", "status": {"message": "raw"}}),
        );
        // Other notifications are ignored by the shim.
        GrouseUnstable::dispatch_notification(
            &rec,
            "session/update",
            json!({"update": {"sessionUpdate": "agent_message_chunk"}}),
        );

        assert_eq!(
            rec.events(),
            vec![
                Ev::CompactionStatus("compacting history…".to_string()),
                Ev::MessageUsage(120, 3400, 220, 0.0042),
                Ev::CompactionStatus("raw".to_string()),
            ]
        );
    }

    #[test]
    fn recipe_params_answer_shape() {
        assert_eq!(
            GrouseUnstable::recipe_params_answer("submit", r#"{"param1":"v1"}"#),
            json!({"action": "submit", "values": {"param1": "v1"}})
        );
        assert_eq!(
            GrouseUnstable::recipe_params_answer("cancel", "not json"),
            json!({"action": "cancel", "values": {}}),
            "unparseable values degrade to an empty map, never a string"
        );
    }

    #[test]
    fn elicitation_answer_shape() {
        assert_eq!(
            GrouseUnstable::elicitation_answer("accept", r#"{"q1":"a1"}"#),
            json!({"action": "accept", "content": {"q1": "a1"}})
        );
        assert_eq!(
            GrouseUnstable::elicitation_answer("decline", ""),
            json!({"action": "decline"}),
            "decline/cancel carry no content object"
        );
    }
}
