// SPDX-License-Identifier: AGPL-3.0-or-later

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

use crate::{Item, SessionSummary};

/// Transcript-cache format version.
///
/// Bumped to 2 when the transcript store stopped allowing a replay to append
/// onto a painted cache: caches written before that could hold painted rows
/// FOLLOWED by replayed ones — not the server's order — which could show an old
/// message as the newest on screen, and `save_transcript` stamps the file with
/// the client's own last-known `updatedAt`, so such a cache compared equal to
/// the server's stamp and looked "fresh" forever, suppressing the very replay
/// that would correct it. A v1 file is therefore not merely stale but
/// MIS-ORDERED, so it must not be trusted even when its stamp matches: reading
/// it returns `None` and the next open takes the replay path, which rewrites it
/// in the server's order. This is what self-heals caches already on disk.
/// Bumped to 3 when the cache became the RICH item list
/// (`docs/TRANSCRIPT_MODEL.md`): a v2 file holds the lossy flat `Message`
/// projection, which cannot restore a chart's spec, an MCP app's key, or a
/// toolgroup's children. Reading a v2 file would silently degrade those rows, so
/// it returns `None` and the next open replays and rewrites in the rich shape.
pub const TRANSCRIPT_CACHE_VERSION: u64 = 3;

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

    /// Persist a session transcript as the RICH item list. An empty transcript
    /// (or empty session id) is not cached, so `load_transcript` reports
    /// `None` for it. Returns whether the file was written.
    pub fn save_transcript(&self, session_id: &str, items: &[Item], updated_at: &str) -> bool {
        if session_id.is_empty() || items.is_empty() {
            return true;
        }
        let Ok(arr) = serde_json::to_value(items) else {
            return false;
        };
        let root = serde_json::json!({
            "v": TRANSCRIPT_CACHE_VERSION,
            "updatedAt": updated_at,
            "items": arr,
        });
        write_json(&self.transcript_path(session_id), &root)
    }

    /// Load a session transcript as the rich item list. Returns `(items,
    /// updatedAt)`; the caller compares `updatedAt` against the session list for
    /// freshness. `None` when the file is missing, corrupt, or holds no items
    /// (desktop `loadCache` semantics).
    pub fn load_transcript(&self, session_id: &str) -> Option<(Vec<Item>, String)> {
        let bytes = fs::read(self.transcript_path(session_id)).ok()?;
        let root: Value = serde_json::from_slice(&bytes).ok()?;
        // A pre-rich cache is lossy (see TRANSCRIPT_CACHE_VERSION) and its own
        // stamp can make it look fresh, so treat it as absent: the caller then
        // replays and rewrites it. One-time; the format is stamped from here on.
        if root.get("v").and_then(Value::as_u64) != Some(TRANSCRIPT_CACHE_VERSION) {
            return None;
        }
        let updated_at = root
            .get("updatedAt")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let arr = root.get("items")?.as_array()?;
        if arr.is_empty() {
            return None;
        }
        let items: Vec<Item> = arr
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        if items.is_empty() {
            return None;
        }
        Some((items, updated_at))
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
                    archived: false,
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
    let Ok(bytes) = serde_json::to_vec(value) else {
        return false;
    };
    atomic_write(path, &bytes).is_ok()
}

/// Write `data` to `path` atomically (S-RC-5): a temp file in the same
/// directory, fsync'd, then renamed over the target — mirroring the desktop's
/// mktemp+swap discipline in `scripts/build-android-libs.sh`. A crash or
/// partial write never leaves a truncated/zero-length cache file, and the
/// rename is atomic on POSIX.
pub(crate) fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.tmp{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("cache"),
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    // Durable rename: fsync the directory so the swap survives a crash.
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

/// Force 0600 perms on a file (POSIX), for the roam identity secret (S-RC-5).
#[cfg(unix)]
pub(crate) fn make_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(test)]
mod tests {
    use crate::SessionSummary;
    use std::collections::BTreeMap;
    use std::fs;

    use crate::{Item, ItemKind, ToolCall};

    use super::{CacheStore, ExtensionDef, ToolCache};

    fn temp_cache_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("grouse-cache-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp cache dir");
        dir
    }

    fn text(id: &str, kind: ItemKind, t: &str) -> Item {
        Item {
            id: id.into(),
            kind,
            text: t.into(),
            detail: String::new(),
            output: String::new(),
            status: String::new(),
            app_key: String::new(),
            calls: Vec::new(),
        }
    }

    #[test]
    fn cache_transcript_round_trip() {
        let dir = temp_cache_dir("roundtrip");
        let store = CacheStore::new(dir.clone());
        let items = vec![
            text("m1", ItemKind::User, "hi"),
            text("m2", ItemKind::Agent, "hello there"),
            Item {
                id: "t1".into(),
                kind: ItemKind::Tool,
                text: "Bash".into(),
                detail: "ls".into(),
                output: "out".into(),
                status: "completed".into(),
                app_key: String::new(),
                calls: Vec::new(),
            },
            text("@1", ItemKind::Thought, "hmm"),
        ];

        assert!(store.save_transcript("sess/1", &items, "2026-08-12T10:00:00Z"));
        let (loaded, updated_at) =
            store.load_transcript("sess/1").expect("cache must load back");
        assert_eq!(updated_at, "2026-08-12T10:00:00Z");
        assert_eq!(loaded, items, "the rich snapshot must round-trip exactly");

        // Desktop file naming: `/` escaped to `_`, `.json` suffix.
        assert!(dir.join("sess_1.json").exists());

        // Empty transcripts are not cached (desktop semantics) → None on load.
        assert!(store.save_transcript("empty", &[], "2026-08-12T10:00:00Z"));
        assert!(store.load_transcript("empty").is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rich_cache_round_trips_charts_apps_and_toolgroups() {
        // The whole point of the rich cache: a chart keeps its spec, an MCP app
        // keeps its key, and a collapsed toolgroup keeps its children — none of
        // which the old flat `Message` projection could carry.
        let dir = temp_cache_dir("rich");
        let store = CacheStore::new(dir.clone());
        let items = vec![
            Item {
                id: "c1".into(),
                kind: ItemKind::Chart,
                text: "Sankey".into(),
                detail: r#"{"nodes":[]}"#.into(),
                output: String::new(),
                status: "completed".into(),
                app_key: String::new(),
                calls: Vec::new(),
            },
            Item {
                id: "a1".into(),
                kind: ItemKind::McpApp,
                text: "Dashboard".into(),
                detail: r#"{"range":"7d"}"#.into(),
                output: String::new(),
                status: "completed".into(),
                app_key: "assistantmonitor|ui://x/dashboard".into(),
                calls: Vec::new(),
            },
            Item {
                id: "t1".into(),
                kind: ItemKind::ToolGroup,
                text: String::new(),
                detail: String::new(),
                output: String::new(),
                status: String::new(),
                app_key: String::new(),
                calls: vec![
                    ToolCall {
                        id: "t1".into(),
                        title: "Bash".into(),
                        detail: "ls".into(),
                        output: "a".into(),
                        status: "completed".into(),
                    },
                    ToolCall {
                        id: "t2".into(),
                        title: "Read".into(),
                        detail: "f".into(),
                        output: "b".into(),
                        status: "completed".into(),
                    },
                ],
            },
        ];
        assert!(store.save_transcript("s1", &items, "2026-08-12T10:00:00Z"));
        let (loaded, _) = store.load_transcript("s1").unwrap();
        assert_eq!(loaded, items);
        assert_eq!(loaded[1].app_key, "assistantmonitor|ui://x/dashboard");
        assert_eq!(loaded[2].calls.len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pre_rich_transcript_caches_are_never_trusted() {
        // A v2 file holds the lossy flat projection (its own stamp can make it
        // look fresh) and cannot restore charts/apps, so it must read as absent
        // and force the replay that rewrites it in the rich shape.
        let dir = temp_cache_dir("version");
        let store = CacheStore::new(dir.clone());
        fs::write(
            dir.join("s1.json"),
            r#"{"v":2,"updatedAt":"2026-08-12T10:00:00Z","messages":[{"id":"m1","role":"agent","text":"old","html":""}]}"#,
        )
        .unwrap();
        assert!(
            store.load_transcript("s1").is_none(),
            "a pre-rich cache must force a replay, not be trusted"
        );

        // The current format is trusted, and carries the stamp through.
        let items = vec![text("m1", ItemKind::Agent, "new")];
        assert!(store.save_transcript("s1", &items, "2026-08-12T10:00:00Z"));
        let (loaded, updated_at) = store.load_transcript("s1").expect("v3 cache loads");
        assert_eq!(updated_at, "2026-08-12T10:00:00Z");
        assert_eq!(loaded, items);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_freshness_is_the_callers_job() {
        let dir = temp_cache_dir("fresh");
        let store = CacheStore::new(dir.clone());
        let items = vec![text("m1", ItemKind::User, "hi")];

        store.save_transcript("s1", &items, "2026-08-12T09:00:00Z");
        let (_, updated_at) = store.load_transcript("s1").unwrap();

        // The store is dumb I/O: the caller compares the cached stamp against
        // the server's. A newer server stamp ⇒ stale cache.
        let server_updated_at = "2026-08-12T10:00:00Z";
        let is_fresh = |cached: &str, server: &str| {
            !cached.is_empty() && cached == server
        };
        assert!(!is_fresh(&updated_at, server_updated_at));

        // Re-saving with the server stamp makes the cache fresh again.
        store.save_transcript("s1", &items, server_updated_at);
        let (_, updated_at) = store.load_transcript("s1").unwrap();
        assert!(is_fresh(&updated_at, server_updated_at));

        // Missing and corrupt files both read as None (no panics).
        assert!(store.load_transcript("nope").is_none());
        fs::write(dir.join("corrupt.json"), b"not json").unwrap();
        assert!(store.load_transcript("corrupt").is_none());
        fs::write(dir.join("emptyarr.json"), r#"{"v":3,"updatedAt":"x","items":[]}"#).unwrap();
        assert!(store.load_transcript("emptyarr").is_none());

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
            archived: false,
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
