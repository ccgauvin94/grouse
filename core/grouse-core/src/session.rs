// SPDX-License-Identifier: AGPL-3.0-or-later

//! Transport-independent session semantics: one place for the ACP parameters,
//! capability gates, wire-payload translation, and the SDK client wiring that
//! both the WebSocket spine and the roam peer use.
//!
//! Nothing here knows whether a session arrived over a WebSocket or an iroh
//! byte stream. A transport is only: where bytes come from, how a session id is
//! namespaced, and when the link is ready (see [`ChatEndpoint`]). Everything
//! else — `initialize` capabilities, `session/new|load|prompt` params,
//! `session/list` parsing, tool/chunk translation — lives here so a protocol
//! change cannot land in one transport and be forgotten in the other.

#![allow(clippy::type_complexity)] // The SDK handler table is intentionally a set of boxed closures.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::{Mutex, RwLock};

use agent_client_protocol::schema::v1::{
    ClientCapabilities, ElicitationCapabilities, ElicitationFormCapabilities,
    FileSystemCapabilities, Implementation, PermissionOptionKind, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome, SessionInfo,
    ToolCall, ToolCallStatus,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{
    on_receive_notification, on_receive_request, Agent, Client, ConnectTo, Error as AcpError,
    Responder,
};
use futures::future::BoxFuture;
use serde_json::{json, Map, Value};

use crate::{
    ConfigChoice, ConfigOption, PermissionOutcome, PermissionRequest, Prompt, PromptBlock,
    SessionSummary, ToolCallKind,
};

// ---------------------------------------------------------------------------
// Session id namespaces
// ---------------------------------------------------------------------------

/// The prefix that namespaces a peer's sessions (`CONTRACT §6`). The UI only
/// ever sees display ids; wire ids exist only inside an endpoint.
pub(crate) const ROAM_PREFIX: &str = "roam:";

/// Display id for a session owned by `label` (`None` = the main connection).
pub(crate) fn display_id(label: Option<&str>, wire: &str) -> String {
    match label {
        Some(label) => format!("{ROAM_PREFIX}{label}:{wire}"),
        None => wire.to_string(),
    }
}

// ---------------------------------------------------------------------------
// ACP parameters
// ---------------------------------------------------------------------------

/// `initialize`: protocol v1 plus the client capabilities goose gates on.
///
/// `mcpHostCapabilities.extensions["io.modelcontextprotocol/ui"]` makes the
/// server's `host_supports_mcp_apps()` gate true; without it goose strips
/// `_meta.goose.mcpApp` and every app degrades to a plain tool chip.
/// `recipeParameterRequests` is required for parameterized recipe sessions.
pub(crate) fn initialize_params() -> Value {
    json!({
        "protocolVersion": ProtocolVersion::V1,
        "clientInfo": {
            "name": "grouse",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "clientCapabilities": {
            "fs": { "readTextFile": false, "writeTextFile": false },
            "elicitation": { "form": {} },
            "_meta": {
                "goose": {
                    "customNotifications": true,
                    "recipeParameterRequests": true,
                    "toolCallLabelEnrichment": true,
                    "mcpHostCapabilities": {
                        "extensions": {
                            "io.modelcontextprotocol/ui": {
                                "mimeTypes": ["text/html;profile=mcp-app"]
                            }
                        }
                    }
                }
            }
        }
    })
}

/// `initialize` as a typed request (the SDK path). Built from the SAME
/// capability declarations as [`initialize_params`] so the two cannot drift.
pub(crate) fn initialize_request() -> agent_client_protocol::schema::v1::InitializeRequest {
    let mut fs = FileSystemCapabilities::default();
    fs.read_text_file = false;
    fs.write_text_file = false;
    let mut caps = ClientCapabilities::new()
        .fs(fs)
        .elicitation(ElicitationCapabilities::new().form(ElicitationFormCapabilities::default()));
    caps.meta = Some(goose_meta());
    agent_client_protocol::schema::v1::InitializeRequest::new(ProtocolVersion::V1)
        .client_capabilities(caps)
        .client_info(Implementation::new("grouse", env!("CARGO_PKG_VERSION")))
}

fn goose_meta() -> Map<String, Value> {
    let mut goose = Map::new();
    goose.insert("customNotifications".into(), Value::Bool(true));
    goose.insert("recipeParameterRequests".into(), Value::Bool(true));
    goose.insert("toolCallLabelEnrichment".into(), Value::Bool(true));
    goose.insert(
        "mcpHostCapabilities".into(),
        json!({ "extensions": { "io.modelcontextprotocol/ui": {
            "mimeTypes": ["text/html;profile=mcp-app"] } } }),
    );
    let mut meta = Map::new();
    meta.insert("goose".into(), Value::Object(goose));
    meta
}

/// `session/list`: user + ACP sessions only (the desktop's `listSessions`).
pub(crate) fn session_list_params() -> Value {
    json!({ "_meta": { "types": ["user", "acp"] } })
}

/// The same filter as a typed `ListSessionsRequest` meta map (the SDK's typed
/// request path).
pub(crate) fn session_list_meta() -> Map<String, Value> {
    let mut meta = Map::new();
    meta.insert(
        "types".into(),
        Value::Array(vec![
            Value::String("user".into()),
            Value::String("acp".into()),
        ]),
    );
    meta
}

/// `session/new`. `_meta.client` present ⇒ goose's `SessionType::User`, so the
/// desktop/CLI can see these chats. `recipeId` starts a recipe session;
/// `projectId` is persisted at creation (the stock-goose project bridge).
pub(crate) fn session_new_params(
    client_id: &str,
    cwd: &str,
    recipe_id: Option<&str>,
    project_id: Option<&str>,
) -> Value {
    let mut meta = Map::new();
    meta.insert("client".into(), Value::String(client_id.to_string()));
    if let Some(recipe) = recipe_id.filter(|r| !r.is_empty()) {
        meta.insert("recipeId".into(), Value::String(recipe.to_string()));
    }
    if let Some(project) = project_id.filter(|p| !p.is_empty()) {
        meta.insert("projectId".into(), Value::String(project.to_string()));
    }
    json!({
        "cwd": cwd,
        "mcpServers": [],
        "_meta": Value::Object(meta),
    })
}

/// `session/load`. `mcpServers` is REQUIRED by goose's deserializer. A
/// `replay_tail` bounds the replay (0 = server-defined minimum); callers pass
/// it only when the cached transcript provably covers the session's state.
pub(crate) fn session_load_params(wire_id: &str, cwd: &str, replay_tail: Option<usize>) -> Value {
    let mut params = json!({
        "sessionId": wire_id,
        "cwd": cwd,
        "mcpServers": [],
    });
    if let Some(tail) = replay_tail {
        params["_meta"] = json!({ "replayTail": tail });
    }
    params
}

/// `session/prompt`. `wire_id` is the endpoint's raw id.
pub(crate) fn prompt_params(prompt: &Prompt, wire_id: &str) -> Value {
    let blocks = prompt
        .blocks
        .iter()
        .map(|block| match block {
            PromptBlock::Text { text } => json!({ "type": "text", "text": text }),
            PromptBlock::Image { mime_type, data } => {
                json!({ "type": "image", "mimeType": mime_type, "data": data })
            }
            PromptBlock::Resource {
                uri,
                mime_type,
                text,
                blob,
            } => {
                let mut resource = json!({ "uri": uri, "mimeType": mime_type });
                if let Some(text) = text {
                    resource["text"] = Value::String(text.clone());
                }
                if let Some(blob) = blob {
                    resource["blob"] = Value::String(blob.clone());
                }
                json!({ "type": "resource", "resource": resource })
            }
        })
        .collect::<Vec<_>>();
    json!({ "sessionId": wire_id, "prompt": blocks })
}

/// Control RPCs are bounded; a prompt is not (the reply IS the turn's end).
/// One timeout table for every transport.
pub(crate) fn rpc_timeout(method: &str) -> Option<Duration> {
    match method {
        // The reply IS the turn's end; a long agent run is not a hang.
        "session/prompt" => None,
        // A full replay of a long session can take minutes; the transport's
        // own failure path reports a dead link.
        "session/load" => None,
        _ => Some(Duration::from_secs(30)),
    }
}

/// One request with the shared timeout policy. A prompt waits for the turn;
/// every control call is bounded so a hung peer cannot pin an intent.
pub(crate) async fn request(
    owner: &dyn ChatEndpoint,
    method: &str,
    params: Value,
) -> Result<Value, AcpError> {
    let fut = owner.request(method, params);
    match rpc_timeout(method) {
        Some(limit) => tokio::time::timeout(limit, fut)
            .await
            .map_err(|_| AcpError::internal_error().data(format!("{method} timed out")))?,
        None => fut.await,
    }
}

// ---------------------------------------------------------------------------
// Wire → record translation (one implementation, both transports)
// ---------------------------------------------------------------------------

/// Canonical text rendering for a content block. Text passes through;
/// non-text blocks become bracketed markers.
pub(crate) fn content_block_text(
    block: &agent_client_protocol::schema::v1::ContentBlock,
) -> String {
    use agent_client_protocol::schema::v1::ContentBlock;
    match block {
        ContentBlock::Text(text) => text.text.clone(),
        ContentBlock::ResourceLink(_) | ContentBlock::Resource(_) => "[resource]".to_string(),
        ContentBlock::Image(_) => "[image]".to_string(),
        ContentBlock::Audio(_) => "[audio]".to_string(),
        _ => "[unknown]".to_string(),
    }
}

pub(crate) fn chunk_text(chunk: &agent_client_protocol::schema::v1::ContentChunk) -> String {
    content_block_text(&chunk.content)
}

/// The chunk's image payload as a renderable data URI, if it IS an image.
/// The base64 body rides the chunk, so these can be item pixels verbatim.
pub(crate) fn content_image_uri(
    block: &agent_client_protocol::schema::v1::ContentBlock,
) -> Option<String> {
    use agent_client_protocol::schema::v1::ContentBlock;
    if let ContentBlock::Image(image) = block {
        Some(format!("data:{};base64,{}", image.mime_type, image.data))
    } else {
        None
    }
}

/// Map a wire `ToolCall` to the contract's `ToolCallKind` plus the tool's
/// command detail.
pub(crate) fn tool_call_kind(tc: &ToolCall) -> (ToolCallKind, String) {
    let raw_input = tc.raw_input.clone().unwrap_or(Value::Null);
    let detail = raw_input
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let goose = tc
        .meta
        .as_ref()
        .and_then(|meta| meta.get("goose"))
        .cloned()
        .unwrap_or(Value::Null);

    // MCP-App path: the server names a UI resource for this tool's output.
    // Until the template is fetched it is still a plain chip.
    let mcp_app = goose.get("mcpApp").cloned().unwrap_or(Value::Null);
    let app_uri = mcp_app
        .get("resourceUri")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let app_ext = mcp_app
        .get("extensionName")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let kind = if !app_uri.is_empty() && !app_ext.is_empty() {
        ToolCallKind::McpApp {
            app_key: format!("{app_ext}|{app_uri}"),
            uri: app_uri,
            extension: app_ext,
            input: compact_json(&raw_input),
        }
    } else {
        // Legacy chart fallback (server too old to send mcpApp meta).
        let tool_name = goose
            .pointer("/toolCall/toolName")
            .and_then(Value::as_str)
            .unwrap_or("");
        let chart_data = raw_input
            .get("data")
            .map(|data| match data {
                Value::Object(_) => compact_json(data),
                Value::String(s) => s.clone(),
                _ => String::new(),
            })
            .unwrap_or_default();
        if tool_name == "autovisualiser__show_chart" && !chart_data.is_empty() {
            ToolCallKind::Chart { spec: chart_data }
        } else {
            ToolCallKind::Plain
        }
    };
    (kind, detail)
}

/// The MCP-App identity a `tool_call_update` hydrates, if any. goose only
/// knows a completed call produced an app once the extension's `ui://`
/// resource resolved, so this is read from the UPDATE, not the call.
pub(crate) fn tool_call_app(
    update: &agent_client_protocol::schema::v1::ToolCallUpdate,
) -> Option<(String, String)> {
    let app = update.meta.as_ref()?.get("goose")?.get("mcpApp")?;
    let uri = app.get("resourceUri")?.as_str()?;
    let extension = app.get("extensionName")?.as_str()?;
    (!uri.is_empty() && !extension.is_empty()).then(|| (uri.to_string(), extension.to_string()))
}

/// The live-output chunk on a `tool_call_update` (`_meta.toolNotification`
/// with `type: live_output`).
pub(crate) fn tool_live_output(
    update: &agent_client_protocol::schema::v1::ToolCallUpdate,
) -> Option<String> {
    let notif = update.meta.as_ref()?.get("toolNotification")?.as_object()?;
    if notif.get("type").and_then(Value::as_str) != Some("live_output") {
        return None;
    }
    let mut chunk = String::new();
    for part in notif.get("params")?.get("chunks")?.as_array()? {
        if let Some(text) = part
            .get("output")
            .and_then(|o| o.get("text"))
            .and_then(Value::as_str)
        {
            chunk.push_str(text);
        }
    }
    Some(chunk)
}

/// The child tool call a SUBAGENT just made, if this update carries one.
///
/// goose keeps subagent sessions out of ACP (`ACP_VISIBLE_SESSION_TYPES`), so the
/// only view of the child's work is what it forwards onto the parent's delegate
/// row: `_meta.toolNotification` with `type: message`, a `subagent:<id>` logger,
/// and `data.type == "subagent_tool_request"` holding the child's tool name and
/// arguments.
pub(crate) fn tool_subagent_step(
    update: &agent_client_protocol::schema::v1::ToolCallUpdate,
) -> Option<crate::SubagentStep> {
    let notif = update.meta.as_ref()?.get("toolNotification")?.as_object()?;
    if notif.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let params = notif.get("params")?.as_object()?;
    let logger = params.get("logger").and_then(Value::as_str)?;
    if !logger.starts_with("subagent:") {
        return None;
    }
    let data = params.get("data")?.as_object()?;
    if data.get("type").and_then(Value::as_str) != Some("subagent_tool_request") {
        return None;
    }
    let call = data.get("tool_call")?.as_object()?;
    let name = call
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Some(crate::SubagentStep {
        name,
        detail: summarize_tool_args(call.get("arguments")),
    })
}

/// One line of a tool call's arguments, for a status list where the full JSON
/// would be unreadable: the first useful scalar (a command, a path, a query), else
/// a compact rendering, truncated.
fn summarize_tool_args(args: Option<&Value>) -> String {
    const MAX: usize = 120;
    let Some(args) = args.and_then(Value::as_object) else {
        return String::new();
    };
    for key in ["command", "path", "file_path", "query", "url", "title", "name"] {
        if let Some(value) = args.get(key).and_then(Value::as_str) {
            if !value.is_empty() {
                return truncate_line(value, MAX);
            }
        }
    }
    let compact = Value::Object(args.clone()).to_string();
    truncate_line(&compact, MAX)
}

fn truncate_line(text: &str, max: usize) -> String {
    let flat = text.replace('\n', " ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The final text content of a completed `tool_call_update`.
pub(crate) fn tool_final_output(
    update: &agent_client_protocol::schema::v1::ToolCallUpdate,
) -> String {
    use agent_client_protocol::schema::v1::{ContentBlock, ToolCallContent};
    let mut outputs = Vec::new();
    for entry in update.fields.content.clone().unwrap_or_default() {
        if let ToolCallContent::Content(content) = entry {
            if let ContentBlock::Text(text) = &content.content {
                outputs.push(text.text.clone());
            }
        }
    }
    outputs.join("\n")
}

pub(crate) fn tool_call_status_str(status: &ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "pending",
        ToolCallStatus::InProgress => "in_progress",
        ToolCallStatus::Completed => "completed",
        ToolCallStatus::Failed => "failed",
        _ => "",
    }
}

pub(crate) fn permission_option_kind_str(kind: &PermissionOptionKind) -> &'static str {
    match kind {
        PermissionOptionKind::AllowOnce => "allow_once",
        PermissionOptionKind::AllowAlways => "allow_always",
        PermissionOptionKind::RejectOnce => "reject_once",
        PermissionOptionKind::RejectAlways => "reject_always",
        _ => "",
    }
}

pub(crate) fn config_option_value(
    option: &agent_client_protocol::schema::v1::SessionConfigOption,
) -> String {
    use agent_client_protocol::schema::v1::SessionConfigKind;
    match &option.kind {
        SessionConfigKind::Select(select) => select.current_value.to_string(),
        SessionConfigKind::Boolean(boolean) => boolean.current_value.to_string(),
        _ => String::new(),
    }
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// Parse a reply's `configOptions`. Selectable values arrive under `options`
/// (never `choices`); preserving them across partial updates is the caller's
/// merge job.
pub(crate) fn parse_config_options(result: &Value) -> Vec<ConfigOption> {
    result
        .get("configOptions")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|option| {
                    let obj = option.as_object()?;
                    let id = obj.get("id")?.as_str()?.to_string();
                    let value = match obj.get("currentValue") {
                        Some(Value::String(s)) => s.clone(),
                        Some(Value::Bool(b)) => b.to_string(),
                        Some(Value::Number(n)) => n.to_string(),
                        _ => String::new(),
                    };
                    let name = obj
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let choices = obj
                        .get("options")
                        .and_then(Value::as_array)
                        .map(|choices| {
                            choices
                                .iter()
                                .filter_map(|c| {
                                    let c = c.as_object()?;
                                    Some(ConfigChoice {
                                        value: c.get("value")?.as_str()?.to_string(),
                                        name: c
                                            .get("name")
                                            .and_then(Value::as_str)
                                            .unwrap_or("")
                                            .to_string(),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    Some(ConfigOption {
                        id,
                        value,
                        name,
                        choices,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The fields every session summary shares. `lastMessageAt` takes precedence
/// over `updatedAt`: goose bumps `updatedAt` on every load (merely selecting a
/// chat), so ordering/freshness must follow message activity only.
pub(crate) fn summary_from_fields(
    id: String,
    title: Option<&str>,
    updated_at: Option<&str>,
    cwd: &str,
    meta: Option<&Map<String, Value>>,
) -> Option<SessionSummary> {
    let title = title.unwrap_or("");
    if title.is_empty() && id.is_empty() {
        return None;
    }
    if title.starts_with("Scheduled job:") {
        return None;
    }
    let snippet = meta
        .and_then(|m| m.get("lastMessageSnippet"))
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let project_id = meta
        .and_then(|m| m.get("projectId"))
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let message_count = meta
        .and_then(|m| m.get("messageCount"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let model = meta
        .and_then(|m| m.get("model"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let has_recipe = meta
        .and_then(|m| m.get("hasRecipe"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let archived = meta.map(|m| m.contains_key("archivedAt")).unwrap_or(false);
    let last_message = meta
        .and_then(|m| m.get("lastMessageAt"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let updated = if last_message.is_empty() {
        updated_at.unwrap_or("").to_string()
    } else {
        last_message.to_string()
    };
    let _ = cwd; // carried by the caller's own side table
    let display_title = if title.is_empty() {
        id.clone()
    } else {
        title.to_string()
    };
    Some(SessionSummary {
        id,
        title: display_title,
        updated_at: updated,
        last_message_snippet: snippet,
        project_id,
        message_count,
        model,
        has_recipe,
        has_new: false,
        archived,
    })
}

/// Parse a `session/list` reply into summaries plus the side tables the core
/// keeps: cwd for resume resolution and updatedAt for cache freshness.
pub(crate) fn parse_sessions(
    result: &Value,
) -> (
    Vec<SessionSummary>,
    Vec<(String, String)>,
    Vec<(String, String)>,
) {
    let mut sessions = Vec::new();
    let mut cwds = Vec::new();
    let mut updated_at = Vec::new();
    let Some(list) = result.get("sessions").and_then(Value::as_array) else {
        return (sessions, cwds, updated_at);
    };
    for entry in list {
        let Some(obj) = entry.as_object() else {
            continue;
        };
        let Some(session_id) = obj.get("sessionId").and_then(Value::as_str) else {
            continue;
        };
        let cwd = obj.get("cwd").and_then(Value::as_str).unwrap_or("");
        let meta = obj.get("_meta").and_then(Value::as_object);
        let Some(summary) = summary_from_fields(
            session_id.to_string(),
            obj.get("title").and_then(Value::as_str),
            obj.get("updatedAt").and_then(Value::as_str),
            cwd,
            meta,
        ) else {
            continue;
        };
        if !cwd.is_empty() {
            cwds.push((session_id.to_string(), cwd.to_string()));
        }
        if !summary.updated_at.is_empty() {
            updated_at.push((session_id.to_string(), summary.updated_at.clone()));
        }
        sessions.push(summary);
    }
    (sessions, cwds, updated_at)
}

/// One typed `session/list` entry → summary (the peer's typed reply path).
pub(crate) fn summary_from_info(label: Option<&str>, info: &SessionInfo) -> Option<SessionSummary> {
    let wire = info.session_id.to_string();
    summary_from_fields(
        display_id(label, &wire),
        info.title.as_deref(),
        info.updated_at.as_deref(),
        &info.cwd.to_string_lossy(),
        info.meta.as_ref(),
    )
}

/// The cwd a typed `session/list` entry carries.
pub(crate) fn info_cwd(info: &SessionInfo) -> String {
    info.cwd.to_string_lossy().to_string()
}

// ---------------------------------------------------------------------------
// Permission requests
// ---------------------------------------------------------------------------

/// The flat permission prompt for a wire request.
pub(crate) fn permission_request(req: &RequestPermissionRequest) -> PermissionRequest {
    PermissionRequest {
        tool_call_id: req.tool_call.tool_call_id.to_string(),
        title: req.tool_call.fields.title.clone().unwrap_or_default(),
        detail: req
            .tool_call
            .fields
            .raw_input
            .as_ref()
            .and_then(|raw| raw.get("command"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        options: req
            .options
            .iter()
            .map(|option| crate::PermissionOption {
                option_id: option.option_id.to_string(),
                name: option.name.clone(),
                kind: permission_option_kind_str(&option.kind).to_string(),
            })
            .collect(),
    }
}

/// The wire outcome for a flat permission answer.
pub(crate) fn permission_response(outcome: PermissionOutcome) -> RequestPermissionResponse {
    let outcome = match outcome {
        PermissionOutcome::Selected { option_id } => {
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                agent_client_protocol::schema::v1::PermissionOptionId::new(option_id),
            ))
        }
        PermissionOutcome::Cancelled => RequestPermissionOutcome::Cancelled,
    };
    RequestPermissionResponse::new(outcome)
}

// ---------------------------------------------------------------------------
// The SDK client: one handler table, whatever the transport
// ---------------------------------------------------------------------------

/// The handlers every connection must register. Both transports build the
/// same client from these, so a custom notification or server request can
/// never be handled on one wire and dropped on the other.
#[derive(Clone)]
pub(crate) struct ClientHandlers {
    pub notification: std::sync::Arc<
        dyn Fn(agent_client_protocol::schema::v1::SessionNotification) + Send + Sync,
    >,
    pub custom_notification: std::sync::Arc<dyn Fn(&str, Value) + Send + Sync>,
    pub permission: std::sync::Arc<
        dyn Fn(RequestPermissionRequest, Responder<RequestPermissionResponse>) + Send + Sync,
    >,
    pub request: std::sync::Arc<dyn Fn(&str, &Value, Responder<Value>) + Send + Sync>,
}

/// Run a connection with the shared handler table. `foreground` runs the
/// handshake and then serves until the link ends; it is the only
/// transport-specific part of the client.
#[allow(clippy::redundant_closure)] // `connect_with` needs an `AsyncFnOnce`, not a plain fn item
pub(crate) async fn connect_client<T, F>(
    transport: T,
    handlers: ClientHandlers,
    foreground: F,
) -> Result<(), AcpError>
where
    T: ConnectTo<Client> + 'static,
    F: FnOnce(
            agent_client_protocol::ConnectionTo<Agent>,
        ) -> BoxFuture<'static, Result<(), AcpError>>
        + Send
        + 'static,
{
    let ClientHandlers {
        notification,
        custom_notification,
        permission,
        request,
    } = handlers;
    Client
        .builder()
        .name("grouse")
        // Typed handler claims `session/update`; the untyped one below claims
        // every other notification (handlers are tried in registration order).
        .on_receive_notification(
            async move |notif, _cx| {
                notification(notif);
                Ok(())
            },
            on_receive_notification!(),
        )
        .on_receive_notification(
            async move |notif: agent_client_protocol::UntypedMessage, _cx| {
                custom_notification(&notif.method, notif.params.clone());
                Ok(())
            },
            on_receive_notification!(),
        )
        .on_receive_request(
            async move |req: RequestPermissionRequest, responder, _cx| {
                permission(req, responder);
                Ok(())
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: agent_client_protocol::UntypedMessage, responder, _cx| {
                request(&req.method, &req.params, responder);
                Ok(())
            },
            on_receive_request!(),
        )
        // The SDK's `connect_with` requires an `AsyncFnOnce`; a plain fn item
        // returning a boxed future does not satisfy it.
        .connect_with(transport, move |cx| foreground(cx))
        .await
}

#[allow(clippy::items_after_test_module)] // kept next to the helpers it exercises
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ConnectionStatus, CoreListener, PermissionRequest, ProjectSummary, SessionSummary,
        TranscriptOp,
    };
    use std::sync::Mutex as StdMutex;

    #[test]
    fn a_subagent_notification_becomes_one_step_on_the_delegate_row() {
        use agent_client_protocol::schema::v1::{
            ToolCallId, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
        };
        use serde_json::json;

        let mut meta = agent_client_protocol::schema::v1::Meta::new();
        meta.insert(
            "toolNotification".to_string(),
            json!({
                "type": "message",
                "params": {
                    "level": "info",
                    "logger": "subagent:abc",
                    "data": {
                        "type": "subagent_tool_request",
                        "subagent_id": "abc",
                        "tool_call": {
                            "name": "developer__shell",
                            "arguments": { "command": "git status --short" }
                        }
                    }
                }
            }),
        );
        let update = ToolCallUpdate::new(
            ToolCallId::new("delegate-1"),
            ToolCallUpdateFields::new().status(ToolCallStatus::InProgress),
        )
        .meta(meta);

        let step = tool_subagent_step(&update).expect("a subagent step");
        assert_eq!(step.name, "developer__shell");
        assert_eq!(step.detail, "git status --short");
    }

    #[test]
    fn a_session_info_update_declares_one_of_three_run_states() {
        // goose's `activeRunId` is three-state and the last two must not be
        // conflated: a generated session-name update says NOTHING about runs,
        // while `null` is the server stating the run is over (the end-of-turn
        // signal a client still receives when the prompt response is lost).
        use agent_client_protocol::schema::v1::{Meta, SessionInfoUpdate};

        fn meta(value: serde_json::Value) -> Meta {
            value.as_object().cloned().expect("meta")
        }

        let live = SessionInfoUpdate::new().meta(meta(json!({ "goose": { "activeRunId": "run-9" } })));
        assert_eq!(declared_run_state(&live).as_deref(), Some("run-9"));

        let ended =
            SessionInfoUpdate::new().meta(meta(json!({ "goose": { "activeRunId": null } })));
        assert_eq!(
            declared_run_state(&ended).as_deref(),
            Some(""),
            "an explicit null is an end, not silence"
        );

        let name_only =
            SessionInfoUpdate::new().title("Grouse").meta(meta(json!({ "messageCount": 12 })));
        assert_eq!(
            declared_run_state(&name_only),
            None,
            "a title update must not be read as a run end"
        );
    }

    #[test]
    fn ordinary_tool_notifications_are_not_mistaken_for_subagent_steps() {
        use agent_client_protocol::schema::v1::{
            ToolCallId, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
        };
        use serde_json::json;

        // A shell live-output chunk rides the same `_meta` channel and must not
        // show up as child activity.
        let mut meta = agent_client_protocol::schema::v1::Meta::new();
        meta.insert(
            "toolNotification".to_string(),
            json!({ "type": "live_output", "params": { "chunks": [] } }),
        );
        let update = ToolCallUpdate::new(
            ToolCallId::new("call-1"),
            ToolCallUpdateFields::new().status(ToolCallStatus::InProgress),
        )
        .meta(meta);
        assert!(tool_subagent_step(&update).is_none());

        // And a plain log line from some other logger is not a step either.
        let mut meta = agent_client_protocol::schema::v1::Meta::new();
        meta.insert(
            "toolNotification".to_string(),
            json!({ "type": "message", "params": { "level": "info", "logger": "app", "data": {} } }),
        );
        let update = ToolCallUpdate::new(
            ToolCallId::new("call-2"),
            ToolCallUpdateFields::new().status(ToolCallStatus::InProgress),
        )
        .meta(meta);
        assert!(tool_subagent_step(&update).is_none());
    }

    struct Recorder {
        events: StdMutex<Vec<String>>,
    }

    impl CoreListener for Recorder {
        fn on_status(&self, _: ConnectionStatus) {}
        fn on_sessions(&self, _: Vec<SessionSummary>) {}
        fn on_item(&self, op: TranscriptOp) {
            let line = match op {
                TranscriptOp::Reset { .. } => "item Reset".to_string(),
                TranscriptOp::Upsert { item } => format!("item Upsert {}", item.id),
                TranscriptOp::AppendText { id, .. } => format!("item AppendText {id}"),
                TranscriptOp::AppendOutput { id, .. } => format!("item AppendOutput {id}"),
                TranscriptOp::Remove { id } => format!("item Remove {id}"),
                TranscriptOp::Window { .. } => "item Window".to_string(),
            };
            self.events.lock().unwrap().push(line);
        }
        fn on_usage(&self, _: i64, _: i64, _: f64, _: String) {}
        fn on_run_ended(&self, _session: String, stop: String) {
            self.events
                .lock()
                .unwrap()
                .push(format!("run_ended {stop}"));
        }
        fn on_speak(&self, _: String) {}
        fn on_config(&self, _: Vec<ConfigOption>) {}
        fn on_permission_request(&self, _: PermissionRequest) {}
        fn on_session_touched(&self, id: String, _: String, _: String) {
            self.events.lock().unwrap().push(format!("touched {id}"));
        }
        fn on_projects(&self, _: Vec<ProjectSummary>) {}
        fn on_roam_peer_status(&self, _: String, _: String) {}
        fn on_roam_sessions(&self, _: String, _: Vec<SessionSummary>) {}
        fn on_peer_new_session(&self, _: String, _: String) {}
        fn on_active_run(&self, _: String, _: String) {}
        fn on_commands(&self, _: Vec<String>) {}
        fn on_session_open_failed(&self, _: String, _: String) {}
    }

    fn hub() -> (Arc<SessionHub>, Arc<Recorder>) {
        let recorder = Arc::new(Recorder {
            events: StdMutex::new(Vec::new()),
        });
        let listener: Arc<dyn CoreListener> = recorder.clone();
        (SessionHub::new(listener), recorder)
    }

    #[test]
    fn hub_paints_only_the_displayed_session() {
        let (hub, recorder) = hub();
        let background = hub.store("roam:p:s1");
        background.append_chunk("agent", "background", Some("m1"), false);
        assert!(
            recorder.events.lock().unwrap().is_empty(),
            "background must not paint"
        );

        hub.display("roam:p:s1");
        background.append_chunk("agent", " live", Some("m1"), false);
        assert!(recorder
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.starts_with("item ")));
    }

    #[test]
    fn hub_marks_a_background_session_once() {
        let (hub, recorder) = hub();
        hub.store("roam:p:s1");
        hub.mark_new("roam:p:s1");
        hub.mark_new("roam:p:s1");
        let count = recorder
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.as_str() == "touched roam:p:s1")
            .count();
        assert_eq!(count, 1, "the green dot is set once per burst");
        assert!(hub.has_new("roam:p:s1"));
        hub.display("roam:p:s1");
        assert!(!hub.has_new("roam:p:s1"), "displaying clears the dot");
    }

    #[test]
    fn paint_plan_respects_stamp_freshness() {
        let dir = std::env::temp_dir().join(format!(
            "grouse-session-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cache = crate::cache::CacheStore::new(dir);
        let item = crate::Item {
            id: "m1".into(),
            kind: crate::ItemKind::Agent,
            text: "hi".into(),
            detail: String::new(),
            output: String::new(),
            seq: 0,
            attachments: 0,
            images: Vec::new(),
            message_id: String::new(),
            status: String::new(),
            app_key: String::new(),
            calls: Vec::new(),
            thought: String::new(),
            summary: String::new(),
            steps: Vec::new(),
        };
        cache.save_transcript("s1", &[item], "t1");

        let fresh = plan_paint(&cache, "s1", "t1", false, false);
        assert!(fresh.suppress && !fresh.merge);

        let stale = plan_paint(&cache, "s1", "t2", false, false);
        assert!(!stale.suppress && stale.merge);

        let forced = plan_paint(&cache, "s1", "t1", true, false);
        assert!(!forced.suppress && !forced.merge && forced.items.is_some());

        let missing = plan_paint(&cache, "nope", "t1", false, false);
        assert!(!missing.suppress && !missing.merge && missing.items.is_none());
    }
}

// ---------------------------------------------------------------------------
// One transmit surface
// ---------------------------------------------------------------------------

/// What an intent needs from a live connection. The spine (WebSocket) and a
/// roam peer (iroh) differ only in how bytes travel and how ids are namespaced;
/// every session intent goes through this.
pub(crate) trait ChatEndpoint: Send + Sync {
    /// A request on this endpoint's wire. The caller applies the timeout
    /// policy ([`rpc_timeout`]); a prompt waits for the turn to finish.
    fn request(&self, method: &str, params: Value) -> BoxFuture<'static, Result<Value, AcpError>>;
    fn notify(&self, method: &str, params: Value) -> Result<(), AcpError>;
    /// The wire id for a display id (identity or prefix-stripped).
    fn wire_id(&self, display: &str) -> String;
    /// Ready to accept a prompt now (handshake complete). Queues otherwise.
    fn accepts_prompt(&self) -> bool;
    /// `None` for the main connection; the peer label otherwise.
    fn label(&self) -> Option<String>;
    /// The session's own working directory, when the endpoint knows it (a
    /// peer's `session/list` carries it; the main path resolves it centrally).
    fn session_cwd(&self, _display: &str) -> Option<String> {
        None
    }
    /// Clear any per-load replay gates before a full in-place rebuild.
    fn reset_replay_gates(&self) {}
}

/// Trailing messages requested from `session/load` for a stale-but-settled
/// cache. Counted in server-side messages (tool requests/responses included);
/// the server widens to the nearest turn boundary. Large enough that the
/// replayed window always overlaps the cached tail it merges against.
pub(crate) const REPLAY_TAIL: usize = 100;

// ---------------------------------------------------------------------------
// Cache paint policy (one decision for both transports)
// ---------------------------------------------------------------------------

/// How to paint a session's cached transcript and which wire load it owes.
pub(crate) struct PaintPlan {
    /// The cache is a settled, up-to-date copy: paint authoritatively and
    /// suppress the replay.
    pub suppress: bool,
    /// The cache is settled but stale: paint and merge a bounded tail.
    pub merge: bool,
    pub items: Option<Vec<crate::Item>>,
}

/// Decide the paint from the cache stamp alone. `force` (a full refresh)
/// ignores freshness; `incomplete` (a store known to hold a cut-off turn) may
/// not anchor a merge.
pub(crate) fn plan_paint(
    cache: &crate::cache::CacheStore,
    key: &str,
    listed_at: &str,
    force: bool,
    incomplete: bool,
) -> PaintPlan {
    match cache.load_transcript(key) {
        Some((items, cached_at)) => {
            let settled = !cached_at.is_empty();
            // An incomplete store may not be trusted as current either: a
            // dropped turn's cache is written with an empty stamp, but a
            // mid-turn save could still carry the server's stamp.
            let fresh =
                !force && !incomplete && settled && !listed_at.is_empty() && cached_at == listed_at;
            PaintPlan {
                suppress: fresh,
                merge: !force && settled && !fresh && !incomplete,
                items: Some(items),
            }
        }
        None => PaintPlan {
            suppress: false,
            merge: false,
            items: None,
        },
    }
}

/// Paint a plan into a store (the one implementation of the three modes).
pub(crate) fn apply_paint(store: &crate::transcript::TranscriptStore, plan: &PaintPlan) {
    match &plan.items {
        Some(items) if plan.suppress => store.replace_rich(items.clone(), false),
        Some(items) if plan.merge => store.replace_rich_for_merge(items.clone()),
        Some(items) => store.replace_rich(items.clone(), true),
        None => store.clear(),
    }
}

/// Open a session: paint its plan, then replay. ONE implementation for every
/// wire, and the load slot is claimed for the whole span — a paint that landed
/// while another load was mid-flight would leave that load's rows appended to a
/// transcript they do not belong to (see [`TranscriptStore::load_gate`]).
///
/// This is the entry point for a transport that decides paint and load
/// together (the roam peer, the remote-change resync). The main wire paints
/// before it dials, so its handshake calls [`replay_session`] directly.
pub(crate) async fn load_with_plan(
    endpoint: &dyn ChatEndpoint,
    host: &dyn DispatchHost,
    store: &crate::transcript::TranscriptStore,
    key: &str,
    cwd: &str,
    plan: &PaintPlan,
) -> Result<Value, AcpError> {
    let _load = store.begin_load().await;
    apply_paint(store, plan);
    replay_held(endpoint, host, store, key, cwd, plan.suppress, plan.merge).await
}

/// The `session/load` replay lifecycle after the paint — one implementation for
/// every wire.
///
/// The caller paints first (`apply_paint`) and owns everything transport-shaped
/// around this: binding the session id, surfacing config, reporting an open
/// failure. What lives HERE is only the part that must be identical everywhere:
///
///   1. begin the replay,
///   2. hold emissions, so a tail lands as ONE repaint instead of the history
///      streaming in from the top (grouse#18),
///   3. request the load with the tail the plan owes,
///   4. close the merge (`end_merge`): on a gap the cached prefix does not meet
///      the replayed tail, so clear and load the whole transcript,
///   5. release the hold and disarm the replay.
///
/// This used to be hand-written once per transport, and the copies drifted: the
/// peer's lost steps 2 and 4, so a reopened roam chat appended its replayed tail
/// on top of a silently truncated cache paint — recent prompts twice, the second
/// copy at the bottom. Anything sequenced around a `session/load` belongs in
/// this function, not in a transport.
///
/// `key` is the DISPLAY id; the endpoint strips its own prefix (`wire_id`).
pub(crate) async fn replay_session(
    endpoint: &dyn ChatEndpoint,
    host: &dyn DispatchHost,
    store: &crate::transcript::TranscriptStore,
    key: &str,
    cwd: &str,
    suppress: bool,
    merge: bool,
) -> Result<Value, AcpError> {
    let _load = store.begin_load().await;
    replay_held(endpoint, host, store, key, cwd, suppress, merge).await
}

/// [`replay_session`]'s body, for callers already holding the load slot.
async fn replay_held(
    endpoint: &dyn ChatEndpoint,
    host: &dyn DispatchHost,
    store: &crate::transcript::TranscriptStore,
    key: &str,
    cwd: &str,
    suppress: bool,
    merge: bool,
) -> Result<Value, AcpError> {
    // A fresh cache asks for the minimum tail; a stale one for a bounded tail
    // it merges; otherwise the full replay.
    let tail = if suppress {
        Some(1)
    } else if merge {
        Some(REPLAY_TAIL)
    } else {
        None
    };
    host.begin_replay(key, suppress);
    store.hold_emissions(true);
    let reply = endpoint
        .request("session/load", session_load_params(key, cwd, tail))
        .await;
    let reply = match reply {
        Ok(reply) => reply,
        Err(error) => {
            // Discard the hold WITHOUT painting: the rows on screen are the
            // provisional cache paint, which is the right thing to keep for a
            // chat that failed to load.
            store.hold_emissions(false);
            host.end_replay(key);
            return Err(error);
        }
    };
    host.end_replay(key);
    // Merge gap: the tail started past the cached prefix, so extending the
    // cache would leave a hole. Clear and load the whole transcript now.
    if store.end_merge() {
        store.clear();
        let full = session_load_params(key, cwd, None);
        let _ = endpoint.request("session/load", full).await;
    }
    // The replay is in: paint the newest window (or the empty transcript) once.
    store.release_held_replay();
    Ok(reply)
}

// ---------------------------------------------------------------------------
// One notification pipeline for every transport
// ---------------------------------------------------------------------------

/// What a connection asks the core to do with a `session/update`. The spine
/// and a roam peer both hand their notifications to [`dispatch`] with an
/// implementation of this; the transport is only `display_id` + the wire.
pub(crate) trait DispatchHost: Send + Sync {
    /// Display id for a wire session id (identity, or the peer prefix).
    fn display_id(&self, wire: &str) -> String;
    /// The store for a display id.
    fn store(&self, key: &str) -> Arc<crate::transcript::TranscriptStore>;
    /// Session metadata (`session_info_update`).
    fn touched(&self, key: String, title: String, updated_at: String);
    /// Run start (non-empty id) or end (empty id).
    fn active_run(&self, key: String, run_id: String);
    /// The wire carrying `key`'s turn is gone, so nothing on it can ever end
    /// that turn. Ending it is the transport's call, not a timeout's: only the
    /// connection knows the reply it was waiting for can no longer arrive.
    fn turn_lost(&self, key: &str);
    /// A config snapshot (only while `key` is displayed).
    fn config(&self, key: &str, options: Vec<ConfigOption>);
    fn usage(&self, key: &str, used: i64, size: i64, cost: f64, currency: String);
    fn commands(&self, key: &str, names: Vec<String>);
    /// A permission prompt (only while `key` is displayed).
    fn permission(&self, key: &str, request: PermissionRequest);
    /// A `session/load` replay is beginning for `key`. `suppress` marks a load
    /// whose rows must be DISCARDED: the cache it was planned from is current,
    /// so the minimal tail the server replays is already on screen and letting
    /// it land would append a second copy of the last turn (measured on the
    /// peer, which had no such gate: +64 rows on every fresh-cache open).
    fn begin_replay(&self, key: &str, suppress: bool);
    /// A live turn started for `key` (a replay is over).
    fn end_replay(&self, key: &str);
    fn replaying(&self, key: &str) -> bool;
    /// Fresh-cache opens drop replayed rows; other transports keep them.
    fn accepts_chat(&self, key: &str) -> bool;
    /// Background content arrived (green dot).
    fn background(&self, key: &str);
    /// A connection reached ready; queued intents may flush.
    fn endpoint_ready(&self, label: Option<&str>);
    /// Persist a session's store with the core's freshness/incomplete rule.
    fn cache_session(&self, key: &str);
    /// Record a listed session's `updatedAt` (the cache-freshness baseline)
    /// without emitting a touch event.
    fn session_stamp(&self, key: &str, updated_at: &str);
    /// A plain tool call landed in `run_id` — the end-of-turn summarizer's
    /// worklist for `key`, oldest first.
    fn tool_run(&self, key: &str, run_id: &str);
    /// A call opened a NEW step, so every earlier step of this turn is now
    /// closed and can be labelled while the turn is still running. Without this
    /// a chip shows the provisional label (the thinking's first line) until the
    /// whole turn ends, which is most of what a reader sees.
    fn step_closed(&self, key: &str);
    /// Re-read `key`'s stamp from its server. `persist` writes the store with
    /// it — only after a FULL replay (the content is current) or our own turn.
    fn stamp_probe(&self, key: &str, persist: bool);
    /// True while `key` is the hidden summarizer session (its chunks are core
    /// bookkeeping, never chat content).
    fn is_summarizer(&self, key: &str) -> bool;
    /// Consume a summarizer agent chunk; true when `key` is a summarizer.
    fn summarizer_chunk(&self, key: &str, text: &str) -> bool;
    /// Take the summarizer's reply text (the id stays registered until the
    /// truncate completes).
    fn take_summarizer_reply(&self, key: &str) -> String;
}

/// Prefix a peer's session ids on top of the core's host.
///
/// The core owns its peers (`Core` → `RoamPeer`), so this holds a WEAK host
/// reference: a strong one would make Core → peer → host → Core a cycle and
/// leak the core.
pub(crate) struct PrefixHost {
    label: String,
    inner: std::sync::Weak<dyn DispatchHost>,
    /// The stores outlive the core's Arc in a background peer task; keeping
    /// them directly avoids a panic after the core drops.
    hub: Arc<SessionHub>,
}

impl PrefixHost {
    pub(crate) fn new(label: String, inner: Arc<dyn DispatchHost>, hub: Arc<SessionHub>) -> Self {
        Self {
            label,
            inner: Arc::downgrade(&inner),
            hub,
        }
    }

    fn host(&self) -> Option<Arc<dyn DispatchHost>> {
        self.inner.upgrade()
    }
}

impl DispatchHost for PrefixHost {
    fn display_id(&self, wire: &str) -> String {
        display_id(Some(&self.label), wire)
    }
    fn store(&self, key: &str) -> Arc<crate::transcript::TranscriptStore> {
        self.hub.store(key)
    }
    fn touched(&self, key: String, title: String, updated_at: String) {
        if let Some(host) = self.host() {
            host.touched(key, title, updated_at);
        }
    }
    fn active_run(&self, key: String, run_id: String) {
        if let Some(host) = self.host() {
            host.active_run(key, run_id);
        }
    }
    fn turn_lost(&self, key: &str) {
        if let Some(host) = self.host() {
            host.turn_lost(key);
        }
    }
    fn config(&self, key: &str, options: Vec<ConfigOption>) {
        if let Some(host) = self.host() {
            host.config(key, options);
        }
    }
    fn usage(&self, key: &str, used: i64, size: i64, cost: f64, currency: String) {
        if let Some(host) = self.host() {
            host.usage(key, used, size, cost, currency);
        }
    }
    fn commands(&self, key: &str, names: Vec<String>) {
        if let Some(host) = self.host() {
            host.commands(key, names);
        }
    }
    fn permission(&self, key: &str, request: PermissionRequest) {
        if let Some(host) = self.host() {
            host.permission(key, request);
        }
    }
    fn begin_replay(&self, key: &str, suppress: bool) {
        if let Some(host) = self.host() {
            host.begin_replay(key, suppress);
        }
    }
    fn end_replay(&self, key: &str) {
        if let Some(host) = self.host() {
            host.end_replay(key);
        }
    }
    fn replaying(&self, key: &str) -> bool {
        self.host().is_some_and(|host| host.replaying(key))
    }
    fn accepts_chat(&self, key: &str) -> bool {
        // The suppression window is the CORE's (one decision for both wires):
        // hardcoding `true` here kept a fresh-cache open from discarding its
        // replayed tail, so every such open appended the last turn again.
        match self.host() {
            Some(host) => host.accepts_chat(key),
            None => true,
        }
    }
    fn background(&self, key: &str) {
        if let Some(host) = self.host() {
            host.background(key);
        }
    }
    fn endpoint_ready(&self, _label: Option<&str>) {
        if let Some(host) = self.host() {
            host.endpoint_ready(Some(&self.label));
        }
    }
    fn cache_session(&self, key: &str) {
        if let Some(host) = self.host() {
            host.cache_session(key);
        }
    }
    fn session_stamp(&self, key: &str, updated_at: &str) {
        if let Some(host) = self.host() {
            host.session_stamp(key, updated_at);
        }
    }
    fn tool_run(&self, key: &str, run_id: &str) {
        if let Some(host) = self.host() {
            host.tool_run(key, run_id);
        }
    }
    fn step_closed(&self, key: &str) {
        if let Some(host) = self.host() {
            host.step_closed(key);
        }
    }
    fn stamp_probe(&self, key: &str, persist: bool) {
        if let Some(host) = self.host() {
            host.stamp_probe(key, persist);
        }
    }
    fn is_summarizer(&self, key: &str) -> bool {
        self.host().is_some_and(|host| host.is_summarizer(key))
    }
    fn summarizer_chunk(&self, key: &str, text: &str) -> bool {
        self.host().is_some_and(|host| host.summarizer_chunk(key, text))
    }
    fn take_summarizer_reply(&self, key: &str) -> String {
        self.host()
            .map(|host| host.take_summarizer_reply(key))
            .unwrap_or_default()
    }
}

/// What a `session_info_update` declares about its session's run, in goose's
/// three states:
///
/// * `Some(id)` — a run is live; its id is the steer key.
/// * `Some("")` — goose EXPLICITLY says no run is live (`activeRunId: null`).
///   This is the client's earliest authoritative end-of-turn, emitted before
///   goose answers `session/prompt`.
/// * `None` — the update says nothing about runs. The generated session-name
///   update carries only `title`/`messageCount` with no `goose` key and can
///   land mid-turn, so it must never be read as an end.
pub(crate) fn declared_run_state(
    info: &agent_client_protocol::schema::v1::SessionInfoUpdate,
) -> Option<String> {
    info.meta
        .as_ref()
        .and_then(|meta| meta.get("goose"))
        .and_then(|goose| goose.get("activeRunId"))
        .map(|value| value.as_str().unwrap_or_default().to_string())
}

/// The single `session/update` dispatch. Every transport calls this; there is
/// no second copy of the accumulation rules.
pub(crate) fn dispatch(
    host: &dyn DispatchHost,
    notif: agent_client_protocol::schema::v1::SessionNotification,
) {
    use agent_client_protocol::schema::v1::SessionUpdate;

    let wire = notif.session_id.to_string();
    let key = host.display_id(&wire);
    // The hidden summarizer session is the core's own bookkeeping: collect its
    // text and keep every one of its updates out of the chat stores.
    if let agent_client_protocol::schema::v1::SessionUpdate::AgentMessageChunk(chunk) =
        &notif.update
    {
        if host.summarizer_chunk(&key, &chunk_text(chunk)) {
            return;
        }
    } else if host.is_summarizer(&key) {
        return;
    }
    let store = host.store(&key);
    let mut content = false;
    match notif.update {
        SessionUpdate::UserMessageChunk(chunk) => {
            if host.accepts_chat(&key) {
                if let Some(uri) = content_image_uri(&chunk.content) {
                    store.append_image(
                        "user",
                        &uri,
                        chunk
                            .message_id
                            .as_ref()
                            .map(|id| id.to_string())
                            .as_deref(),
                    );
                } else {
                    let text = chunk_text(&chunk);
                    if !text.is_empty() {
                        let mid = chunk.message_id.map(|id| id.to_string());
                        store.append_chunk("user", &text, mid.as_deref(), false);
                    }
                }
                content = true;
            }
        }
        SessionUpdate::AgentMessageChunk(chunk) => {
            if host.accepts_chat(&key) {
                if let Some(uri) = content_image_uri(&chunk.content) {
                    store.append_image(
                        "agent",
                        &uri,
                        chunk
                            .message_id
                            .as_ref()
                            .map(|id| id.to_string())
                            .as_deref(),
                    );
                } else {
                    let text = chunk_text(&chunk);
                    if !text.is_empty() {
                        let mid = chunk.message_id.map(|id| id.to_string());
                        store.append_chunk("agent", &text, mid.as_deref(), false);
                    }
                }
                content = true;
            }
        }
        SessionUpdate::AgentThoughtChunk(chunk) => {
            if host.accepts_chat(&key) {
                let text = chunk_text(&chunk);
                if !text.is_empty() {
                    if host.replaying(&key) {
                        // A replayed thought is framing for the tool run it
                        // precedes, never a row of its own.
                        store.replay_thought(&text);
                    } else {
                        store.append_chunk("thought", &text, None, true);
                        content = true;
                    }
                }
            }
        }
        SessionUpdate::ToolCall(tool) => {
            if host.accepts_chat(&key) {
                let id = tool.tool_call_id.to_string();
                let (kind, detail) = tool_call_kind(&tool);
                store.tool_call(&tool.title, &detail, &id, kind);
                // The end-of-turn summarizer labels the turn's plain runs.
                if let Some(run_id) = store.run_id_for(&id) {
                    // The call anchors its own run ⇒ it OPENED a step, so the one
                    // above it is finished and can be labelled now rather than at
                    // the end of the turn.
                    let opened_step = run_id == id;
                    host.tool_run(&key, &run_id);
                    if opened_step {
                        host.step_closed(&key);
                    }
                }
                content = true;
            }
        }
        SessionUpdate::ToolCallUpdate(update) => {
            if host.accepts_chat(&key) {
                apply_tool_update(&store, &update);
                content = true;
            }
        }
        SessionUpdate::UsageUpdate(usage) => {
            let (amount, currency) = usage
                .cost
                .as_ref()
                .map(|cost| (cost.amount, cost.currency.clone()))
                .unwrap_or((0.0, String::new()));
            host.usage(&key, usage.used as i64, usage.size as i64, amount, currency);
        }
        SessionUpdate::SessionInfoUpdate(info) => {
            // `activeRunId` is three-state, and the difference between the last
            // two states is load-bearing:
            //
            //   "run_x"  — a run started (steer becomes available).
            //   null     — goose SAYS the run is over. This is the earliest
            //              authoritative end-of-turn the client ever gets: goose
            //              emits it before it answers `session/prompt`, so a lost
            //              or late response no longer strands the turn.
            //   absent   — says nothing about runs at all. The generated
            //              session-name update carries only `title`/`messageCount`
            //              with no `goose` key, and it can land mid-turn; reading
            //              it as "no run" used to clear the steer affordance in
            //              the middle of a live run.
            if let Some(run_id) = declared_run_state(&info) {
                host.active_run(key.clone(), run_id.clone());
                if !run_id.is_empty() {
                    host.end_replay(&key);
                }
            }
            let title = info
                .title
                .value()
                .map(|t| t.to_string())
                .unwrap_or_default();
            let updated_at = info
                .updated_at
                .value()
                .map(|t| t.to_string())
                .unwrap_or_default();
            if !title.is_empty() || !updated_at.is_empty() {
                host.touched(key.clone(), title, updated_at);
            }
        }
        SessionUpdate::ConfigOptionUpdate(config) => {
            let options = config
                .config_options
                .iter()
                .map(|option| ConfigOption {
                    id: option.id.to_string(),
                    value: config_option_value(option),
                    name: option.name.clone(),
                    choices: Vec::new(),
                })
                .collect();
            host.config(&key, options);
        }
        SessionUpdate::AvailableCommandsUpdate(commands) => {
            let names = commands
                .available_commands
                .iter()
                .map(|command| command.name.clone())
                .collect();
            host.commands(&key, names);
        }
        _ => {}
    }
    if content {
        host.background(&key);
    }
}

/// Apply a tool lifecycle update: app promotion, live output, final content.
pub(crate) fn apply_tool_update(
    store: &crate::transcript::TranscriptStore,
    update: &agent_client_protocol::schema::v1::ToolCallUpdate,
) {
    let id = update.tool_call_id.to_string();
    let status = update
        .fields
        .status
        .as_ref()
        .map(tool_call_status_str)
        .unwrap_or_default();
    if let Some(step) = tool_subagent_step(update) {
        store.tool_step(&id, step);
    }
    if let Some((uri, ext)) = tool_call_app(update) {
        if status != "in_progress" {
            store.tool_app(&id, &uri, &ext);
        }
    }
    if let Some(chunk) = tool_live_output(update) {
        if !chunk.is_empty() {
            store.tool_update(&id, status, &chunk, true);
        }
        return;
    }
    let output = tool_final_output(update);
    store.tool_update(&id, status, &output, false);
}

// ---------------------------------------------------------------------------
// One store per session, one display gate
// ---------------------------------------------------------------------------

/// Every session the core knows about — the main connection's and every roam
/// peer's — gets exactly one [`TranscriptStore`]. The `displayed` key is what
/// the single UI transcript window shows; a background session's store still
/// accumulates (and is saved to the cache) but emits no painting.
///
/// This replaces the per-transport split: the old `PeerStore` + staging map and
/// the spine's single store + listener forwarder were two implementations of
/// this one idea.
pub(crate) struct SessionHub {
    listener: Arc<dyn crate::CoreListener>,
    stores: Mutex<HashMap<String, Arc<crate::transcript::TranscriptStore>>>,
    displayed: Arc<RwLock<Option<String>>>,
    /// Sessions whose background store gained content since the last time they
    /// were displayed (the drawer's green dot).
    has_new: Mutex<HashSet<String>>,
}

impl SessionHub {
    pub(crate) fn new(listener: Arc<dyn crate::CoreListener>) -> Arc<Self> {
        Arc::new(Self {
            listener,
            stores: Mutex::new(HashMap::new()),
            displayed: Arc::new(RwLock::new(None)),
            has_new: Mutex::new(HashSet::new()),
        })
    }

    /// The store for a session, created on first use. Its emissions are gated:
    /// nothing paints unless this key is the displayed one.
    pub(crate) fn store(&self, key: &str) -> Arc<crate::transcript::TranscriptStore> {
        let mut stores = self.stores.lock();
        if let Some(store) = stores.get(key) {
            return store.clone();
        }
        let store = Arc::new(crate::transcript::TranscriptStore::new(Box::new(
            SessionGate {
                key: key.to_string(),
                displayed: self.displayed.clone(),
                inner: self.listener.clone(),
            },
        )));
        store.set_session(key);
        stores.insert(key.to_string(), store.clone());
        store
    }

    /// Whether a store exists for `key` — the chat has been opened (or painted)
    /// in this process, so a prompt naming it is not a stale UI id.
    pub(crate) fn has_store(&self, key: &str) -> bool {
        self.stores.lock().contains_key(key)
    }

    /// Make `key` the session the UI shows. Does not paint by itself.
    pub(crate) fn display(&self, key: &str) -> Arc<crate::transcript::TranscriptStore> {
        let store = self.store(key);
        *self.displayed.write() = Some(key.to_string());
        self.has_new.lock().remove(key);
        store
    }

    /// Show no session (a fresh chat whose id is not known yet).
    pub(crate) fn clear_display(&self) {
        *self.displayed.write() = None;
    }

    pub(crate) fn is_displayed(&self, key: &str) -> bool {
        self.displayed.read().as_deref() == Some(key)
    }

    pub(crate) fn displayed_key(&self) -> Option<String> {
        self.displayed.read().clone()
    }

    pub(crate) fn displayed_store(&self) -> Option<Arc<crate::transcript::TranscriptStore>> {
        self.displayed_key().map(|key| self.store(&key))
    }

    /// A background session produced content: set its green dot once.
    pub(crate) fn mark_new(&self, key: &str) {
        if self.is_displayed(key) {
            return;
        }
        if self.has_new.lock().insert(key.to_string()) {
            self.listener.on_session_touched(
                key.to_string(),
                String::new(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis().to_string())
                    .unwrap_or_default(),
            );
        }
    }

    pub(crate) fn has_new(&self, key: &str) -> bool {
        self.has_new.lock().contains(key)
    }

    /// Drop a session's store (deleted session, closed peer).
    pub(crate) fn forget(&self, key: &str) {
        self.stores.lock().remove(key);
        self.has_new.lock().remove(key);
        if self.displayed_key().as_deref() == Some(key) {
            self.clear_display();
        }
    }
}

/// The per-store listener: paints only while its key is displayed; everything
/// that is turn CONTROL (run end, open failure) and not painting passes.
struct SessionGate {
    key: String,
    displayed: Arc<RwLock<Option<String>>>,
    inner: Arc<dyn crate::CoreListener>,
}

impl SessionGate {
    fn shown(&self) -> bool {
        self.displayed.read().as_deref() == Some(self.key.as_str())
    }
}

impl crate::CoreListener for SessionGate {
    fn on_status(&self, status: crate::ConnectionStatus) {
        self.inner.on_status(status);
    }
    fn on_sessions(&self, sessions: Vec<crate::SessionSummary>) {
        self.inner.on_sessions(sessions);
    }
    fn on_item(&self, op: crate::TranscriptOp) {
        if self.shown() {
            self.inner.on_item(op);
        }
    }
    fn on_usage(&self, used: i64, size: i64, cost: f64, currency: String) {
        if self.shown() {
            self.inner.on_usage(used, size, cost, currency);
        }
    }
    fn on_run_ended(&self, session_id: String, stop_reason: String) {
        // Turn control, not painting: a background turn must still release
        // its chat's in-flight state — which is why the key travels with it.
        self.inner.on_run_ended(session_id, stop_reason);
    }
    fn on_speak(&self, text: String) {
        if self.shown() {
            self.inner.on_speak(text);
        }
    }
    fn on_config(&self, options: Vec<crate::ConfigOption>) {
        self.inner.on_config(options);
    }
    fn on_permission_request(&self, request: crate::PermissionRequest) {
        self.inner.on_permission_request(request);
    }
    fn on_session_touched(&self, session_id: String, title: String, updated_at: String) {
        self.inner.on_session_touched(session_id, title, updated_at);
    }
    fn on_projects(&self, projects: Vec<crate::ProjectSummary>) {
        self.inner.on_projects(projects);
    }
    fn on_roam_peer_status(&self, label: String, status: String) {
        self.inner.on_roam_peer_status(label, status);
    }
    fn on_roam_sessions(&self, label: String, sessions: Vec<crate::SessionSummary>) {
        self.inner.on_roam_sessions(label, sessions);
    }
    fn on_peer_new_session(&self, label: String, session_id: String) {
        self.inner.on_peer_new_session(label, session_id);
    }
    fn on_active_run(&self, session_id: String, run_id: String) {
        self.inner.on_active_run(session_id, run_id);
    }
    fn on_commands(&self, commands: Vec<String>) {
        self.inner.on_commands(commands);
    }
    fn on_session_open_failed(&self, session_id: String, message: String) {
        self.inner.on_session_open_failed(session_id, message);
    }
}
