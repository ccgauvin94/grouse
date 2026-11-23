// SPDX-License-Identifier: AGPL-3.0-or-later

//! ACP parameter builders, timeout policy and shared RPC helpers.
//!
//! Every transport sends the same `initialize`, `session/new|load|prompt`
//! and list parameters; a field added here reaches both wires at once.

use super::*;

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

/// How many `session/list` pages a single refresh will walk.
///
/// 50 rows a page, so this is a 2,000-session ceiling — far past anything this box
/// has (167 real chats), and a bound the loop needs so a server that hands back a
/// cursor it never exhausts cannot spin forever.
pub(crate) const SESSION_LIST_MAX_PAGES: usize = 40;

/// `session/list`: user + ACP sessions only (the desktop's `listSessions`).
///
/// `cursor` asks for the page after a previous reply's `nextCursor`. Callers follow
/// it to the end (`Core::list_sessions`): one page is 50 rows ordered by last
/// message, and this box's Assistant thread chats and scheduled runs fill that page
/// on their own, so a project's own conversations fall off the bottom and a client
/// grouping by project draws one row where there used to be five.
pub(crate) fn session_list_params(cursor: Option<&str>, tags: &[(String, String)]) -> Value {
    let mut meta = Map::new();
    meta.insert(
        "types".into(),
        Value::Array(vec![
            Value::String("user".into()),
            Value::String("acp".into()),
        ]),
    );
    if !tags.is_empty() {
        let mut wanted = Map::new();
        for (key, value) in tags {
            wanted.insert(key.clone(), Value::String(value.clone()));
        }
        meta.insert("tags".into(), Value::Object(wanted));
    }
    let mut params = json!({ "_meta": Value::Object(meta) });
    if let Some(cursor) = cursor.filter(|c| !c.is_empty()) {
        params["cursor"] = Value::String(cursor.to_string());
    }
    params
}

/// The same filter as a typed `ListSessionsRequest` meta map (the SDK's typed
/// request path). `tags` narrows the list to sessions carrying every pair — the
/// only read a tag has (`session/list` never returns one on a row).
pub(crate) fn session_list_meta(tags: &[(String, String)]) -> Map<String, Value> {
    let mut meta = Map::new();
    meta.insert(
        "types".into(),
        Value::Array(vec![
            Value::String("user".into()),
            Value::String("acp".into()),
        ]),
    );
    if !tags.is_empty() {
        let mut wanted = Map::new();
        for (key, value) in tags {
            wanted.insert(key.clone(), Value::String(value.clone()));
        }
        meta.insert("tags".into(), Value::Object(wanted));
    }
    meta
}

/// The tag key that carries a session's project, for the migration off
/// `_meta.projectId`.
///
/// goose is moving project association out of the server and has shipped the first
/// half of client-owned session metadata (`session/new` `_meta.tags`, a flat string
/// map, filtered server-side; aaif-goose/goose#13173, tracked here as grouse#5 /
/// upstream #13014). So a project is written BOTH ways while the server still
/// honours the column, and read column-first with the tag as the fallback — the
/// order matters because `projectId` is what makes goose inject the project's
/// instructions, which a tag cannot do.
///
/// Namespaced deliberately: `grouse.` is ours, the same ownership rule
/// #13014 asks goose to enforce, so a bare `project` from another client (or from
/// goose itself, later) cannot collide with it.
pub const PROJECT_TAG_KEY: &str = "grouse.project";

/// The tag key that carries a session's MODE — `dev`, `chat`, or absent (chat).
///
/// A mode is a client-owned profile (a tag), not a goose feature: goose stores it
/// and filters on it, nothing else. **It is fixed at creation**, because the server
/// we speak to exposes `_meta.tags` on `session/new` and as a `session/list` filter
/// but carries no tags back on a row and has no mutation method — so there is
/// nothing to update a mode WITH. Flipping a mode in place waits on upstream.
pub const MODE_TAG_KEY: &str = "grouse.mode";

/// The one non-default mode: a chat that opens the dev-only surface (the advisor today,
/// a changes panel next). Named because several call sites compare
/// against it — the advisor's turn-end trigger among them — and a string literal per
/// site is how a typo becomes "the feature silently never runs".
pub const DEV_MODE: &str = "dev";

/// The modes this client knows: `chat` (the default, and what an absent tag means)
/// or `dev`.
///
/// Closed on purpose: the only way to learn a session's mode is to ask which
/// sessions carry each value (one filtered `session/list` per value), so an open
/// vocabulary would cost a walk per word.
pub const MODE_VALUES: &[&str] = &[DEFAULT_MODE, DEV_MODE];

/// An untagged session is a normal chat. The read path only walks the modes that
/// are NOT the default, because "absent" already says chat.
pub const DEFAULT_MODE: &str = "chat";


/// `session/new`. `_meta.client` present ⇒ goose's `SessionType::User`, so the
/// desktop/CLI can see these chats. `recipeId` starts a recipe session;
/// `projectId` is persisted at creation (the stock-goose project bridge) and
/// mirrored into `_meta.tags` (`PROJECT_TAG_KEY`) for the day it is not.
pub(crate) fn session_new_params(
    client_id: &str,
    cwd: &str,
    recipe_id: Option<&str>,
    project_id: Option<&str>,
    mode: Option<&str>,
) -> Value {
    let mut meta = Map::new();
    meta.insert("client".into(), Value::String(client_id.to_string()));
    if let Some(recipe) = recipe_id.filter(|r| !r.is_empty()) {
        meta.insert("recipeId".into(), Value::String(recipe.to_string()));
    }
    let project = project_id.map(str::trim).filter(|p| !p.is_empty());
    // An unknown mode is dropped rather than stored: a typo would otherwise become
    // a tag nothing filters on, invisible to every UI.
    let mode = mode
        .map(str::trim)
        .filter(|m| MODE_VALUES.contains(m));
    if let Some(project) = project {
        meta.insert("projectId".into(), Value::String(project.to_string()));
    }
    // The forward-compatible half. A server that predates client tags ignores
    // unknown `_meta` keys (verified against goose 1.56.0), so this costs nothing
    // today and means a chat filed or started from now on is already tagged the day
    // tags become the only mechanism. It is also why no backfill exists for older
    // chats: `session_tags` is written only by `session/new` until upstream adds a
    // mutation.
    if project.is_some() || mode.is_some() {
        let mut tags = Map::new();
        if let Some(project) = project {
            tags.insert(PROJECT_TAG_KEY.to_string(), Value::String(project.to_string()));
        }
        if let Some(mode) = mode {
            tags.insert(MODE_TAG_KEY.to_string(), Value::String(mode.to_string()));
        }
        meta.insert("tags".into(), Value::Object(tags));
    }
    json!({
        "cwd": cwd,
        "mcpServers": [],
        "_meta": Value::Object(meta),
    })
}

/// A session's project, from a `session/list` entry's `_meta`.
///
/// Column first, tag second, and blank counts as absent in both: a client that sends
/// `"  "` should not create a project named two spaces. Unknown extra tags are
/// ignored — the tag bag is shared with whatever else a client (or a future goose)
/// puts in it.
pub(crate) fn project_from_meta(meta: Option<&Map<String, Value>>) -> Option<String> {
    let meta = meta?;
    let column = meta
        .get("projectId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|project| !project.is_empty());
    let tag = meta
        .get("tags")
        .and_then(Value::as_object)
        .and_then(|tags| tags.get(PROJECT_TAG_KEY))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|project| !project.is_empty());
    column.or(tag).map(str::to_string)
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
            PromptBlock::Link {
                uri,
                name,
                mime_type,
            } => json!({
                "type": "resource_link",
                "uri": uri,
                "name": name,
                "mimeType": mime_type,
            }),
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
        // The context report tokenizes every message in the conversation with
        // goose's local tokenizer — seconds on a 600k-token chat, and a roam peer
        // has to do that on the other machine and send it back. The 30s default
        // cut it off exactly where it was most useful.
        "_goose/unstable/context/report" => Some(Duration::from_secs(120)),
        _ => Some(Duration::from_secs(30)),
    }
}

/// How long a `session/load` may go unanswered before the open is reported as a
/// FAILURE.
///
/// Deliberately not part of [`rpc_timeout`] (which answers `None` here): a full
/// replay of a long session can honestly take minutes, so a short cap would fail
/// loads that would have worked. But "no bound at all" is what turned one
/// unanswered load into a chat that can never open again: the await holds the
/// store's load slot (`TranscriptStore::begin_load`), so every retry of the SAME
/// chat queues behind the first one, the client's spinner never resolves, and no
/// listener event is ever emitted to say why — the roam "this chat will not load"
/// report. Ten minutes is far past any replay this client asks for and still
/// converts "forever" into a message the user can act on.
pub(crate) static LOAD_TIMEOUT_SECS: AtomicU64 = AtomicU64::new(600);

/// The same for a load that replays the WHOLE session (no tail): a resync's
/// in-place rebuild, or the merge-gap fallback. Those are not user-initiated and
/// they FREEZE the chat while they wait — the store's emissions are held until
/// the replay ends, so every row the live turn produces is swallowed and the
/// screen stops updating (the "nothing shows until I refresh" report). Two
/// minutes is far longer than a real full replay needs and short enough that a
/// chat recovers instead of sitting dead behind a load that will never answer.
pub(crate) static REBUILD_TIMEOUT_SECS: AtomicU64 = AtomicU64::new(120);

fn load_timeout(full: bool) -> Duration {
    let secs = if full {
        REBUILD_TIMEOUT_SECS.load(Ordering::SeqCst)
    } else {
        LOAD_TIMEOUT_SECS.load(Ordering::SeqCst)
    };
    Duration::from_secs(secs)
}

/// `session/load` with the bounded wait above. Both wires go through this, so a
/// load that is never answered fails the same way everywhere and the caller can
/// report an open failure the client can retry.
pub(crate) async fn load_request(
    endpoint: &dyn ChatEndpoint,
    key: &str,
    cwd: &str,
    tail: Option<usize>,
) -> Result<Value, AcpError> {
    let limit = load_timeout(tail.is_none());
    match tokio::time::timeout(
        limit,
        endpoint.request("session/load", session_load_params(key, cwd, tail)),
    )
    .await
    {
        Ok(reply) => reply,
        Err(_) => Err(AcpError::internal_error().data(format!(
            "session/load gave no answer in {}s (the host is busy with another turn, or the link \
             is half-open) — open the chat again to retry",
            limit.as_secs()
        ))),
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

