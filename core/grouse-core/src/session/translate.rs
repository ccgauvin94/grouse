// SPDX-License-Identifier: AGPL-3.0-or-later

//! Wire payload → core record translation (one implementation, both transports).
//!
//! Tool calls, chunks, session summaries and permission prompts are decoded
//! here so a protocol change cannot land on one wire and be forgotten on the other.

use super::*;

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

/// When goose says this message was created — epoch seconds, `_meta.goose.created`.
///
/// Every message chunk carries it (`crates/goose/src/acp/server/message_meta.rs`), and it
/// is the ONLY clock a transcript row has: `Item::seq` orders rows inside one chat and
/// nothing else. An area timeline has to interleave chat with events that live outside the
/// transcript (a My Day card, an action taken in another app), and this stamp is what makes
/// that possible. 0 when the server sent none, which a renderer must read as "unknown",
/// never as 1970.
pub(crate) fn chunk_created(chunk: &agent_client_protocol::schema::v1::ContentChunk) -> i64 {
    chunk
        .meta
        .as_ref()
        .and_then(|meta| meta.get("goose"))
        .and_then(|goose| goose.get("created"))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
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

/// The shared file a tool call announces, if it announces one.
///
/// `_meta.goose.toolCall.toolName` is the tool's own `<extension>__<tool>` name,
/// sent verbatim by the server, and `rawInput` is the call's arguments — neither
/// is model prose, so both are safe to key a card on.
pub(super) fn share_from_tool_call(
    tc: &agent_client_protocol::schema::v1::ToolCall,
) -> Option<crate::attachments::SharedFile> {
    let tool_name = tc
        .meta
        .as_ref()?
        .get("goose")?
        .pointer("/toolCall/toolName")?
        .as_str()?;
    let raw_input = tc.raw_input.clone().unwrap_or(serde_json::Value::Null);
    crate::attachments::share_from_call(tool_name, &raw_input)
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

/// The goose TOOL NAME behind a tool call (`_meta.goose.toolCall.toolName`), e.g.
/// `developer__shell` or `delegate`.
///
/// The display title is the MODEL's prose ("checking the ssh banner and reverse dns"),
/// so this is the only honest identity a call carries. `Core::activity` needs it to tell
/// a deliberate background spawn from a step of the turn itself.
pub(crate) fn tool_call_name(tool: &agent_client_protocol::schema::v1::ToolCall) -> String {
    tool.meta
        .as_ref()
        .and_then(|meta| meta.get("goose"))
        .and_then(|goose| goose.get("toolCall"))
        .and_then(|call| call.get("toolName"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
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

/// The goose project that holds the assistant's OWN conversations — one chat per
/// thread label, opened from My Day's thread groups.
///
/// It is a real project, which is the point: goose injects a filed session's project
/// instructions into its prompts (`agents/state_machine/ops_project.rs`), so the
/// persona the assistant answers with in a thread chat is the project's content, not
/// something a client has to paste in. It is also why the sessions filed under it are
/// NOT chats in the user's list: they are the assistant's working memory for an area,
/// reached from My Day, and both clients hide them by this one rule. That rule lives
/// here rather than in either UI because "shared behaviour lands in the core, once".
pub const ASSISTANT_PROJECT: &str = "assistant";

/// Whether a summary belongs to the assistant's own project.
pub(crate) fn in_assistant_project(summary: &SessionSummary) -> bool {
    summary
        .project_id
        .as_deref()
        .is_some_and(|p| p.trim().eq_ignore_ascii_case(ASSISTANT_PROJECT))
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
    let project_id = project_from_meta(meta);
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
        // Stamped by whoever knows the mode sets: a row never carries its tags
        // back, so `parse_sessions` cannot see one.
        mode: None,
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
        // Hidden from every chat list, but its cwd is recorded above on purpose: a
        // thread chat is opened BY ID from My Day, and `session/load` rewrites the
        // working dir from whatever cwd we send. Dropping the side-table entry would
        // quietly move the chat's cwd on the first resume.
        if in_assistant_project(&summary) {
            continue;
        }
        sessions.push(summary);
    }
    (sessions, cwds, updated_at)
}

/// One typed `session/list` entry → summary (the peer's typed reply path).
pub(crate) fn summary_from_info(label: Option<&str>, info: &SessionInfo) -> Option<SessionSummary> {
    let wire = info.session_id.to_string();
    let summary = summary_from_fields(
        display_id(label, &wire),
        info.title.as_deref(),
        info.updated_at.as_deref(),
        &info.cwd.to_string_lossy(),
        info.meta.as_ref(),
    )?;
    // Same rule as the WebSocket path: an assistant-project session is not a chat
    // in the list, whoever is serving it.
    if in_assistant_project(&summary) {
        return None;
    }
    Some(summary)
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
        session_id: String::new(), // stamped by the wire host via on_chat()
        owner: String::new(),
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

