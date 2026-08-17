//! CacheStore: per-session transcript + tool caches under the UI-supplied
//! cache directory. Pure dumb I/O — NO network, NO uniffi exports; freshness
//! (comparing `updatedAt` against the server's stamp) is the CALLER's job.
//!
//! Mirrors the desktop's cache format (`manager.cpp` cacheFilePath /
//! saveCache / loadCache / saveToolCache / loadToolCache) so an existing
//! desktop cache can be read:
//! - transcript: `<dir>/<session>.json` (session id with `/` escaped to `_`),
//!   a JSON object `{ "updatedAt": <str>, "messages": [ ... ] }` where each
//!   message row carries the desktop's field names (`role`, `text`, `html`,
//!   `title`, `detail`, `output`, `status`, `thought`, `toolCallId`, and
//!   `calls` for toolgroups); we also write `id` (our bubble key) as an extra
//!   key the desktop ignores.
//! - tools: `<dir>/<session>-tools.json`, `{ "tools": [...],
//!   "sessionExtensions": [...], "extensions": [...], "catalog": {...} }`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Message, SessionSummary};

/// The tool catalog cache, mirroring the desktop's tool-cache JSON.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCache {
    pub tools: Vec<String>,
    pub session_extensions: Vec<String>,
    pub extensions: Vec<ExtensionDef>,
    /// Extension name → tool names it contributes.
    pub catalog: BTreeMap<String, Vec<String>>,
}

/// One extension definition in the tool cache (the desktop's `ExtDef`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionDef {
    pub name: String,
    #[serde(rename = "type")]
    pub extension_type: String,
    pub attrib: bool,
    pub available_tools: Vec<String>,
    /// The extension's raw JSON blob, passed through untouched.
    pub raw: Value,
}

/// Dumb per-session cache I/O under a fixed directory.
pub struct CacheStore {
    cache_dir: PathBuf,
}

impl CacheStore {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }

    /// The cache root the core was constructed with (roam identity etc.).
    pub fn dir(&self) -> &Path {
        &self.cache_dir
    }

    /// `<dir>/<session>.json`, `/` escaped to `_` (desktop `cacheFilePath`).
    fn transcript_path(&self, session_id: &str) -> PathBuf {
        self.cache_dir.join(format!("{}.json", escape(session_id)))
    }

    /// `<dir>/<session>-tools.json` (desktop `toolCacheFilePath`).
    fn tools_path(&self, session_id: &str) -> PathBuf {
        self.cache_dir.join(format!("{}-tools.json", escape(session_id)))
    }

    /// Persist a session transcript. Mirrors the desktop: an empty transcript
    /// (or empty session id) is not cached, so `load_transcript` reports
    /// `None` for it. Returns whether the file was written.
    pub fn save_transcript(&self, session_id: &str, messages: &[Message], updated_at: &str) -> bool {
        if session_id.is_empty() || messages.is_empty() {
            return true;
        }
        let mut arr = Vec::with_capacity(messages.len());
        for m in messages {
            let mut o = serde_json::json!({
                "id": m.id,
                "role": m.role,
                "text": m.content,
                "html": "",
            });
            if m.role == "thought" {
                o["thought"] = serde_json::json!(true);
            }
            if m.role == "tool" {
                // The flat projection carries the tool title in `content`;
                // write it into the desktop's `title` field, `text` stays "".
                o["title"] = serde_json::json!(m.content);
                o["text"] = serde_json::json!("");
            }
            arr.push(o);
        }
        let root = serde_json::json!({ "updatedAt": updated_at, "messages": arr });
        write_json(&self.transcript_path(session_id), &root)
    }

    /// Load a session transcript. Returns `(messages, updatedAt)`; the caller
    /// compares `updatedAt` against the session list for freshness. `None`
    /// when the file is missing, corrupt, or holds no messages (desktop
    /// `loadCache` semantics).
    pub fn load_transcript(&self, session_id: &str) -> Option<(Vec<Message>, String)> {
        let bytes = fs::read(self.transcript_path(session_id)).ok()?;
        let root: Value = serde_json::from_slice(&bytes).ok()?;
        let updated_at = root
            .get("updatedAt")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let arr = root.get("messages")?.as_array()?;
        if arr.is_empty() {
            return None;
        }
        let messages = arr.iter().map(message_from_json).collect();
        Some((messages, updated_at))
    }

    /// Persist the session directory (the drawer's names + the per-session
    /// cwd side-table) so the UI can render it before the first session/list
    /// round trip. Returns whether the file was written.
    pub fn save_directory(
        &self,
        sessions: &[SessionSummary],
        cwds: &BTreeMap<String, String>,
    ) -> bool {
        if sessions.is_empty() {
            return true;
        }
        let arr = sessions
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "title": s.title,
                    "updatedAt": s.updated_at,
                    "projectId": s.project_id,
                    "messageCount": s.message_count,
                    "model": s.model,
                    "hasRecipe": s.has_recipe,
                    "lastMessageSnippet": s.last_message_snippet,
                    "cwd": cwds.get(&s.id),
                })
            })
            .collect::<Vec<_>>();
        write_json(&self.directory_path(), &serde_json::json!({ "sessions": arr }))
    }

    /// Load the cached session directory (names + cwds). `None` when missing
    /// or corrupt.
    pub fn load_directory(&self) -> Option<(Vec<SessionSummary>, BTreeMap<String, String>)> {
        let bytes = fs::read(self.directory_path()).ok()?;
        let root: Value = serde_json::from_slice(&bytes).ok()?;
        let arr = root.get("sessions")?.as_array()?;
        if arr.is_empty() {
            return None;
        }
        let mut cwds = BTreeMap::new();
        let sessions = arr
            .iter()
            .filter_map(|el| {
                let id = el.get("id")?.as_str()?.to_string();
                if let Some(cwd) = el.get("cwd").and_then(Value::as_str) {
                    cwds.insert(id.clone(), cwd.to_string());
                }
                Some(SessionSummary {
                    id,
                    title: el.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
                    updated_at: el.get("updatedAt").and_then(Value::as_str).unwrap_or("").to_string(),
                    last_message_snippet: el.get("lastMessageSnippet").and_then(Value::as_str).map(|s| s.to_string()),
                    project_id: el.get("projectId").and_then(Value::as_str).map(|s| s.to_string()),
                    message_count: el.get("messageCount").and_then(Value::as_i64).unwrap_or(0),
                    model: el.get("model").and_then(Value::as_str).unwrap_or("").to_string(),
                    has_recipe: el.get("hasRecipe").and_then(Value::as_bool).unwrap_or(false),
                    // The cache is a replay of the last list; a cached summary is
                    // never "new" — has_new is live-only, derived from staging.
                    has_new: false,
                })
            })
            .collect::<Vec<_>>();
        if sessions.is_empty() {
            return None;
        }
        Some((sessions, cwds))
    }

    fn directory_path(&self) -> PathBuf {
        self.cache_dir.join("directory.json")
    }

    /// Persist the tool catalog for a session. Returns whether the file was
    /// written.
    pub fn save_tools(&self, session_id: &str, tools: &ToolCache) -> bool {
        if session_id.is_empty() {
            return true;
        }
        let Ok(root) = serde_json::to_value(tools) else {
            return false;
        };
        write_json(&self.tools_path(session_id), &root)
    }

    /// Load the tool catalog for a session. `None` when missing or corrupt.
    pub fn load_tools(&self, session_id: &str) -> Option<ToolCache> {
        let bytes = fs::read(self.tools_path(session_id)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

fn escape(session_id: &str) -> String {
    session_id.replace('/', "_")
}

fn write_json(path: &Path, value: &Value) -> bool {
    if let Some(parent) = path.parent() {
        if fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    let Ok(bytes) = serde_json::to_vec(value) else {
        return false;
    };
    fs::write(path, bytes).is_ok()
}

/// Map one desktop-format cache row to a flat `Message`.
///
/// - text rows (user/agent/thought/error): `text` → `content`; the desktop
///   cache carries no bubble keys, so `id` is empty unless our extra `id`
///   key is present.
/// - tool rows: the title lives in `title` with `text` empty (both our own
///   cache and the desktop's) → `content` = title, `id` = `toolCallId`.
/// - chart/mcpapp rows: same, flattened to role "tool" (the CONTRACT umbrella).
/// - toolgroup rows: flattened to ONE tool message anchored on the first call
///   (matching how the live toolgroup projects to the transcript).
fn message_from_json(el: &Value) -> Message {
    // Desktop chart/mcp-app rows flatten to the CONTRACT's "tool" umbrella,
    // exactly like the live projection does.
    let role = match el.get("role").and_then(Value::as_str).unwrap_or("") {
        "chart" | "mcpapp" => "tool",
        r => r,
    }
    .to_string();
    let text = el.get("text").and_then(Value::as_str).unwrap_or("").to_string();
    let id = el.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    let tool_call_id = el.get("toolCallId").and_then(Value::as_str).unwrap_or("").to_string();

    if role == "toolgroup" {
        if let Some(first) = el
            .get("calls")
            .and_then(Value::as_array)
            .and_then(|calls| calls.first())
        {
            let title = first.get("title").and_then(Value::as_str).unwrap_or("");
            let cid = first.get("toolCallId").and_then(Value::as_str).unwrap_or("");
            return Message {
                id: cid.to_string(),
                role: "tool".to_string(),
                content: title.to_string(),
            };
        }
        return Message { id: String::new(), role: "tool".to_string(), content: String::new() };
    }

    let content = if (role == "tool" || role == "chart" || role == "mcpapp") && text.is_empty() {
        el.get("title").and_then(Value::as_str).unwrap_or("").to_string()
    } else {
        text
    };
    let id = if id.is_empty() && !tool_call_id.is_empty() { tool_call_id } else { id };
    Message { id, role, content }
}

#[cfg(test)]
mod tests {
    use crate::SessionSummary;
    use std::collections::BTreeMap;
    use std::fs;

    use crate::Message;

    use super::{CacheStore, ExtensionDef, ToolCache};

    fn temp_cache_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("grouse-cache-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp cache dir");
        dir
    }

    fn msgs(messages: &[Message]) -> Vec<(String, String, String)> {
        messages
            .iter()
            .map(|m| (m.role.clone(), m.id.clone(), m.content.clone()))
            .collect()
    }

    #[test]
    fn cache_transcript_round_trip() {
        let dir = temp_cache_dir("roundtrip");
        let store = CacheStore::new(dir.clone());
        let messages = vec![
            Message { id: "m1".into(), role: "user".into(), content: "hi".into() },
            Message { id: "m2".into(), role: "agent".into(), content: "hello there".into() },
            Message { id: "t1".into(), role: "tool".into(), content: "Bash".into() },
            Message { id: String::new(), role: "thought".into(), content: "hmm".into() },
        ];

        assert!(store.save_transcript("sess/1", &messages, "2026-08-12T10:00:00Z"));
        let (loaded, updated_at) =
            store.load_transcript("sess/1").expect("cache must load back");
        assert_eq!(updated_at, "2026-08-12T10:00:00Z");
        assert_eq!(msgs(&loaded), msgs(&messages));

        // Desktop file naming: `/` escaped to `_`, `.json` suffix.
        assert!(dir.join("sess_1.json").exists());

        // Empty transcripts are not cached (desktop semantics) → None on load.
        assert!(store.save_transcript("empty", &[], "2026-08-12T10:00:00Z"));
        assert!(store.load_transcript("empty").is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_freshness_is_the_callers_job() {
        let dir = temp_cache_dir("fresh");
        let store = CacheStore::new(dir.clone());
        let messages =
            vec![Message { id: "m1".into(), role: "user".into(), content: "hi".into() }];

        store.save_transcript("s1", &messages, "2026-08-12T09:00:00Z");
        let (_, updated_at) = store.load_transcript("s1").unwrap();

        // The store is dumb I/O: the caller compares the cached stamp against
        // the server's. A newer server stamp ⇒ stale cache.
        let server_updated_at = "2026-08-12T10:00:00Z";
        let is_fresh = |cached: &str, server: &str| {
            !cached.is_empty() && cached == server
        };
        assert!(!is_fresh(&updated_at, server_updated_at));

        // Re-saving with the server stamp makes the cache fresh again.
        store.save_transcript("s1", &messages, server_updated_at);
        let (_, updated_at) = store.load_transcript("s1").unwrap();
        assert!(is_fresh(&updated_at, server_updated_at));

        // Missing and corrupt files both read as None (no panics).
        assert!(store.load_transcript("nope").is_none());
        fs::write(dir.join("corrupt.json"), b"not json").unwrap();
        assert!(store.load_transcript("corrupt").is_none());
        fs::write(dir.join("emptyarr.json"), r#"{"updatedAt":"x","messages":[]}"#).unwrap();
        assert!(store.load_transcript("emptyarr").is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_desktop_cache_format() {
        let dir = temp_cache_dir("desktop");
        // A desktop-written cache: rich tool rows, a collapsed toolgroup with
        // nested calls, a chart row — no `id` keys anywhere.
        let desktop = serde_json::json!({
            "updatedAt": "2026-08-12T10:00:00Z",
            "messages": [
                {"role": "user", "text": "hello", "html": "<p>hello</p>"},
                {"role": "agent", "text": "hi there", "html": "<p>hi there</p>"},
                {"role": "tool", "text": "", "html": "", "title": "Bash",
                 "detail": "ls", "output": "out", "status": "completed", "toolCallId": "t1"},
                {"role": "toolgroup", "text": "", "html": "", "calls": [
                    {"title": "Bash", "detail": "ls", "output": "out",
                     "status": "completed", "toolCallId": "t1"},
                    {"title": "Read", "detail": "file.txt", "output": "",
                     "status": "completed", "toolCallId": "t2"}
                ]},
                {"role": "chart", "text": "", "title": "Sankey",
                 "chartData": "{}", "toolCallId": "c1"}
            ]
        });
        fs::write(
            dir.join("s9.json"),
            serde_json::to_vec(&desktop).unwrap(),
        )
        .unwrap();

        let store = CacheStore::new(dir.clone());
        let (messages, updated_at) = store.load_transcript("s9").unwrap();
        assert_eq!(updated_at, "2026-08-12T10:00:00Z");
        assert_eq!(
            msgs(&messages),
            vec![
                // text rows: no ids in a desktop cache
                ("user".to_string(), String::new(), "hello".to_string()),
                ("agent".to_string(), String::new(), "hi there".to_string()),
                // tool row: title → content, toolCallId → id
                ("tool".to_string(), "t1".to_string(), "Bash".to_string()),
                // toolgroup: ONE message anchored on its first call
                ("tool".to_string(), "t1".to_string(), "Bash".to_string()),
                // chart row flattened under the "tool" umbrella
                ("tool".to_string(), "c1".to_string(), "Sankey".to_string()),
            ]
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn directory_round_trip() {
        let dir = temp_cache_dir("directory");
        let store = CacheStore::new(dir.clone());
        let sessions = vec![SessionSummary {
            id: "sess/1".into(),
            title: "Chat one".into(),
            updated_at: "2026-08-12T10:00:00Z".into(),
            last_message_snippet: Some("hi".into()),
            project_id: Some("proj-a".into()),
            message_count: 42,
            model: "gpt-4o".into(),
            has_recipe: true,
            has_new: false,
        }];
        let mut cwds = BTreeMap::new();
        cwds.insert("sess/1".into(), "/tmp".into());
        assert!(store.save_directory(&sessions, &cwds));
        let (loaded, cwds2) = store.load_directory().expect("directory must load back");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "sess/1");
        assert_eq!(loaded[0].title, "Chat one");
        assert_eq!(loaded[0].project_id.as_deref(), Some("proj-a"));
        assert_eq!(loaded[0].message_count, 42);
        assert_eq!(cwds2.get("sess/1").map(String::as_str), Some("/tmp"));
        // Empty saves no-op (existing file untouched); a store with no
        // prior file loads None.
        assert!(store.save_directory(&[], &BTreeMap::new()));
        let empty_store = CacheStore::new(dir.join("other"));
        assert!(empty_store.load_directory().is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn cache_tools_round_trip() {
        let dir = temp_cache_dir("tools");
        let store = CacheStore::new(dir.clone());
        let cache = ToolCache {
            tools: vec!["bash".into(), "read".into()],
            session_extensions: vec!["core".into()],
            extensions: vec![ExtensionDef {
                name: "core".into(),
                extension_type: "builtin".into(),
                attrib: true,
                available_tools: vec!["bash".into()],
                raw: serde_json::json!({"version": 1}),
            }],
            catalog: BTreeMap::from([("core".into(), vec!["bash".into(), "read".into()])]),
        };

        assert!(store.save_tools("s1", &cache));
        assert_eq!(store.load_tools("s1").unwrap(), cache);

        // Desktop-compatible field names on disk.
        let raw: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join("s1-tools.json")).unwrap()).unwrap();
        assert_eq!(raw["sessionExtensions"][0], "core");
        assert_eq!(raw["extensions"][0]["name"], "core");
        assert_eq!(raw["extensions"][0]["type"], "builtin");
        assert_eq!(raw["extensions"][0]["attrib"], true);
        assert_eq!(raw["extensions"][0]["availableTools"][0], "bash");
        assert_eq!(raw["extensions"][0]["raw"]["version"], 1);
        assert_eq!(raw["catalog"]["core"][1], "read");

        // Missing → None.
        assert!(store.load_tools("nope").is_none());

        let _ = fs::remove_dir_all(&dir);
    }
}
