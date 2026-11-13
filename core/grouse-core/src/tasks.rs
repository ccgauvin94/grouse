// SPDX-License-Identifier: AGPL-3.0-or-later

//! The My Day tab's task surface: assistant-state items reached over goose's
//! `_goose/unstable/tools/call` — a deterministic tool invocation with NO model
//! turn, so the tab costs milliseconds instead of a 60k-token prefill.
//!
//! Two properties of that path are load-bearing and were measured on the box,
//! not assumed:
//!
//! * **`tools/call` requires the session to be in `auto` mode.** goose refuses
//!   otherwise with "app tool calls require auto mode", and an app call goes
//!   straight to the extension lease, bypassing the permission manager — which
//!   is exactly why upstream insists on auto. So the calls run on a dedicated
//!   HIDDEN session (`_meta.hidden`, i.e. `SessionType::Hidden`, which goose
//!   filters out of every session list) that is never prompted and is set to
//!   auto. Chat sessions keep smart_approve; nothing here touches them.
//! * **Extensions come from the server config**, so that hidden session already
//!   carries assistant-state like any chat does — no per-session extension work.
//!
//! Everything here is pure (params in, JSON out) so it can be tested without a
//! server; the plumbing lives on `Core`.

use serde_json::{json, Value};

/// The extension the task tools live on, as `name_to_key` spells it.
///
/// Measured against the live server: a session advertises
/// `assistantstate__state_items`, i.e. goose's key for the config entry strips
/// the punctuation out of the name — the config key is `assistant-state` and the
/// tool prefix is `assistantstate`. The `extensionName` argument is normalised by
/// the same function, so the display name works there; the TOOL name is compared
/// verbatim, so it needs the key.
pub(crate) const EXTENSION: &str = "Assistant State";

/// The extension KEY, which is what prefixes every tool name.
pub(crate) const EXTENSION_KEY: &str = "assistantstate";

/// The notifier, which owns the relay into a session's transcript. Only
/// `thread_note` is exposed on it globally (see the `notify` block in config.yaml).
pub(crate) const NOTE_EXTENSION: &str = "Notify";
pub(crate) const NOTE_EXTENSION_KEY: &str = "notify";

/// `_goose/unstable/tools/call` params for one tool on `wire`.
///
/// `extensionName` is REQUIRED by the server's `GooseToolCallRequest` and it is
/// what scopes the lookup (`list_tools(session, Some(extension))`); the older
/// two-argument shape in this shim omitted it and could never have worked.
pub(crate) fn tool_params(wire: &str, tool: &str, arguments: Value) -> Value {
    json!({
        "sessionId": wire,
        "extensionName": EXTENSION,
        "name": format!("{EXTENSION_KEY}__{tool}"),
        "arguments": arguments,
    })
}

/// The open list: `open` plus `snoozed`, because a snoozed item is still open —
/// it is only quiet until its `until` passes.
pub(crate) fn open_params(wire: &str, limit: u32) -> Value {
    tool_params(
        wire,
        "state_items",
        json!({ "state": "open,snoozed", "limit": limit }),
    )
}

/// The closed list. `limit` is small on purpose: this is a "what did I finish
/// lately" tail, not an archive (the archive is `state_items` in a chat).
pub(crate) fn done_params(wire: &str, limit: u32) -> Value {
    tool_params(
        wire,
        "state_items",
        json!({ "state": "done,expired", "limit": limit }),
    )
}

/// Handed-over work: `running` and `review` only. `review` is the state that
/// matters to a person — the work stopped and is waiting for a look — and it is
/// what makes this list worth painting above the open items rather than buried
/// among them.
pub(crate) fn task_params(wire: &str, limit: u32) -> Value {
    tool_params(
        wire,
        "task_list",
        json!({ "state": "running,review", "limit": limit }),
    )
}

pub(crate) fn close_params(wire: &str, id: &str, state: &str, resolution: &str) -> Value {
    tool_params(
        wire,
        "state_close",
        json!({ "id": id, "state": state, "resolution": resolution }),
    )
}

pub(crate) fn snooze_params(wire: &str, id: &str, until: &str, reason: &str) -> Value {
    tool_params(
        wire,
        "state_snooze",
        json!({ "id": id, "until": until, "reason": reason }),
    )
}

pub(crate) fn react_params(wire: &str, id: &str, reaction: &str) -> Value {
    tool_params(
        wire,
        "state_react",
        json!({ "item_id": id, "reaction": reaction }),
    )
}

/// The thread -> conversation map: `thread_sessions` (assistant-state, schema v6).
///
/// This is the ONLY way to find a thread's own chat. `session/list` caps at 50,
/// excludes 0-message sessions, and the assistant project is filtered out of every
/// chat list here — so an idle thread conversation is unfindable by name. The map
/// lives on the server rather than in a client because a per-client map forked the
/// conversation: measured 2026-11-01, "Thread: Global entry" existed twice (82 and
/// 99 messages, 21 minutes apart), once from each client.
pub(crate) fn thread_map_params(wire: &str) -> Value {
    tool_params(wire, "thread_sessions", json!({}))
}

/// Record (or forget — an empty session id) the conversation for a label.
pub(crate) fn link_params(wire: &str, label: &str, session_id: &str, session_name: &str) -> Value {
    tool_params(
        wire,
        "thread_link",
        json!({ "thread": label, "session_id": session_id, "session_name": session_name }),
    )
}

/// `{"threads": {label: sessionId}}` -> the map. A bare map is accepted too: the
/// tool's own reply nests it, but a future shape that flattens it must not empty
/// every thread.
pub(crate) fn parse_threads(text: &str) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return out;
    };
    let map = match value.get("threads") {
        Some(Value::Object(m)) => m,
        None => match &value {
            Value::Object(m) => m,
            _ => return out,
        },
        Some(_) => return out,
    };
    for (label, id) in map {
        if let Some(id) = id.as_str() {
            if !label.trim().is_empty() && !id.trim().is_empty() {
                out.insert(label.to_string(), id.to_string());
            }
        }
    }
    out
}

/// The labels the rows carry, deduped in first-seen order.
///
/// Derived from the payload rather than from a separate query: a refresh already has
/// the rows, and a label the server stopped reporting is one we must not create a
/// conversation for.
pub(crate) fn labels(open: Option<&str>, handed: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for text in [open, handed].into_iter().flatten() {
        let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(text) else {
            continue;
        };
        for row in rows {
            let Some(label) = row.get("thread").and_then(Value::as_str) else {
                continue;
            };
            let label = label.trim();
            if !label.is_empty() && !out.iter().any(|seen| seen == label) {
                out.push(label.to_string());
            }
        }
    }
    out
}

/// One line the core offers into a thread's conversation, carrying the key that makes
/// it idempotent (see `note_params`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextNote {
    /// What the note is ABOUT — an item id, a task id, or `thread:<label>` for the
    /// header. It rides in the note's own text, so the relay can find it again.
    pub key: String,
    pub text: String,
}

/// How many of a thread's rows the opening context carries in full. Beyond this the
/// header points at the tool instead: a note is re-sent on every turn of that
/// conversation, so this is bounded by cost, not by what the store holds.
const NOTES_PER_THREAD: usize = 4;

/// How much of a row's `detail` a note carries.
///
/// The detail is where the WHO that the title assumes actually lives — the item that
/// prompted this (2026-11-12) was titled "…right before Schaible's 3:10 seminar you
/// haven't answered about" and its detail said "George Schaible's 15:10 seminar in
/// PBB108". The thread chat was seeded from the title alone, so it could not answer a
/// question about its own item and spent twenty minutes rediscovering it by tool call.
const DETAIL_CHARS: usize = 700;

/// A title is one sentence in practice; this only guards against a pathological one.
const TITLE_CHARS: usize = 300;

fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect::<String>() + "…"
}

/// The context notes for every thread the board shows: a header naming the area, then
/// one note per row carrying its title, its detail and where to read the rest.
///
/// This is the "context, pre-loaded" half of a thread chat, and the whole reason a thread
/// can be discussed at all: the conversation is minted (and topped up) with deterministic
/// assistant rows, no model turn involved. Notes are keyed per ROW rather than per label,
/// so a thread that gains an item gains exactly that item's note the next time it is
/// opened, and one that has not changed gets nothing.
pub(crate) fn thread_notes(
    open: Option<&str>,
    handed: Option<&str>,
) -> std::collections::BTreeMap<String, Vec<ContextNote>> {
    // label -> (total rows, notes so far)
    let mut out: std::collections::BTreeMap<String, (usize, Vec<ContextNote>)> =
        std::collections::BTreeMap::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (text, kind) in [(open, "open"), (handed, "handed over")] {
        let Some(text) = text else { continue };
        let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(text) else {
            continue;
        };
        for row in rows {
            let Some(label) = row.get("thread").and_then(Value::as_str) else {
                continue;
            };
            let label = label.trim().to_string();
            if label.is_empty() {
                continue;
            }
            let id = row
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            let title = row
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if title.is_empty() {
                continue;
            }
            let entry = out.entry(label).or_insert((0, Vec::new()));
            entry.0 += 1;
            if entry.1.len() >= NOTES_PER_THREAD || !seen.insert(id.clone()) {
                continue;
            }
            entry.1.push(ContextNote {
                key: if id.is_empty() { title.clone() } else { id.clone() },
                text: row_note(&row, kind, &id, &title),
            });
        }
    }
    out.into_iter()
        .map(|(label, (count, mut notes))| {
            let header = header_note(&label, count);
            notes.insert(0, header);
            (label, notes)
        })
        .collect()
}

/// The one line that tells a thread chat which area it is and where the rest lives.
fn header_note(label: &str, count: usize) -> ContextNote {
    ContextNote {
        key: format!("thread:{label}"),
        text: format!(
            "Thread {label}: this chat is that area's own conversation. {count} row(s) of \
             it follow, with the detail behind each one. Everything under the label — with \
             evidence, provenance and state — is `assistantstate__state_items(thread=\"{label}\")`, \
             and `state_item(<id>)` returns one in full."
        ),
    }
}

/// One row as context: the title (the sentence My Day shows), the detail (what the title
/// assumes the reader knows), and a footer naming the row so it stays addressable and so
/// the note can be recognised as already-written.
fn row_note(row: &Value, kind: &str, id: &str, title: &str) -> String {
    let field = |name: &str| -> String {
        row.get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    let mut text = clip(title, TITLE_CHARS);
    // A task's own words are request (what was asked) and result (what came back); an
    // item's are detail.
    let mut body = if kind == "handed over" {
        let request = field("request");
        let result = field("result");
        [request, result]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        field("detail")
    };
    if !body.is_empty() && body != text {
        body = clip(&body, DETAIL_CHARS);
        text.push_str("\n\n");
        text.push_str(&body);
    }
    let mut footer = Vec::new();
    let state = field("state");
    let row_kind = if kind == "handed over" {
        "handed over".to_string()
    } else {
        field("kind")
    };
    footer.push(if id.is_empty() {
        format!("{kind} row")
    } else {
        id.to_string()
    });
    if !row_kind.is_empty() {
        footer.push(row_kind);
    }
    if !state.is_empty() {
        footer.push(state);
    }
    let due = field("due_at");
    if !due.is_empty() {
        footer.push(format!("due {due}"));
    }
    text.push_str(&format!("\n\n({})", footer.join(" · ")));
    text
}

/// `notify__thread_note` — append one line into a thread's own conversation.
///
/// The CORE uses it for exactly one thing: a thread chat's context notes. It must never
/// write into `sessions.db` itself, and this is the one path that already knows how.
///
/// `key` makes the RELAY the guard, not the core: it writes only when no row in that
/// conversation already mentions the key. That is what lets the core re-offer every note
/// on every open with no state of its own —
///
/// * a thread whose mint-time relay failed, or one minted by an older client, gets its
///   context the first time somebody opens it;
/// * a thread that has gained an item since it was last opened gets exactly that item's
///   note and nothing else (the keys of the rows already written are already there);
/// * and a thread whose rows have not changed is left alone.
///
/// `once` (write only into an EMPTY conversation) was the previous guard and is gone:
/// it could never repair a thread whose one truncated line had already landed, which is
/// exactly the state 2026-11-12 was stuck in.
pub(crate) fn note_params(wire: &str, label: &str, text: &str, key: &str) -> Value {
    json!({
        "sessionId": wire,
        "extensionName": NOTE_EXTENSION,
        "name": format!("{NOTE_EXTENSION_KEY}__thread_note"),
        "arguments": { "thread": label, "text": text, "key": key },
    })
}

/// `nuclera-pricing` -> `Thread: Nuclera pricing`, the name every thread chat carries.
/// Matches what both clients have always named them, so sessions created before this
/// map existed keep their identity.
pub(crate) fn thread_title(label: &str) -> String {
    let words = label.trim().replace(['-', '_'], " ");
    let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
    if words.is_empty() {
        return String::new();
    }
    let mut chars = words.chars();
    let first = chars.next().unwrap_or_default().to_uppercase().collect::<String>();
    format!("Thread: {first}{}", chars.as_str())
}

/// `session/new` params for the hidden task host, and the mode to put it in.
///
/// `mcpServers: []` is not "no extensions" — the server resolves its own config
/// defaults; the list is only client-supplied servers, and we supply none.
pub(crate) fn new_session_params(cwd: &str) -> Value {
    json!({
        "cwd": if cwd.is_empty() { "/" } else { cwd },
        "mcpServers": [],
        "_meta": { "hidden": true },
    })
}

pub(crate) fn set_mode_params(wire: &str, mode: &str) -> Value {
    json!({ "sessionId": wire, "modeId": mode })
}

/// The concatenated text blocks of a `GooseToolCallResponse`.
///
/// assistant-state answers every tool as one `text` block holding JSON, so this
/// is the whole payload. Non-text blocks are ignored rather than stringified:
/// nothing in this path emits them, and a guess here would be a parsing bug
/// waiting to happen.
pub(crate) fn content_text(reply: &Value) -> String {
    let Some(blocks) = reply.get("content").and_then(Value::as_array) else {
        return String::new();
    };
    let mut out = String::new();
    for block in blocks {
        if let Some(t) = block.get("text").and_then(Value::as_str) {
            out.push_str(t);
        }
    }
    out
}

/// Whether the tool result reports an error. The server spells it `isError`
/// (`GooseToolCallResponse` is camelCase); the snake spelling is accepted too so
/// a shape change degrades into "treated as an error", never into silent empty
/// content.
pub(crate) fn is_error(reply: &Value) -> bool {
    reply
        .get("isError")
        .or_else(|| reply.get("is_error"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// One section of the tab: a parsed row array (projected by `project`), or the
/// reason there is none.
fn section(result: Result<String, String>, project: fn(&mut Value)) -> (Value, Option<String>) {
    match result {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(mut v @ Value::Array(_)) => {
                project(&mut v);
                (v, None)
            }
            Ok(other) => (
                Value::Array(Vec::new()),
                Some(format!("unexpected payload: {other}")),
            ),
            Err(e) => (Value::Array(Vec::new()), Some(format!("bad JSON: {e}"))),
        },
        Err(e) => (Value::Array(Vec::new()), Some(e)),
    }
}

/// The fields a row needs, and only those.
///
/// `state_items` answers with the whole row — evidence arrays carrying paragraph
/// long quotes from mail and calendar (a 60+20 item read measured 82 KB, most of
/// it evidence). Every byte crosses the FFI and lands in a Compose state list, so
/// the projection is done HERE, once, and `state_item` remains the way to ask for
/// one item's provenance when a row is opened.
///
/// `snooze_until` is load-bearing: the open list is `open` AND `snoozed` (a
/// snoozed item is still open, just quiet), so without this field a snoozed row
/// arrives looking exactly like an awake one — which is why the first cut of the
/// tab made Snooze look like it did nothing at all.
///
/// The three stamps are kept so My Day can read as a timeline rather than a pile:
/// `first_seen` is when the item was added (the row's own "added" line),
/// `last_evidence` is the freshest sighting of it (a re-raised loop is old news
/// with new news attached), and `closed_at` is when it left the board. All three
/// were always on the wire — `state_items` answers `SELECT *` — they were simply
/// dropped here, which is why nothing on screen had a date.
const KEPT_FIELDS: [&str; 14] = [
    "id",
    "kind",
    "state",
    "title",
    "detail",
    "owner",
    "due_at",
    "salience",
    "confidence",
    "resolution",
    "snooze_until",
    "first_seen",
    "last_evidence",
    "closed_at",
];

fn slim(items: &mut Value) {
    let Value::Array(rows) = items else { return };
    for row in rows.iter_mut() {
        let Value::Object(map) = row else { continue };
        map.retain(|k, _| KEPT_FIELDS.contains(&k.as_str()) || k == "thread");
        normalise_thread(map);
    }
}

/// `thread` is a LABEL on the server since schema v3 (2026-10-29): one nullable
/// string naming the area an item belongs to — one thread may hold many items
/// over its life — and no thread table behind it. Servers before v3 answered
/// `{slug, title}`; both shapes collapse to one string here so the app has a
/// single field to render, and a blank label is DROPPED rather than forwarded as
/// `""`, because an empty segment is visible noise in the row's meta line.
///
/// This is a real break, not a hypothetical: the app read `thread` as an object
/// and rendered nothing once the server started answering a string, so the label
/// silently vanished from every row until this normalisation landed.
fn normalise_thread(map: &mut serde_json::Map<String, Value>) {
    let Some(raw) = map.get("thread") else { return };
    let label = match raw {
        Value::String(s) => s.trim().to_string(),
        Value::Object(o) => o
            .get("title")
            .or_else(|| o.get("slug"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string(),
        _ => String::new(),
    };
    if label.is_empty() {
        map.remove("thread");
    } else {
        map.insert("thread".into(), Value::String(label));
    }
}

/// The fields a task row needs. Deliberately a different set from an item's: a
/// task has no salience, no evidence and no confidence — it has a state, the line
/// it came back with, and where the work was written.
const TASK_FIELDS: [&str; 10] = [
    "id",
    "title",
    "state",
    "thread",
    "session_id",
    "request",
    "result",
    "artifacts",
    "created_at",
    "updated_at",
];

/// `request` is context, not content: a row shows the RESULT line, and the asked-for
/// prose can be a paragraph. Trimmed here because every byte crosses the FFI and
/// lands in a Compose state list.
const REQUEST_CAP: usize = 300;

fn slim_task(items: &mut Value) {
    let Value::Array(rows) = items else { return };
    for row in rows.iter_mut() {
        let Value::Object(map) = row else { continue };
        map.retain(|k, _| TASK_FIELDS.contains(&k.as_str()));
        if let Some(Value::String(s)) = map.get_mut("request") {
            if s.chars().count() > REQUEST_CAP {
                let mut cut: String = s.chars().take(REQUEST_CAP).collect();
                cut.push('…');
                *s = cut;
            }
        }
        normalise_thread(map);
    }
}

/// The JSON `on_tasks` carries:
/// `{"open": [...], "done": [...], "tasks": [...], "errors": {...}}`.
///
/// `errors` is keyed by section and omitted when every section succeeded, so the
/// UI's happy path is three plain arrays and a failure is legible instead of an
/// empty list that looks like "nothing to do".
///
/// `tasks` is handed-over work (assistant-state's `task` rows: running, or waiting
/// for review). It is a different object from an item — an item is a fact that
/// needs attention, a task is an act with a result — which is why it has its own
/// key and its own projection rather than being folded into `open`.
/// `envelope` plus the thread map, which is what lets a client open a thread's own
/// conversation instead of minting a second one. Additive: a client that predates it
/// ignores the key, and a server whose map is empty simplifies to the old document.
pub(crate) fn envelope_threads(
    open: Result<String, String>,
    done: Result<String, String>,
    tasks: Result<String, String>,
    threads: &std::collections::BTreeMap<String, String>,
    thread_error: Option<String>,
) -> String {
    let mut out: Value = serde_json::from_str(&envelope(open, done, tasks)).unwrap_or_else(|_| json!({}));
    out["threads"] = json!(threads);
    if let Some(e) = thread_error {
        // Same shape as a section failure: the tab shows it rather than pretending
        // the map is empty, which would read as "no thread has a conversation".
        out["errors"]["threads"] = Value::String(e);
    }
    out.to_string()
}

/// A cached envelope, marked so a client can paint it and still say "refreshing".
///
/// `on_tasks` is one raw JSON string (no typed record), so the flag rides the
/// envelope instead of the signature: `cached: true` means "this came from disk,
/// the live refresh is still in flight". Unknown keys are ignored by clients that
/// do not look for it, which is why it is additive.
pub(crate) fn cached_envelope(json: &str) -> String {
    let mut out: Value = serde_json::from_str(json).unwrap_or_else(|_| json!({}));
    out["cached"] = Value::Bool(true);
    out.to_string()
}

/// Whether an envelope carries the sections at all AND every one of them came
/// back from the server (no `errors` entry for any section).
pub(crate) fn envelope_is_complete(json: &str) -> bool {
    let Ok(out) = serde_json::from_str::<Value>(json) else {
        return false;
    };
    let Some(errors) = out.get("errors") else {
        return out.get("open").is_some() && out.get("done").is_some() && out.get("tasks").is_some();
    };
    // `errors` present: complete only if it holds nothing but a thread-map
    // failure, which is not a section (the lists are still the board).
    match errors.as_object() {
        Some(map) => map.keys().all(|k| k == "threads"),
        None => false,
    }
}

pub(crate) fn envelope(
    open: Result<String, String>,
    done: Result<String, String>,
    tasks: Result<String, String>,
) -> String {
    let (open_items, open_err) = section(open, slim);
    let (done_items, done_err) = section(done, slim);
    let (task_items, task_err) = section(tasks, slim_task);
    let mut out = json!({ "open": open_items, "done": done_items, "tasks": task_items });
    let mut errors = serde_json::Map::new();
    if let Some(e) = open_err {
        errors.insert("open".into(), Value::String(e));
    }
    if let Some(e) = done_err {
        errors.insert("done".into(), Value::String(e));
    }
    if let Some(e) = task_err {
        errors.insert("tasks".into(), Value::String(e));
    }
    if !errors.is_empty() {
        out["errors"] = Value::Object(errors);
    }
    out.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_carry_the_extension_the_server_requires() {
        let p = open_params("s1", 50);
        assert_eq!(p["sessionId"], "s1");
        assert_eq!(p["extensionName"], EXTENSION);
        assert_eq!(p["name"], "assistantstate__state_items");
        assert_eq!(p["arguments"]["state"], "open,snoozed");
        assert_eq!(p["arguments"]["limit"], 50);
    }

    #[test]
    fn the_hidden_session_is_hidden_and_auto() {
        let p = new_session_params("/w");
        assert_eq!(p["_meta"]["hidden"], true);
        assert_eq!(p["cwd"], "/w");
        assert_eq!(p["mcpServers"], json!([]));
        // An empty cwd must not reach the server: it rejects a relative path.
        assert_eq!(new_session_params("")["cwd"], "/");
        assert_eq!(set_mode_params("s1", "auto")["modeId"], "auto");
    }

    #[test]
    fn actions_carry_the_ids_the_tools_expect() {
        // state_react keys on `item_id`; state_close and state_snooze key on `id`.
        assert_eq!(
            react_params("s", "i1", "acted")["arguments"]["item_id"],
            "i1"
        );
        assert_eq!(
            close_params("s", "i1", "done", "sent it")["arguments"]["id"],
            "i1"
        );
        assert_eq!(
            snooze_params("s", "i1", "2026-11-02T15:00:00Z", "later")["arguments"]["until"],
            "2026-11-02T15:00:00Z"
        );
    }

    #[test]
    fn content_text_concatenates_text_blocks_only() {
        let reply = json!({"content": [
            {"type": "text", "text": "[{\"id\": \"a\"}]"},
            {"type": "image", "data": "…"},
            {"type": "text", "text": ""},
        ], "isError": false});
        assert_eq!(content_text(&reply), "[{\"id\": \"a\"}]");
        assert!(content_text(&json!({})).is_empty());
    }

    #[test]
    fn errors_are_detected_in_either_spelling() {
        assert!(is_error(&json!({"isError": true})));
        assert!(is_error(&json!({"is_error": true})));
        assert!(!is_error(&json!({"isError": false})));
        assert!(!is_error(&json!({})));
    }

    #[test]
    fn envelope_parses_both_sections() {
        let got: Value = serde_json::from_str(&envelope(
            Ok(r#"[{"id":"a","title":"Ship it"}]"#.to_string()),
            Ok("[]".to_string()),
            Ok("[]".to_string()),
        ))
        .unwrap();
        assert_eq!(got["open"][0]["id"], "a");
        assert_eq!(got["done"], json!([]));
        assert!(got.get("errors").is_none());
    }

    #[test]
    fn envelope_reports_a_failed_section_without_losing_the_other() {
        let got: Value = serde_json::from_str(&envelope(
            Ok(r#"[{"id":"a"}]"#.to_string()),
            Err("not connected".to_string()),
            Ok("[]".to_string()),
        ))
        .unwrap();
        assert_eq!(got["open"][0]["id"], "a");
        assert_eq!(got["done"], json!([]));
        assert_eq!(got["errors"]["done"], "not connected");
        assert!(got["errors"].get("open").is_none());
    }

    #[test]
    fn a_non_array_payload_is_an_error_not_an_empty_win() {
        let got: Value = serde_json::from_str(&envelope(
            Ok(r#"{"detail":"boom"}"#.to_string()),
            Ok("not json".to_string()),
            Err("not connected".to_string()),
        ))
        .unwrap();
        assert_eq!(got["open"], json!([]));
        assert_eq!(got["done"], json!([]));
        assert!(got["errors"]["open"]
            .as_str()
            .unwrap()
            .contains("unexpected payload"));
        assert!(got["errors"]["done"].as_str().unwrap().contains("bad JSON"));
    }

    #[test]
    fn rows_keep_the_fields_a_row_needs_and_drop_the_evidence() {
        // The shape the live server answers with: the whole DB row, evidence
        // included. Evidence is the bulk of it (paragraph-long quotes).
        let raw = r#"[{"id":"itm_1","kind":"waiting","state":"open","title":"Ship it",
            "detail":"short","owner":"other:Mazdak","due_at":null,"salience":0.5,
            "confidence":0.8,"resolution":null,"dedupe_key":"k","sources":["nc_mail"],
            "evidence":[{"source":"nc_mail","at":"2026-10-27T13:06:57Z","text":"a long quote"}],
            "meta":{},"thread_id":"thr_1","first_seen":"2026-10-26T09:00:00Z",
            "last_evidence":"2026-10-27T13:06:57Z","closed_at":null,
            "thread":{"slug":"chami-billing","title":"chami billing"}}]"#;
        let got: Value =
            serde_json::from_str(&envelope(Ok(raw.to_string()), Ok("[]".to_string()), Ok("[]".to_string()))).unwrap();
        let row = &got["open"][0];
        assert_eq!(row["title"], "Ship it");
        // An OLDER server's `{slug, title}` still renders: the label is the title.
        assert_eq!(row["thread"], "chami billing");
        // Kept: the stamps the timeline is drawn from. They were on the wire all
        // along (state_items answers SELECT *), so a row with no date was a
        // projection bug, not a server one.
        assert_eq!(row["first_seen"], "2026-10-26T09:00:00Z");
        assert_eq!(row["last_evidence"], "2026-10-27T13:06:57Z");
        // Dropped: the tab never renders these, and they are the payload's bulk.
        assert!(row.get("evidence").is_none());
        assert!(row.get("dedupe_key").is_none());
        assert!(row.get("sources").is_none());
        assert!(row.get("thread_id").is_none());
    }

    #[test]
    fn a_v3_thread_label_passes_through_and_a_blank_one_is_dropped() {
        // The live shape since schema v3 (2026-10-29): `thread` is a nullable LABEL,
        // not `{slug, title}`. Reading it as an object dropped it from every row.
        let raw = r#"[{"id":"a","thread":"chami-billing","title":"Ship it"},
                      {"id":"b","thread":"  ","title":"blank"},
                      {"id":"c","thread":null,"title":"null"},
                      {"id":"d","title":"absent"}]"#;
        let got: Value =
            serde_json::from_str(&envelope(Ok(raw.to_string()), Ok("[]".to_string()), Ok("[]".to_string()))).unwrap();
        assert_eq!(got["open"][0]["thread"], "chami-billing");
        // A blank/absent label is no label: the meta line would otherwise carry a
        // stray separator for every unthreaded item, which is most of them.
        for i in 1..4 {
            assert!(got["open"][i].get("thread").is_none(), "row {i} kept a blank thread");
        }
    }

    #[test]
    fn the_thread_map_params_name_the_tools_the_server_serves() {
        assert_eq!(
            thread_map_params("s1")["name"],
            "assistantstate__thread_sessions"
        );
        assert_eq!(thread_map_params("s1")["extensionName"], EXTENSION);
        let link = link_params("s1", "nuclera-pricing", "20261104_9", "Thread: Nuclera pricing");
        assert_eq!(link["name"], "assistantstate__thread_link");
        assert_eq!(link["arguments"]["thread"], "nuclera-pricing");
        assert_eq!(link["arguments"]["session_id"], "20261104_9");
        // An empty id is the FORGET form: the session is gone, so the link is dropped
        // rather than pointing at a dead id forever.
        assert_eq!(link_params("s1", "a", "", "")["arguments"]["session_id"], "");
    }

    #[test]
    fn the_thread_map_parses_both_shapes_and_drops_junk() {
        let nested = parse_threads(r#"{"threads": {"a-one": "s1", "b-two": "s2"}}"#);
        assert_eq!(nested.get("a-one").map(String::as_str), Some("s1"));
        assert_eq!(nested.len(), 2);
        // A flattened reply is the same map with one less wrapper.
        let flat = parse_threads(r#"{"a-one": "s1"}"#);
        assert_eq!(flat.get("a-one").map(String::as_str), Some("s1"));
        // Garbage, a missing id and a blank label are all "no mapping", never an
        // entry a client would try to open.
        assert!(parse_threads("not json").is_empty());
        assert!(parse_threads(r#"{"threads": {"a": "", "": "s2", "c": 7}}"#).is_empty());
    }

    #[test]
    fn labels_come_from_the_rows_and_keep_their_order() {
        let open = r#"[{"id":"1","thread":"b-two"},{"id":"2","thread":"  "},
                       {"id":"3"},{"id":"4","thread":"b-two"},{"id":"5","thread":"a-one"}]"#;
        let handed = r#"[{"id":"6","thread":"a-one"},{"id":"7","thread":"c-three"}]"#;
        assert_eq!(
            labels(Some(open), Some(handed)),
            vec!["b-two".to_string(), "a-one".to_string(), "c-three".to_string()]
        );
        // A section that failed contributes nothing rather than emptying the set.
        assert_eq!(labels(None, Some(handed)), vec!["a-one".to_string(), "c-three".to_string()]);
        assert!(labels(None, None).is_empty());
    }

    #[test]
    fn a_thread_note_carries_the_detail_that_makes_the_title_answerable() {
        // The regression this guards, measured 2026-11-12: the seed was the title clipped
        // at 110 chars, and the title did not say WHO. The item's detail did.
        let open = r#"[{"id":"itm_1","thread":"heike-1-1","kind":"expectation","state":"open",
                        "due_at":"2026-11-16T14:00:00-07:00",
                        "title":"Heike moved your 1:1 to Monday Nov 16 at 2pm MST, right before Schaible's 3:10 seminar you haven't answered about",
                        "detail":"It lands 40 minutes before George Schaible's 15:10 seminar in PBB108, which Hatzenpichler is still waiting on an answer about."},
                       {"id":"itm_2","thread":"heike-1-1","kind":"question","state":"open",
                        "title":"Short","detail":"Short"}]"#;
        let notes = thread_notes(Some(open), None).remove("heike-1-1").unwrap_or_default();
        // A header naming the area and the tool that holds the rest, then one note per row.
        assert_eq!(notes.len(), 3);
        assert_eq!(notes[0].key, "thread:heike-1-1");
        assert!(notes[0].text.contains("assistantstate__state_items(thread=\"heike-1-1\")"),
                "{}", notes[0].text);
        // The title arrives WHOLE — no 110-char ellipsis, which is what removed "Schaible's".
        let first = &notes[1];
        assert!(first.text.contains("right before Schaible's 3:10 seminar you haven't answered about"),
                "{}", first.text);
        // And the detail lands with it, which is where the name lives.
        assert!(first.text.contains("George Schaible's 15:10 seminar in PBB108"), "{}", first.text);
        // The footer keeps the row addressable AND is the relay's dedupe key.
        assert!(first.text.contains("itm_1 · expectation · open · due 2026-11-16T14:00:00-07:00"),
                "{}", first.text);
        assert_eq!(first.key, "itm_1");
        // A detail identical to the title is not printed twice.
        assert_eq!(notes[2].text.matches("Short").count(), 1, "{}", notes[2].text);
        // A row with no label cannot seed a thread.
        assert!(thread_notes(Some(r#"[{"id":"9","title":"unlabelled"}]"#), None).is_empty());
    }

    #[test]
    fn a_thread_note_is_bounded_and_keyed_per_row() {
        let rows = (1..=7)
            .map(|n| format!(r#"{{"id":"itm_{n}","thread":"a-one","title":"Row {n}","detail":"d{n}"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let open = format!("[{rows}]");
        let notes = thread_notes(Some(&open), None).remove("a-one").unwrap_or_default();
        // Header + the cap: a note is re-sent on every turn of the conversation, so the
        // count is bounded by cost — the header says how many rows there really are.
        assert_eq!(notes.len(), NOTES_PER_THREAD + 1);
        assert!(notes[0].text.contains("7 row(s)"), "{}", notes[0].text);
        // Keys are the ROW ids, which is what makes re-offering idempotent per row.
        assert_eq!(notes[1].key, "itm_1");
        assert_eq!(notes[NOTES_PER_THREAD].key, format!("itm_{NOTES_PER_THREAD}"));
    }

    #[test]
    fn a_handed_over_row_notes_what_was_asked_and_what_came_back() {
        let handed = r#"[{"id":"tsk_1","thread":"a-one","state":"review",
                          "title":"Table for Heike",
                          "request":"Build the structure-by-structure table",
                          "result":"Sent 03:20 Fri"}]"#;
        let notes = thread_notes(None, Some(handed)).remove("a-one").unwrap_or_default();
        let note = &notes[1];
        assert!(note.text.contains("Build the structure-by-structure table"), "{}", note.text);
        assert!(note.text.contains("Sent 03:20 Fri"), "{}", note.text);
        assert!(note.text.ends_with("(tsk_1 · handed over · review)"), "{}", note.text);
        assert_eq!(note.key, "tsk_1");
    }

    #[test]
    fn the_note_params_name_the_relay_tool() {
        let p = note_params("s1", "a-one", "hello", "itm_1");
        assert_eq!(p["name"], "notify__thread_note");
        assert_eq!(p["extensionName"], NOTE_EXTENSION);
        assert_eq!(p["arguments"]["thread"], "a-one");
        // The key is the whole idempotence story: the relay writes only when the
        // conversation does not already mention it.
        assert_eq!(p["arguments"]["key"], "itm_1");
    }

    #[test]
    fn a_thread_title_matches_the_name_both_clients_have_always_used() {
        assert_eq!(thread_title("nuclera-pricing"), "Thread: Nuclera pricing");
        assert_eq!(thread_title("msu_cryoarm-contract"), "Thread: Msu cryoarm contract");
        assert_eq!(thread_title("  "), "");
    }

    #[test]
    fn the_envelope_carries_the_thread_map_and_its_error() {
        let mut threads = std::collections::BTreeMap::new();
        threads.insert("a-one".to_string(), "s1".to_string());
        let got: Value = serde_json::from_str(&envelope_threads(
            Ok("[]".to_string()),
            Ok("[]".to_string()),
            Ok("[]".to_string()),
            &threads,
            Some("not connected".to_string()),
        ))
        .unwrap();
        assert_eq!(got["threads"]["a-one"], "s1");
        assert_eq!(got["errors"]["threads"], "not connected");
        // Still the document the tab already paints.
        assert_eq!(got["open"], json!([]));
        assert_eq!(got["tasks"], json!([]));
    }

    #[test]
    fn task_params_ask_for_the_states_a_person_cares_about() {
        let p = task_params("s1", 20);
        assert_eq!(p["name"], "assistantstate__task_list");
        assert_eq!(p["arguments"]["state"], "running,review");
        assert_eq!(p["extensionName"], EXTENSION);
    }

    #[test]
    fn a_task_row_is_projected_to_its_own_fields() {
        // A task is not an item: no salience, no evidence, no confidence — but it
        // does carry what the work produced, and which session produced it.
        let raw = r#"[{"id":"tsk_1","title":"Board deck","state":"review",
                       "thread":"series-a","session_id":"sess-9",
                       "request":"build a deck from the data room",
                       "result":"12 slides","artifacts":[{"path":"/workspace/deck.pdf"}],
                       "created_at":"2026-10-30T20:30:21Z",
                       "updated_at":"2026-10-30T21:02:00Z",
                       "ended_at":null,"meta":{"match":{"by":"nothing"}}}]"#;
        let got: Value = serde_json::from_str(&envelope(
            Ok("[]".to_string()),
            Ok("[]".to_string()),
            Ok(raw.to_string()),
        ))
        .unwrap();
        let row = &got["tasks"][0];
        assert_eq!(row["state"], "review");
        assert_eq!(row["result"], "12 slides");
        assert_eq!(row["session_id"], "sess-9");
        assert_eq!(row["artifacts"][0]["path"], "/workspace/deck.pdf");
        assert_eq!(row["thread"], "series-a");
        // Server bookkeeping the tab never paints.
        assert!(row.get("meta").is_none());
        assert!(row.get("ended_at").is_none());
    }

    #[test]
    fn a_long_request_is_trimmed_and_an_empty_thread_is_dropped() {
        let long = "x".repeat(400);
        let raw = format!(
            r#"[{{"id":"tsk_1","title":"T","state":"running","request":"{long}","thread":"  "}}]"#
        );
        let got: Value = serde_json::from_str(&envelope(
            Ok("[]".to_string()),
            Ok("[]".to_string()),
            Ok(raw),
        ))
        .unwrap();
        let req = got["tasks"][0]["request"].as_str().unwrap();
        assert_eq!(req.chars().count(), REQUEST_CAP + 1); // + the ellipsis
        assert!(req.ends_with('…'));
        assert!(got["tasks"][0].get("thread").is_none());
    }

    #[test]
    fn a_failing_task_section_does_not_cost_the_item_lists() {
        let got: Value = serde_json::from_str(&envelope(
            Ok(r#"[{"id":"a"}]"#.to_string()),
            Ok("[]".to_string()),
            Err("unknown tool assistantstate__task_list".to_string()),
        ))
        .unwrap();
        assert_eq!(got["open"][0]["id"], "a");
        assert_eq!(got["tasks"], json!([]));
        // An older server that has no task_list says so, instead of painting an
        // empty section that reads like "nothing was handed over".
        assert!(got["errors"]["tasks"]
            .as_str()
            .unwrap()
            .contains("unknown tool"));
    }

    #[test]
    fn a_snoozed_row_is_distinguishable_from_an_awake_one() {
        // Both arrive in the same `open` section (snoozed is still open, just
        // quiet), so `snooze_until` is the ONLY difference the UI can group on.
        // Dropping it here is what made Snooze look like a no-op on the phone.
        let raw = r#"[{"id":"a","state":"snoozed","title":"quiet","snooze_until":"2026-10-30T11:00:00Z"},
                      {"id":"b","state":"open","title":"awake","snooze_until":null}]"#;
        let got: Value =
            serde_json::from_str(&envelope(Ok(raw.to_string()), Ok("[]".to_string()), Ok("[]".to_string()))).unwrap();
        assert_eq!(got["open"][0]["snooze_until"], "2026-10-30T11:00:00Z");
        assert_eq!(got["open"][1]["snooze_until"], Value::Null);
        assert_eq!(got["open"][0]["state"], "snoozed");
    }

    #[test]
    fn only_a_board_whose_sections_all_answered_is_cacheable() {
        // A cached board is replayed as if the server had just said it, so
        // "the open list failed" must never be remembered (it would paint as an
        // empty board on the next open). A thread-map failure is different: the
        // map is not a section, the lists are still the board.
        let good = envelope_threads(
            Ok("[]".to_string()),
            Ok("[]".to_string()),
            Ok("[]".to_string()),
            &std::collections::BTreeMap::new(),
            None,
        );
        assert!(envelope_is_complete(&good));
        let no_map = envelope_threads(
            Ok("[]".to_string()),
            Ok("[]".to_string()),
            Ok("[]".to_string()),
            &std::collections::BTreeMap::new(),
            Some("unreachable".to_string()),
        );
        assert!(envelope_is_complete(&no_map));
        for failed in [
            envelope_threads(
                Err("boom".to_string()),
                Ok("[]".to_string()),
                Ok("[]".to_string()),
                &std::collections::BTreeMap::new(),
                None,
            ),
            envelope_threads(
                Ok("[]".to_string()),
                Err("boom".to_string()),
                Ok("[]".to_string()),
                &std::collections::BTreeMap::new(),
                None,
            ),
            envelope_threads(
                Ok("[]".to_string()),
                Ok("[]".to_string()),
                Err("unknown tool".to_string()),
                &std::collections::BTreeMap::new(),
                None,
            ),
        ] {
            assert!(!envelope_is_complete(&failed), "{failed}");
        }
        // Anything unparseable is not a board either.
        assert!(!envelope_is_complete("not json"));
        assert!(!envelope_is_complete(r#"{"open": []}"#));
    }

    #[test]
    fn a_painted_cache_is_marked_so_the_tab_can_still_say_refreshing() {
        // The flag rides the envelope because `on_tasks` is one raw string and
        // the clients that do not look for it must keep working unchanged.
        let board = envelope(Ok("[]".to_string()), Ok("[]".to_string()), Ok("[]".to_string()));
        let got: Value = serde_json::from_str(&cached_envelope(&board)).unwrap();
        assert_eq!(got["cached"], Value::Bool(true));
        assert_eq!(got["open"], json!([]));
        // A stamp that cannot be parsed still yields a flag, never a crash.
        let broken: Value = serde_json::from_str(&cached_envelope("garbage")).unwrap();
        assert_eq!(broken["cached"], Value::Bool(true));
    }
}
