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

/// One section of the tab: a parsed item array, or the reason there is none.
fn section(result: Result<String, String>) -> (Value, Option<String>) {
    match result {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(mut v @ Value::Array(_)) => {
                slim(&mut v);
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
const KEPT_FIELDS: [&str; 10] = [
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
];

fn slim(items: &mut Value) {
    let Value::Array(rows) = items else { return };
    for row in rows.iter_mut() {
        let Value::Object(map) = row else { continue };
        map.retain(|k, _| KEPT_FIELDS.contains(&k.as_str()) || k == "thread");
        // `thread` is `{slug, title}` or null; the slug is what a message about
        // the item references, the title is what the section header shows.
        if let Some(Value::Object(t)) = map.get_mut("thread") {
            t.retain(|k, _| k == "slug" || k == "title");
        }
    }
}

/// The JSON `on_tasks` carries: `{"open": [...], "done": [...], "errors": {...}}`.
///
/// `errors` is keyed by section and omitted when both sections succeeded, so the
/// UI's happy path is a plain pair of arrays and a failure is legible instead of
/// an empty list that looks like "nothing to do".
pub(crate) fn envelope(open: Result<String, String>, done: Result<String, String>) -> String {
    let (open_items, open_err) = section(open);
    let (done_items, done_err) = section(done);
    let mut out = json!({ "open": open_items, "done": done_items });
    let mut errors = serde_json::Map::new();
    if let Some(e) = open_err {
        errors.insert("open".into(), Value::String(e));
    }
    if let Some(e) = done_err {
        errors.insert("done".into(), Value::String(e));
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
            "meta":{},"thread_id":"thr_1",
            "thread":{"slug":"chami-billing","title":"chami billing"}}]"#;
        let got: Value =
            serde_json::from_str(&envelope(Ok(raw.to_string()), Ok("[]".to_string()))).unwrap();
        let row = &got["open"][0];
        assert_eq!(row["title"], "Ship it");
        assert_eq!(row["thread"]["slug"], "chami-billing");
        // Dropped: the tab never renders these, and they are the payload's bulk.
        assert!(row.get("evidence").is_none());
        assert!(row.get("dedupe_key").is_none());
        assert!(row.get("sources").is_none());
        assert!(row.get("thread_id").is_none());
    }
}
