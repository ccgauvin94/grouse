// SPDX-License-Identifier: AGPL-3.0-or-later

//! TranscriptStore: chunk accumulation → Message bubbles; emits `on_stream` +
//! `on_transcript`. Pure internal state — NO network, NO uniffi exports.
#![allow(clippy::type_complexity)] // test helper returns event-recorder tuples
//!
//! Ports the desktop's streaming logic (`manager.cpp` appendChunk /
//! onToolCall / onToolCallUpdate):
//! - bubbles are keyed by `(role, message_id)`; a new message_id (or a role
//!   change, or a tool call between chunks) starts a new bubble, consecutive
//!   same-role chunks with the same id accumulate;
//! - consecutive *plain* tool calls collapse into a single toolgroup bubble
//!   (chart/mcp-app calls always stay standalone, exactly like the desktop);
//! - live tool output APPENDS, the completion update REPLACES;
//! - stream chunks / tool events / usage / run-ended go out on `on_stream`;
//!   bubble structure changes (append / update / clear) on `on_transcript`.

use std::collections::HashMap;

use parking_lot::Mutex;

use crate::{
    CoreListener, Item, ItemKind, Message, StreamEvent, ToolCall, ToolCallKind, TranscriptEvent,
    TranscriptOp, TranscriptWindow,
};

/// One tool call in the transcript: a standalone bubble or one entry of a
/// collapsed toolgroup. Mirrors the desktop's tool-row fields.
#[derive(Debug, Clone, PartialEq)]
struct ToolRow {
    title: String,
    detail: String,
    tool_call_id: String,
    output: String,
    status: String,
    /// `McpApp` only: `<extension>|<uri>`, the resource-read key. Retained so
    /// the cache round-trips the app and a rebuild can re-fetch its html.
    app_key: String,
    /// `Chart` only: the chart spec, retained for the same reason.
    spec: String,
}

impl ToolRow {
    fn new(title: &str, detail: &str, tool_call_id: &str) -> Self {
        Self {
            title: title.to_string(),
            detail: detail.to_string(),
            tool_call_id: tool_call_id.to_string(),
            output: String::new(),
            // The desktop stamps every fresh call "in_progress".
            status: "in_progress".to_string(),
            app_key: String::new(),
            spec: String::new(),
        }
    }

    /// The rich projection of this row (a toolgroup child, or a lone tool).
    fn to_call(&self) -> ToolCall {
        ToolCall {
            id: self.tool_call_id.clone(),
            title: self.title.clone(),
            detail: self.detail.clone(),
            output: self.output.clone(),
            status: self.status.clone(),
        }
    }
}

/// An accumulated transcript bubble. Text bubbles carry user/agent/thought
/// content; the tool-ish bubbles mirror the desktop's tool / toolgroup /
/// chart / mcpapp rows.
#[derive(Debug, Clone, PartialEq)]
enum Bubble {
    Text {
        /// Bubble key (`message_id`); empty for live bubbles without an id.
        id: String,
        /// Stable RICH item id: `message_id` when the server sent one, else a
        /// core-assigned `@n` (two live text rows can share a role+id-less
        /// shape, so the legacy `id` is not unique enough for a keyed store).
        uid: String,
        role: String,
        text: String,
        thought: bool,
    },
    /// A lone plain tool call (collapses into a `ToolGroup` on the next call).
    Tool(ToolRow),
    /// Consecutive plain tool calls collapsed into one bubble.
    ToolGroup(Vec<ToolRow>),
    /// `ToolCallKind::Chart` — always a standalone bubble (desktop "chart" row).
    Chart(ToolRow),
    /// `ToolCallKind::McpApp` — always a standalone bubble (desktop "mcpapp" row).
    McpApp(ToolRow),
}

impl Bubble {
    /// The flat `Message` projection. All tool-ish bubbles project to role
    /// "tool" (the CONTRACT's umbrella role); a toolgroup collapses to ONE
    /// message anchored on its first call, which is what makes the collapse
    /// observable through `transcript()`.
    fn project(&self) -> Message {
        match self {
            Bubble::Text { id, role, text, .. } => Message {
                id: id.clone(),
                role: role.clone(),
                content: text.clone(),
                output: String::new(),
            },
            Bubble::Tool(r) => Message {
                id: r.tool_call_id.clone(),
                role: "tool".to_string(),
                content: r.title.clone(),
                output: String::new(),
            },
            Bubble::ToolGroup(calls) => {
                let first = &calls[0];
                Message {
                    id: first.tool_call_id.clone(),
                    role: "tool".to_string(),
                    content: first.title.clone(),
                    output: String::new(),
                }
            }
            Bubble::Chart(r) | Bubble::McpApp(r) => Message {
                id: r.tool_call_id.clone(),
                role: "tool".to_string(),
                content: r.title.clone(),
                output: String::new(),
            },
        }
    }

    /// The rich item projection (docs/TRANSCRIPT_MODEL.md): keeps the row's
    /// kind and payload, so a chart stays a chart and an MCP app keeps its key.
    fn item(&self) -> Item {
        match self {
            Bubble::Text { uid, role, text, .. } => Item {
                id: uid.clone(),
                kind: match role.as_str() {
                    "user" => ItemKind::User,
                    "thought" => ItemKind::Thought,
                    "error" => ItemKind::Error,
                    _ => ItemKind::Agent,
                },
                text: text.clone(),
                ..empty_item()
            },
            Bubble::Tool(r) => Item {
                id: r.tool_call_id.clone(),
                kind: ItemKind::Tool,
                text: r.title.clone(),
                detail: r.detail.clone(),
                output: r.output.clone(),
                status: r.status.clone(),
                ..empty_item()
            },
            Bubble::ToolGroup(calls) => Item {
                // Anchored on the first call (the legacy projection's rule too).
                id: calls[0].tool_call_id.clone(),
                kind: ItemKind::ToolGroup,
                calls: calls.iter().map(ToolRow::to_call).collect(),
                ..empty_item()
            },
            Bubble::Chart(r) => Item {
                id: r.tool_call_id.clone(),
                kind: ItemKind::Chart,
                text: r.title.clone(),
                // The spec, not the tool input: a chart row has no result.
                detail: r.spec.clone(),
                status: r.status.clone(),
                ..empty_item()
            },
            Bubble::McpApp(r) => Item {
                id: r.tool_call_id.clone(),
                kind: ItemKind::McpApp,
                text: r.title.clone(),
                detail: r.detail.clone(),
                status: r.status.clone(),
                app_key: r.app_key.clone(),
                ..empty_item()
            },
        }
    }
}

/// An all-empty item, for the `..` field spread above.
fn empty_item() -> Item {
    Item {
        id: String::new(),
        kind: ItemKind::Agent,
        text: String::new(),
        detail: String::new(),
        output: String::new(),
        status: String::new(),
        app_key: String::new(),
        calls: Vec::new(),
    }
}

/// The kind of the last bubble, for the toolgroup collapse decision.
enum LastRow {
    Tool,
    Group,
    Other,
}

struct State {
    bubbles: Vec<Bubble>,
    /// The bubble the live text stream is currently accumulating into. `None`
    /// before any chunk, and after a tool call / clear / replace — the next
    /// chunk then always starts a fresh bubble (desktop's `m_currentIndex`).
    stream_idx: Option<usize>,
    stream_role: String,
    stream_msg_id: String,
    /// `tool_call_id` → the newest bubble index holding it, for O(1)
    /// `tool_update` (S-RC-4). Bubble indices are stable — bubbles are only
    /// appended, and a lone `Tool` converts in place to a `ToolGroup` at the
    /// same index — so this never goes stale short of a `clear`/`replace`,
    /// which rebuild it.
    tool_by_id: HashMap<String, usize>,
    /// The rows currently held were painted from the on-disk cache and a
    /// `session/load` replay is owed. The replay APPENDS, so the first real
    /// row must drop them wholesale rather than append after them — otherwise
    /// the transcript holds painted rows followed by replayed rows, which is
    /// NOT the server's order and can present an old message as the newest
    /// thing on screen. Enforced here, in the store, so every caller (cold
    /// start, live reuse, resync) gets it by construction.
    provisional: bool,
    /// Replay-MERGE mode: the rows held are a painted cache and the replay is a
    /// bounded TAIL that overlaps it. The first replayed row whose id is in the
    /// cache truncates it there (the replay re-supplies that row and everything
    /// after), so the older prefix survives and the tail is not duplicated.
    /// `merge_overlap` records whether that anchor was ever found: false means
    /// the tail starts past the cache (a gap), and the caller owes a full
    /// reload rather than leaving a hole in the transcript.
    merging: bool,
    merge_anchored: bool,
    merge_overlap: bool,
    /// The session these rows belong to, stamped on `Reset` ops.
    session_id: String,
    /// Whether the cache can still produce items older than `oldest_id` (the
    /// client's `load_older` cursor). Set by the core when it paints a window.
    has_older: bool,
    /// Monotonic counter for ids on live text rows the server sent without a
    /// `message_id` (see [`Bubble::Text::uid`]).
    next_uid: u64,
}

/// Hard cap on retained bubbles (S-RC-4). A transcript is inherently
/// unbounded over a run, but we evict the OLDEST bubbles past this watermark
/// (bulk, to a ¾ mark) so a pathological session cannot grow memory without
/// bound. Far above any realistic screenful.
const MAX_BUBBLES: usize = 2000;

/// Rebuild `tool_by_id` from scratch — used after a bulk eviction or rebuild,
/// when the surviving bubbles' absolute indices have shifted.
fn index_bubbles(map: &mut HashMap<String, usize>, bubbles: &[Bubble]) {
    for (i, b) in bubbles.iter().enumerate() {
        match b {
            Bubble::Tool(r) | Bubble::Chart(r) | Bubble::McpApp(r) => {
                map.insert(r.tool_call_id.clone(), i);
            }
            Bubble::ToolGroup(calls) => {
                for call in calls {
                    map.insert(call.tool_call_id.clone(), i);
                }
            }
            Bubble::Text { .. } => {}
        }
    }
}

impl State {
    /// Enforce the [`MAX_BUBBLES`] cap, evicting the oldest bubbles in bulk to
    /// a watermark so eviction stays rare (amortized O(1) per append). The id
    /// index is rebuilt and the stream anchor cleared (an evicted bubble may
    /// have been the one streaming into).
    fn trim(&mut self) {
        if self.bubbles.len() <= MAX_BUBBLES {
            return;
        }
        let keep = MAX_BUBBLES - MAX_BUBBLES / 4;
        let drop = self.bubbles.len() - keep;
        self.bubbles.drain(0..drop);
        self.tool_by_id.clear();
        index_bubbles(&mut self.tool_by_id, &self.bubbles);
        self.stream_idx = None;
    }

    /// A fresh `@n` id for a live text row the server did not id.
    fn new_uid(&mut self) -> String {
        self.next_uid += 1;
        format!("@{}", self.next_uid)
    }

    /// Forget everything and start clean, keeping the id index consistent.
    fn reset(&mut self) {
        self.bubbles.clear();
        self.tool_by_id.clear();
        self.stream_idx = None;
        self.stream_role.clear();
        self.stream_msg_id.clear();
        self.provisional = false;
        self.merging = false;
        self.merge_anchored = false;
        self.merge_overlap = false;
    }

    /// Drop a provisional cache paint the moment real server content arrives
    /// (see [`State::provisional`]). Idempotent: a no-op once superseded, so
    /// every row of a replay may call it. A merge paint is NOT dropped — its
    /// overlap is truncated by [`State::anchor_merge`] instead.
    ///
    /// Returns whether rows were dropped, so the caller can emit the `Reset` the
    /// item stream needs: a client following `on_item` must be told the painted
    /// items are gone, or the replay's Upserts would land beside them (the very
    /// duplication the provisional drop exists to prevent).
    fn supersede_provisional(&mut self) -> bool {
        if self.provisional && !self.merging {
            self.reset();
            return true;
        }
        false
    }

    /// Replay-merge anchor (see [`State::merging`]): on the first replayed row
    /// whose id is already in the painted cache, truncate the cache there so the
    /// replay re-supplies that row and everything after — the older prefix is
    /// kept, the overlapping suffix is replaced, nothing duplicates. Returns
    /// whether an anchor exists (`false` = the tail starts past the cache, a
    /// gap the caller must resolve with a full reload).
    fn anchor_merge(&mut self, id: &str) -> bool {
        if !self.merging {
            return true;
        }
        if self.merge_anchored {
            return self.merge_overlap;
        }
        if id.is_empty() {
            return self.merge_overlap;
        }
        self.merge_anchored = true;
        if let Some(k) = self.bubbles.iter().position(|b| bubble_owns(b, id)) {
            self.bubbles.truncate(k);
            self.tool_by_id.clear();
            index_bubbles(&mut self.tool_by_id, &self.bubbles);
            self.stream_idx = None;
            self.stream_role.clear();
            self.stream_msg_id.clear();
            self.merge_overlap = true;
        }
        self.merge_overlap
    }
}

/// Does this bubble belong to `id` (its message id for text, its tool_call_id
/// for the tool-ish bubbles)? Used to anchor a replay merge.
fn bubble_owns(b: &Bubble, id: &str) -> bool {
    match b {
        Bubble::Text { id: bid, .. } => !bid.is_empty() && bid == id,
        Bubble::Tool(r) | Bubble::Chart(r) | Bubble::McpApp(r) => r.tool_call_id == id,
        Bubble::ToolGroup(calls) => calls.iter().any(|r| r.tool_call_id == id),
    }
}

/// The stable RICH item id of a bubble (see [`Bubble::Text::uid`]).
fn bubble_uid(b: &Bubble) -> String {
    match b {
        Bubble::Text { uid, .. } => uid.clone(),
        Bubble::Tool(r) | Bubble::Chart(r) | Bubble::McpApp(r) => r.tool_call_id.clone(),
        Bubble::ToolGroup(calls) => {
            calls.first().map(|c| c.tool_call_id.clone()).unwrap_or_default()
        }
    }
}

/// A `ToolCall` back to the store's row shape.
fn call_to_row(c: &ToolCall) -> ToolRow {
    ToolRow {
        title: c.title.clone(),
        detail: c.detail.clone(),
        tool_call_id: c.id.clone(),
        output: c.output.clone(),
        status: c.status.clone(),
        app_key: String::new(),
        spec: String::new(),
    }
}

/// Rebuild a bubble from a rich item — the cache-load path. `None` for an empty
/// toolgroup (nothing to draw).
fn bubble_from_item(it: &Item) -> Option<Bubble> {
    match it.kind {
        ItemKind::User | ItemKind::Agent | ItemKind::Thought | ItemKind::Error => {
            let role = match it.kind {
                ItemKind::User => "user",
                ItemKind::Thought => "thought",
                ItemKind::Error => "error",
                _ => "agent",
            };
            // A core-assigned `@n` id has no server message_id to recover.
            let id = if it.id.starts_with('@') { String::new() } else { it.id.clone() };
            Some(Bubble::Text {
                id,
                uid: it.id.clone(),
                role: role.to_string(),
                text: it.text.clone(),
                thought: matches!(it.kind, ItemKind::Thought),
            })
        }
        ItemKind::Tool => Some(Bubble::Tool(ToolRow {
            title: it.text.clone(),
            detail: it.detail.clone(),
            tool_call_id: it.id.clone(),
            output: it.output.clone(),
            status: it.status.clone(),
            app_key: String::new(),
            spec: String::new(),
        })),
        ItemKind::ToolGroup => {
            if it.calls.is_empty() {
                None
            } else {
                Some(Bubble::ToolGroup(it.calls.iter().map(call_to_row).collect()))
            }
        }
        ItemKind::Chart => Some(Bubble::Chart(ToolRow {
            title: it.text.clone(),
            detail: String::new(),
            tool_call_id: it.id.clone(),
            output: String::new(),
            status: it.status.clone(),
            app_key: String::new(),
            // The item's `detail` is the spec for a chart.
            spec: it.detail.clone(),
        })),
        ItemKind::McpApp => Some(Bubble::McpApp(ToolRow {
            title: it.text.clone(),
            detail: it.detail.clone(),
            tool_call_id: it.id.clone(),
            output: String::new(),
            status: it.status.clone(),
            app_key: it.app_key.clone(),
            spec: String::new(),
        })),
    }
}

/// Accumulates streamed chunks into transcript bubbles and fans events out to
/// the `CoreListener`. Internally synchronised: the runtime thread streams
/// while `transcript()` may be read from any thread (the getters take `&self`).
pub struct TranscriptStore {
    listener: Box<dyn CoreListener>,
    state: Mutex<State>,
}

impl TranscriptStore {
    pub fn new(listener: Box<dyn CoreListener>) -> Self {
        Self {
            listener,
            state: Mutex::new(State {
                bubbles: Vec::new(),
                stream_idx: None,
                stream_role: String::new(),
                stream_msg_id: String::new(),
                tool_by_id: HashMap::new(),
                provisional: false,
                merging: false,
                merge_anchored: false,
                merge_overlap: false,
                session_id: String::new(),
                has_older: false,
                next_uid: 0,
            }),
        }
    }

    /// Append a streamed text chunk. `role` ∈ user | agent | thought.
    ///
    /// A new bubble starts when there is no current stream bubble, when the
    /// role changes, or when both the previous and the new chunk carry a
    /// non-empty message_id and they differ. Otherwise the chunk accumulates
    /// into the current bubble (desktop `appendChunk` fresh check).
    ///
    /// Real content supersedes a provisional cache paint first (see
    /// [`State::provisional`]): the replay APPENDS, so painted rows left in
    /// place would be followed by replayed rows — not the server's order, and
    /// an old message can end up reading as the newest.
    ///
    /// Emits the matching `on_stream` chunk and an `on_transcript`
    /// Append/Update.
    pub fn append_chunk(&self, role: &str, text: &str, message_id: Option<&str>, thought: bool) {
        let (stream_evt, transcript_evt, ops) = {
            let mut st = self.state.lock();
            st.anchor_merge(message_id.unwrap_or(""));
            let mut ops = Vec::new();
            if st.supersede_provisional() {
                ops.push(TranscriptOp::Reset { session_id: st.session_id.clone() });
            }
            let fresh = st.stream_idx.is_none()
                || st.stream_role != role
                || (message_id.is_some()
                    && !st.stream_msg_id.is_empty()
                    && message_id != Some(st.stream_msg_id.as_str()));

            let idx = if fresh {
                let uid = match message_id.filter(|m| !m.is_empty()) {
                    Some(m) => m.to_string(),
                    None => st.new_uid(),
                };
                st.bubbles.push(Bubble::Text {
                    id: message_id.unwrap_or("").to_string(),
                    uid,
                    role: role.to_string(),
                    text: text.to_string(),
                    thought,
                });
                let idx = st.bubbles.len() - 1;
                st.stream_idx = Some(idx);
                st.stream_role = role.to_string();
                st.stream_msg_id = message_id.unwrap_or("").to_string();
                idx
            } else {
                let idx = st.stream_idx.expect("non-fresh implies an open stream bubble");
                if let Bubble::Text { text: acc, thought: t, .. } = &mut st.bubbles[idx] {
                    acc.push_str(text);
                    *t = thought;
                }
                idx
            };

            let stream_evt = match role {
                "user" => StreamEvent::UserChunk {
                    text: text.to_string(),
                    message_id: message_id.unwrap_or("").to_string(),
                },
                "thought" => StreamEvent::ThoughtChunk { text: text.to_string() },
                _ => StreamEvent::AgentChunk {
                    text: text.to_string(),
                    message_id: message_id.unwrap_or("").to_string(),
                },
            };
            let transcript_evt = if fresh {
                TranscriptEvent::Append { message: st.bubbles[idx].project() }
            } else {
                TranscriptEvent::Update { message: st.bubbles[idx].project() }
            };
            // The item stream: a fresh bubble is an authoritative Upsert; an
            // accumulated chunk is the O(chunk) delta.
            let op = if fresh {
                TranscriptOp::Upsert { item: st.bubbles[idx].item() }
            } else {
                match &st.bubbles[idx] {
                    Bubble::Text { uid, .. } => {
                        TranscriptOp::AppendText { id: uid.clone(), chunk: text.to_string() }
                    }
                    _ => TranscriptOp::Upsert { item: st.bubbles[idx].item() },
                }
            };
            ops.push(op);
            st.trim();
            (stream_evt, transcript_evt, ops)
        };

        self.listener.on_stream(stream_evt);
        self.listener.on_transcript(transcript_evt);
        self.emit_ops(ops);
    }

    /// A tool call. Consecutive `Plain` calls collapse into one toolgroup
    /// bubble (first call appended, later calls update it); `Chart`/`McpApp`
    /// calls always append their own standalone bubble (desktop
    /// `onToolCall` / `onChartToolCall` / `onMcpAppToolCall`).
    ///
    /// A tool call breaks the text stream: the next chunk starts a fresh
    /// bubble. Emits `on_stream` ToolCall + an `on_transcript`
    /// Append/Update.
    pub fn tool_call(&self, title: &str, detail: &str, tool_call_id: &str, kind: ToolCallKind) {
        let (transcript_evt, ops) = {
            let mut st = self.state.lock();
            st.anchor_merge(tool_call_id);
            let mut ops = Vec::new();
            if st.supersede_provisional() {
                ops.push(TranscriptOp::Reset { session_id: st.session_id.clone() });
            }
            let mut row = ToolRow::new(title, detail, tool_call_id);
            // Retain the payload the flat Message projection drops, so the cache
            // round-trips a chart's spec and an app's key.
            match &kind {
                ToolCallKind::Chart { spec } => row.spec = spec.clone(),
                ToolCallKind::McpApp { app_key, .. } => row.app_key = app_key.clone(),
                ToolCallKind::Plain => {}
            }
            // Indexed by id (S-RC-4) so a later tool_update is O(1).
            let row_id = row.tool_call_id.clone();
            let plain = matches!(&kind, ToolCallKind::Plain);
            let last = st.bubbles.last().map(|b| match b {
                Bubble::Tool(_) => LastRow::Tool,
                Bubble::ToolGroup(_) => LastRow::Group,
                _ => LastRow::Other,
            });

            let (evt, idx) = if plain {
                match last {
                    Some(LastRow::Tool) => {
                        // Convert the lone first call into a group.
                        let idx = st.bubbles.len() - 1;
                        let first = match &st.bubbles[idx] {
                            Bubble::Tool(r) => r.clone(),
                            _ => unreachable!("last row is Tool"),
                        };
                        st.bubbles[idx] = Bubble::ToolGroup(vec![first, row]);
                        (TranscriptEvent::Update { message: st.bubbles[idx].project() }, idx)
                    }
                    Some(LastRow::Group) => {
                        let idx = st.bubbles.len() - 1;
                        if let Bubble::ToolGroup(calls) = &mut st.bubbles[idx] {
                            calls.push(row);
                        }
                        (TranscriptEvent::Update { message: st.bubbles[idx].project() }, idx)
                    }
                    _ => {
                        st.bubbles.push(Bubble::Tool(row));
                        let idx = st.bubbles.len() - 1;
                        (TranscriptEvent::Append { message: st.bubbles[idx].project() }, idx)
                    }
                }
            } else {
                let bubble = match &kind {
                    ToolCallKind::Chart { .. } => Bubble::Chart(row),
                    _ => Bubble::McpApp(row),
                };
                st.bubbles.push(bubble);
                let idx = st.bubbles.len() - 1;
                (TranscriptEvent::Append { message: st.bubbles[idx].project() }, idx)
            };
            st.tool_by_id.insert(row_id, idx);
            ops.push(TranscriptOp::Upsert { item: st.bubbles[idx].item() });
            st.trim();

            // Desktop: every tool call resets the streaming anchor so the next
            // chunk opens a fresh bubble.
            st.stream_idx = None;
            (evt, ops)
        };

        self.listener.on_stream(StreamEvent::ToolCall {
            title: title.to_string(),
            detail: detail.to_string(),
            tool_call_id: tool_call_id.to_string(),
            kind,
        });
        self.listener.on_transcript(transcript_evt);
        self.emit_ops(ops);
    }

    /// A tool lifecycle update. Searches from the newest bubble backwards for
    /// the tool call (tool / toolgroup / chart / mcpapp all carry their id).
    /// Status always updates; `output` follows the desktop rule: live output
    /// APPENDS to the accumulated output, the completion update (live=false)
    /// REPLACES it, and an empty output leaves it untouched. Chart/mcp-app
    /// bubbles take status only (desktop quirk).
    ///
    /// Emits `on_stream` ToolCallUpdate always; `on_transcript` Update only
    /// when a matching bubble was found.
    pub fn tool_update(&self, id: &str, status: &str, output: &str, live: bool) {
        let (transcript_evt, ops) = {
            let mut st = self.state.lock();
            st.anchor_merge(id);
            let mut ops = Vec::new();
            if st.supersede_provisional() {
                ops.push(TranscriptOp::Reset { session_id: st.session_id.clone() });
            }
            // O(1) lookup via the tool_by_id index (S-RC-4) — the previous
            // newest-backwards scan was O(n) per update, quadratic over a long
            // chat. Fall back to a scan only if the index is somehow stale.
            let idx = st.tool_by_id.get(id).copied().filter(|i| *i < st.bubbles.len());
            let mut updated = None;
            if let Some(idx) = idx {
                let found = match &mut st.bubbles[idx] {
                    Bubble::Tool(r) if r.tool_call_id == id => {
                        r.status = status.to_string();
                        apply_output(&mut r.output, output, live);
                        true
                    }
                    Bubble::Chart(r) | Bubble::McpApp(r) if r.tool_call_id == id => {
                        r.status = status.to_string();
                        true
                    }
                    Bubble::ToolGroup(calls) => {
                        let mut found = false;
                        for call in calls.iter_mut() {
                            if call.tool_call_id == id {
                                call.status = status.to_string();
                                apply_output(&mut call.output, output, live);
                                found = true;
                                break;
                            }
                        }
                        found
                    }
                    _ => false,
                };
                if found {
                    updated = Some(TranscriptEvent::Update { message: st.bubbles[idx].project() });
                    // A live append to a standalone tool is the O(chunk) delta;
                    // everything else (completion replace, group child, chart /
                    // app status) is an authoritative Upsert.
                    let standalone_live = live
                        && !output.is_empty()
                        && matches!(&st.bubbles[idx], Bubble::Tool(r) if r.tool_call_id == id);
                    ops.push(if standalone_live {
                        TranscriptOp::AppendOutput { id: id.to_string(), chunk: output.to_string() }
                    } else {
                        TranscriptOp::Upsert { item: st.bubbles[idx].item() }
                    });
                }
            }
            (updated, ops)
        };

        self.listener.on_stream(StreamEvent::ToolCallUpdate {
            id: id.to_string(),
            status: status.to_string(),
            output: output.to_string(),
            live,
        });
        if let Some(evt) = transcript_evt {
            self.listener.on_transcript(evt);
        }
        self.emit_ops(ops);
    }

    /// Promote a previously-plain tool call to an MCP App. The server does not
    /// know a call is an app at `tool_call` time — it hydrates `_meta.goose.mcpApp`
    /// onto the COMPLETING `tool_call_update` — so the core first announced a
    /// Plain call, and the app identity arrives late.
    ///
    /// Clients learn via a RE-ISSUED `on_stream` ToolCall carrying `kind=McpApp`
    /// for the same `tool_call_id` (desktop converts its chip in place; Android
    /// re-stashes and the transcript rebuild flips the bubble role), followed by
    /// the matching transcript Update — or Update+Append when the row is pulled
    /// out of a collapsed toolgroup. Re-promoting is a no-op (replays re-issue
    /// the update).
    pub fn tool_app(&self, id: &str, uri: &str, extension: &str) {
        // NOTE: no provisional drop here. The promoting update is always
        // preceded by the call / lifecycle update that supersedes a paint (and
        // emits the item `Reset`), so dropping here would clear the very row we
        // need to promote.
        let (title, detail, events, ops) = {
            let mut st = self.state.lock();
            st.anchor_merge(id);
            let Some(&idx) = st.tool_by_id.get(id) else { return };
            if idx >= st.bubbles.len() {
                return;
            }
            let row = match &st.bubbles[idx] {
                Bubble::Tool(r) if r.tool_call_id == id => Some(r.clone()),
                Bubble::ToolGroup(calls) => calls.iter().find(|c| c.tool_call_id == id).cloned(),
                _ => None, // already an app/chart bubble, or a stale index
            };
            let Some(mut row) = row else { return };
            // The app's resource key travels on the row so the cache keeps it.
            row.app_key = format!("{extension}|{uri}");
            let title = row.title.clone();
            let detail = row.detail.clone();

            if matches!(&st.bubbles[idx], Bubble::Tool(_)) {
                st.bubbles[idx] = Bubble::McpApp(row);
                let item = st.bubbles[idx].item();
                (
                    title,
                    detail,
                    vec![TranscriptEvent::Update { message: st.bubbles[idx].project() }],
                    vec![TranscriptOp::Upsert { item }],
                )
            } else {
                let Bubble::ToolGroup(calls) = &st.bubbles[idx] else { unreachable!() };
                let pos = calls.iter().position(|c| c.tool_call_id == id).unwrap();
                let mut calls = calls.clone();
                calls.remove(pos);
                let (events, ops) = if calls.is_empty() {
                    st.bubbles[idx] = Bubble::McpApp(row);
                    let item = st.bubbles[idx].item();
                    (
                        vec![TranscriptEvent::Update { message: st.bubbles[idx].project() }],
                        vec![TranscriptOp::Upsert { item }],
                    )
                } else {
                    st.bubbles[idx] = Bubble::ToolGroup(calls);
                    st.bubbles.insert(idx + 1, Bubble::McpApp(row));
                    for j in st.tool_by_id.values_mut() {
                        if *j > idx {
                            *j += 1;
                        }
                    }
                    st.tool_by_id.insert(id.to_string(), idx + 1);
                    let group_item = st.bubbles[idx].item();
                    let app_item = st.bubbles[idx + 1].item();
                    (
                        vec![
                            TranscriptEvent::Update { message: st.bubbles[idx].project() },
                            TranscriptEvent::Append { message: st.bubbles[idx + 1].project() },
                        ],
                        vec![
                            TranscriptOp::Upsert { item: group_item },
                            TranscriptOp::Upsert { item: app_item },
                        ],
                    )
                };
                (title, detail, events, ops)
            }
        };

        self.listener.on_stream(StreamEvent::ToolCall {
            title: title.clone(),
            detail: detail.clone(),
            tool_call_id: id.to_string(),
            kind: ToolCallKind::McpApp {
                app_key: format!("{extension}|{uri}"),
                uri: uri.to_string(),
                extension: extension.to_string(),
                input: detail,
            },
        });
        for evt in events {
            self.listener.on_transcript(evt);
        }
        self.emit_ops(ops);
    }

    /// Usage accounting (on_stream only — no transcript change).
    pub fn usage(&self, used: i64, size: i64, cost: f64, currency: &str) {
        self.listener.on_stream(StreamEvent::Usage {
            used,
            size,
            cost,
            currency: currency.to_string(),
        });
    }

    /// A turn finished (on_stream only — no transcript change; the desktop's
    /// markdown finalisation has no core equivalent).
    pub fn run_ended(&self, stop_reason: &str) {
        self.listener.on_stream(StreamEvent::RunEnded { stop_reason: stop_reason.to_string() });
    }

    /// Wipe the transcript and reset the stream state; emits `on_transcript`
    /// Clear and an item `Reset` + empty `Window`.
    pub fn clear(&self) {
        let session_id = {
            let mut st = self.state.lock();
            st.reset();
            st.session_id.clone()
        };
        self.listener.on_transcript(TranscriptEvent::Clear);
        self.listener.on_item(TranscriptOp::Reset { session_id });
        self.listener.on_item(TranscriptOp::Window {
            oldest_id: String::new(),
            has_older: false,
        });
    }

    /// The session the held rows belong to (stamped on `Reset` ops).
    pub fn set_session(&self, session_id: &str) {
        self.state.lock().session_id = session_id.to_string();
    }

    /// The id of the oldest held item (empty when the store is empty).
    pub fn oldest_id(&self) -> String {
        let st = self.state.lock();
        st.bubbles.first().map(bubble_uid).unwrap_or_default()
    }

    /// The id of the newest held item (empty when the store is empty).
    pub fn newest_id(&self) -> String {
        let st = self.state.lock();
        st.bubbles.last().map(bubble_uid).unwrap_or_default()
    }

    /// The pagination cursor: the oldest item plus whether `load_older` can
    /// still produce older items.
    pub fn window(&self) -> TranscriptWindow {
        let st = self.state.lock();
        TranscriptWindow {
            oldest_id: st.bubbles.first().map(bubble_uid).unwrap_or_default(),
            newest_id: st.bubbles.last().map(bubble_uid).unwrap_or_default(),
            has_older: st.has_older,
        }
    }

    /// Set the `has_older` cursor without repainting, and announce it.
    pub fn set_has_older(&self, has_older: bool) {
        let oldest = {
            let mut st = self.state.lock();
            st.has_older = has_older;
            st.bubbles.first().map(bubble_uid).unwrap_or_default()
        };
        self.listener.on_item(TranscriptOp::Window { oldest_id: oldest, has_older });
    }

    /// The rich item snapshot (docs/TRANSCRIPT_MODEL.md), in transcript order.
    pub fn rich_transcript(&self) -> Vec<Item> {
        let st = self.state.lock();
        st.bubbles.iter().map(Bubble::item).collect()
    }

    /// One item by id, if held (maps the id index for O(1) tool lookup, then
    /// scans text rows).
    pub fn item(&self, id: &str) -> Option<Item> {
        let st = self.state.lock();
        if let Some(&idx) = st.tool_by_id.get(id) {
            if let Some(b) = st.bubbles.get(idx) {
                return Some(b.item());
            }
        }
        st.bubbles
            .iter()
            .find(|b| matches!(b, Bubble::Text { uid, .. } if uid == id))
            .map(Bubble::item)
    }

    /// Drop a provisional cache paint (see [`State::provisional`]) — the spine
    /// calls this as the first row of a `session/load` replay arrives, and once
    /// more when a load completes without producing any, meaning the server's
    /// copy is empty. A no-op once real content superseded the paint, so it is
    /// safe to call per row.
    pub fn supersede_provisional(&self) {
        let (dropped, session_id) = {
            let mut st = self.state.lock();
            let dropped = st.supersede_provisional();
            (dropped, st.session_id.clone())
        };
        if dropped {
            // Tell BOTH channels the painted rows are gone: a client following
            // `on_item` must clear before the replay's Upserts arrive, and a
            // client following the legacy channel must drop its model (a Clear
            // means "rebuild from the store", which is now empty) so the
            // replay's Appends do not land beside the paint.
            self.listener.on_transcript(TranscriptEvent::Clear);
            self.listener.on_item(TranscriptOp::Reset { session_id });
        }
    }

    /// Emit a batch of item ops, mirroring a leading `Reset` onto the legacy
    /// channel as a `Clear` (see [`Self::supersede_provisional`]).
    fn emit_ops(&self, ops: Vec<TranscriptOp>) {
        for op in ops {
            if matches!(op, TranscriptOp::Reset { .. }) {
                self.listener.on_transcript(TranscriptEvent::Clear);
            }
            self.listener.on_item(op);
        }
    }

    /// Mark the rows currently held as provisional, without repainting: the
    /// rows on screen are the right ones to SHOW but must not survive a replay
    /// that is about to rebuild the transcript (a stale resume during
    /// reconnect, where the store still holds the pre-drop transcript).
    pub fn mark_provisional(&self) {
        self.state.lock().provisional = true;
    }

    /// Snapshot of the accumulated transcript (flat `Message` projection).
    pub fn transcript(&self) -> Vec<Message> {
        let st = self.state.lock();
        st.bubbles.iter().map(Bubble::project).collect()
    }

    /// Rebuild the transcript from a flat snapshot (replay / cache load path):
    /// clear + rebuild, emitting exactly one `on_transcript` Clear. The
    /// rebuild is faithful — projecting the rebuilt bubbles reproduces the
    /// input — and the stream state resets so the next chunk starts fresh.
    ///
    /// No-op when the snapshot already matches the current bubbles: a cold
    /// start paints the cached transcript via `load_cached_transcript`, then
    /// `open_session`'s fresh path replaces the same content again — the
    /// second rebuild emptied the list and re-painted all rows (flicker +
    /// recomposition storm). The Clear is skipped, so the reading position
    /// and the list stay untouched.
    pub fn replace(&self, messages: Vec<Message>) {
        self.replace_inner(messages, false);
    }

    /// Paint a cached transcript as PROVISIONAL: the rows render instantly and
    /// are dropped wholesale by the first real server row (see
    /// [`State::provisional`]). Use this — not [`Self::replace`] — whenever a
    /// `session/load` replay is owed, which is every cache paint that is not
    /// known to be current.
    pub fn replace_provisional(&self, messages: Vec<Message>) {
        self.replace_inner(messages, true);
    }

    /// Paint a cached transcript and arm replay-MERGE: the `session/load` tail
    /// that follows overlaps it, and the first replayed row truncates the cache
    /// there (see [`State::merging`]) rather than dropping it. Use this for a
    /// stale-but-present cache so the open is a bounded tail instead of a full
    /// replay — the older history stays on screen from the cache.
    pub fn replace_for_merge(&self, messages: Vec<Message>) {
        self.replace_inner(messages, false);
        let mut st = self.state.lock();
        st.merging = true;
        st.merge_anchored = false;
        st.merge_overlap = false;
        st.provisional = false;
    }

    /// The rich counterpart of [`Self::replace_for_merge`].
    pub fn replace_rich_for_merge(&self, items: Vec<Item>) {
        self.replace_rich(items, false, false);
        let mut st = self.state.lock();
        st.merging = true;
        st.merge_anchored = false;
        st.merge_overlap = false;
        st.provisional = false;
    }

    /// End replay-merge once the replay has completed. Returns true when the
    /// tail never overlapped the cache (a gap): the caller must clear and do a
    /// full reload rather than leave a hole. Idempotent.
    ///
    /// On a successful merge the store now holds the painted PREFIX plus the
    /// replayed tail, but clients have been appending the tail on top of their
    /// full paint (whose stale suffix the anchor dropped). Emit one `Clear` so
    /// they rebuild from the store: the stale suffix disappears and the tail is
    /// not duplicated. One rebuild at the END of the replay — never a per-row
    /// re-render, and never a full replay on the wire.
    pub fn end_merge(&self) -> bool {
        let (gap, merged_session) = {
            let mut st = self.state.lock();
            let gap = st.merging && !st.merge_overlap;
            let merged = st.merging && st.merge_overlap;
            st.merging = false;
            st.merge_anchored = false;
            (
                gap,
                if merged { st.session_id.clone() } else { String::new() },
            )
        };
        if !merged_session.is_empty() {
            self.listener.on_transcript(TranscriptEvent::Clear);
            self.listener.on_item(TranscriptOp::Reset { session_id: merged_session });
        }
        gap
    }

    /// Rebuild the transcript from a rich snapshot (the rich cache paint):
    /// clear + rebuild, emitting Clear + `Reset` + one `Upsert` per item + a
    /// `Window`. Full fidelity — charts, MCP apps and toolgroups survive.
    pub fn replace_rich(&self, items: Vec<Item>, provisional: bool, has_older: bool) {
        {
            // Identical paint (the cold-start paint followed by the open's own):
            // skip the Clear, or the list empties and re-paints on every open
            // (flicker + a scroll jump). Adopt this call's mode/cursor.
            let mut st = self.state.lock();
            let same = {
                let cur = st.bubbles.iter().map(Bubble::item);
                let mut n = 0;
                let mut matches = true;
                for (a, b) in cur.zip(items.iter()) {
                    if a != *b {
                        matches = false;
                        break;
                    }
                    n += 1;
                }
                matches && n == items.len()
            };
            if same {
                st.provisional = provisional;
                st.has_older = has_older;
                return;
            }
        }
        let (session_id, oldest) = {
            let mut st = self.state.lock();
            st.reset();
            for it in &items {
                if let Some(b) = bubble_from_item(it) {
                    st.bubbles.push(b);
                }
            }
            let st = &mut *st;
            st.tool_by_id.clear();
            index_bubbles(&mut st.tool_by_id, &st.bubbles);
            st.provisional = provisional;
            st.has_older = has_older;
            st.trim();
            (st.session_id.clone(), st.bubbles.first().map(bubble_uid).unwrap_or_default())
        };
        self.listener.on_transcript(TranscriptEvent::Clear);
        self.listener.on_item(TranscriptOp::Reset { session_id });
        for it in &items {
            self.listener.on_item(TranscriptOp::Upsert { item: it.clone() });
        }
        self.listener.on_item(TranscriptOp::Window { oldest_id: oldest, has_older });
    }

    /// Prepend older items (the `load_older` path), oldest-first: each is
    /// Upserted and a refreshed `Window` announces the new cursor. The flat
    /// `on_transcript` channel is deliberately NOT used — an Append would put
    /// them at the wrong end.
    pub fn prepend_items(&self, items: Vec<Item>, has_older: bool) {
        let (session_id, oldest) = {
            let mut st = self.state.lock();
            let mut rebuilt: Vec<Bubble> = Vec::with_capacity(items.len());
            for it in &items {
                if let Some(b) = bubble_from_item(it) {
                    rebuilt.push(b);
                }
            }
            for b in rebuilt.into_iter().rev() {
                st.bubbles.insert(0, b);
            }
            let st = &mut *st;
            st.tool_by_id.clear();
            index_bubbles(&mut st.tool_by_id, &st.bubbles);
            st.has_older = has_older;
            st.trim();
            (st.session_id.clone(), st.bubbles.first().map(bubble_uid).unwrap_or_default())
        };
        // Tell the client to drop nothing (no Reset) but to prepend.
        let _ = session_id;
        for it in &items {
            self.listener.on_item(TranscriptOp::Upsert { item: it.clone() });
        }
        self.listener.on_item(TranscriptOp::Window { oldest_id: oldest, has_older });
    }

    fn replace_inner(&self, messages: Vec<Message>, provisional: bool) {
        let (session_id, oldest, items) = {
            let mut st = self.state.lock();
            let same = {
                let cur = st.bubbles.iter().map(Bubble::project);
                let mut n = 0;
                let mut matches = true;
                for (a, b) in cur.zip(messages.iter()) {
                    if a != *b {
                        matches = false;
                        break;
                    }
                    n += 1;
                }
                matches && n == messages.len()
            };
            if same {
                // Identical paint (the Kotlin cold-start paint followed by the
                // core's own): nothing to repaint, but still adopt this call's
                // provisional mode — a stale cache must stay armed, and a
                // fresh one must stop being droppable.
                st.provisional = provisional;
                return;
            }
            st.reset();
            for m in messages {
                if m.role == "tool" {
                    let idx = st.bubbles.len();
                    st.tool_by_id.insert(m.id.clone(), idx);
                    st.bubbles.push(Bubble::Tool(ToolRow {
                        title: m.content.clone(),
                        detail: String::new(),
                        tool_call_id: m.id.clone(),
                        output: String::new(),
                        status: String::new(),
                        app_key: String::new(),
                        spec: String::new(),
                    }));
                } else {
                    let uid = if m.id.is_empty() { st.new_uid() } else { m.id.clone() };
                    st.bubbles.push(Bubble::Text {
                        id: m.id.clone(),
                        uid,
                        role: m.role.clone(),
                        text: m.content.clone(),
                        thought: m.role == "thought",
                    });
                }
            }
            st.provisional = provisional;
            let items: Vec<Item> = st.bubbles.iter().map(Bubble::item).collect();
            let oldest = st.bubbles.first().map(bubble_uid).unwrap_or_default();
            (st.session_id.clone(), oldest, items)
        };
        self.listener.on_transcript(TranscriptEvent::Clear);
        self.listener.on_item(TranscriptOp::Reset { session_id });
        for it in &items {
            self.listener.on_item(TranscriptOp::Upsert { item: it.clone() });
        }
        self.listener.on_item(TranscriptOp::Window {
            oldest_id: oldest,
            has_older: false,
        });
    }

    /// Test-only view of the rich rows (outputs/statuses are not visible
    /// through the flat `Message` projection).
    #[cfg(test)]
    fn rows_for_test(&self) -> Vec<Bubble> {
        self.state.lock().bubbles.clone()
    }
}

/// The desktop's output rule: an empty output leaves the accumulator alone;
/// live output appends, the completion update replaces.
fn apply_output(acc: &mut String, output: &str, live: bool) {
    if !output.is_empty() {
        if live {
            acc.push_str(output);
        } else {
            *acc = output.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use parking_lot::Mutex;

    use crate::{
        ConfigOption, ConnectionStatus, CoreListener, Item, ItemKind, Message,
        PermissionRequest, ProjectSummary, SessionSummary, StreamEvent, ToolCall, ToolCallKind,
        TranscriptEvent,
    };

    use super::{Bubble, TranscriptStore};

    /// Records the events the store fans out, as plain strings so assertions
    /// stay readable. Events are consumed by value (they are not `Clone`).
    struct TestListener {
        transcript_events: Arc<Mutex<Vec<String>>>,
        stream_events: Arc<Mutex<Vec<String>>>,
        item_events: Arc<Mutex<Vec<String>>>,
    }

    /// A readable one-line rendering of an item op, so assertions stay terse.
    fn op_desc(op: &crate::TranscriptOp) -> String {
        use crate::TranscriptOp as Op;
        match op {
            Op::Reset { .. } => "reset".to_string(),
            Op::Upsert { item } => format!(
                "upsert:{}:{:?}:{}:{}",
                item.id, item.kind, item.text, item.status
            ),
            Op::AppendText { id, chunk } => format!("text:{id}:{chunk}"),
            Op::AppendOutput { id, chunk } => format!("output:{id}:{chunk}"),
            Op::Remove { id } => format!("remove:{id}"),
            Op::Window { oldest_id, has_older } => format!("window:{oldest_id}:{has_older}"),
        }
    }

    fn listener() -> (
        Box<dyn CoreListener>,
        Arc<Mutex<Vec<String>>>,
        Arc<Mutex<Vec<String>>>,
        Arc<Mutex<Vec<String>>>,
    ) {
        let t = Arc::new(Mutex::new(Vec::new()));
        let s = Arc::new(Mutex::new(Vec::new()));
        let i = Arc::new(Mutex::new(Vec::new()));
        let l = TestListener {
            transcript_events: t.clone(),
            stream_events: s.clone(),
            item_events: i.clone(),
        };
        (Box::new(l), t, s, i)
    }

    fn kind_desc(kind: &ToolCallKind) -> String {
        match kind {
            ToolCallKind::Plain => "plain".to_string(),
            ToolCallKind::Chart { spec } => format!("chart:{spec}"),
            ToolCallKind::McpApp { .. } => "mcpapp".to_string(),
        }
    }

    impl CoreListener for TestListener {
        fn on_status(&self, _status: ConnectionStatus) {}
        fn on_sessions(&self, _sessions: Vec<SessionSummary>) {}
        fn on_transcript(&self, event: TranscriptEvent) {
            let s = match event {
                TranscriptEvent::Append { message } => {
                    format!("append:{}:{}:{}", message.role, message.id, message.content)
                }
                TranscriptEvent::Update { message } => {
                    format!("update:{}:{}:{}", message.role, message.id, message.content)
                }
                TranscriptEvent::Clear => "clear".to_string(),
            };
            self.transcript_events.lock().push(s);
        }
        fn on_stream(&self, event: StreamEvent) {
            let s = match event {
                StreamEvent::AgentChunk { text, message_id } => {
                    format!("agent:{message_id}:{text}")
                }
                StreamEvent::UserChunk { text, message_id } => {
                    format!("user:{message_id}:{text}")
                }
                StreamEvent::ThoughtChunk { text } => format!("thought:{text}"),
                StreamEvent::ToolCall { title, detail, tool_call_id, kind } => {
                    format!("toolcall:{title}:{detail}:{tool_call_id}:{}", kind_desc(&kind))
                }
                StreamEvent::ToolCallUpdate { id, status, output, live } => {
                    format!("toolupdate:{id}:{status}:{output}:{live}")
                }
                StreamEvent::Usage { used, size, cost, currency } => {
                    format!("usage:{used}:{size}:{cost}:{currency}")
                }
                StreamEvent::RunEnded { stop_reason } => format!("runended:{stop_reason}"),
            };
            self.stream_events.lock().push(s);
        }
        fn on_item(&self, op: crate::TranscriptOp) {
            self.item_events.lock().push(op_desc(&op));
        }
        fn on_config(&self, _options: Vec<ConfigOption>) {}
        fn on_permission_request(&self, _request: PermissionRequest) {}
        fn on_session_touched(&self, _session_id: String, _title: String, _updated_at: String) {}
        fn on_projects(&self, _projects: Vec<ProjectSummary>) {}
        fn on_roam_peer_status(&self, _label: String, _status: String) {}
        fn on_roam_sessions(&self, _label: String, _sessions: Vec<SessionSummary>) {}
        fn on_peer_new_session(&self, _label: String, _session_id: String) {}
        fn on_active_run(&self, _session_id: String, _run_id: String) {}
        fn on_commands(&self, _commands: Vec<String>) {}
    }

    fn msgs(store: &TranscriptStore) -> Vec<(String, String, String)> {
        store
            .transcript()
            .iter()
            .map(|m| (m.role.clone(), m.id.clone(), m.content.clone()))
            .collect()
    }

    #[test]
    fn chunk_accumulation_across_ids() {
        let (l, t_evts, s_evts, _i_evts) = listener();
        let store = TranscriptStore::new(l);

        store.append_chunk("user", "Hello", Some("m1"), false);
        store.append_chunk("user", " world", Some("m1"), false); // same role+id → accumulate
        store.append_chunk("user", "!", Some("m2"), false); // new id → new bubble
        store.append_chunk("agent", "Sure", Some("m2"), false); // role change → new bubble
        store.append_chunk("agent", " thing", Some("m2"), false); // accumulate
        store.append_chunk("thought", "hmm", None, true);
        store.append_chunk("thought", " more", None, true);

        assert_eq!(
            msgs(&store),
            vec![
                ("user".to_string(), "m1".to_string(), "Hello world".to_string()),
                ("user".to_string(), "m2".to_string(), "!".to_string()),
                ("agent".to_string(), "m2".to_string(), "Sure thing".to_string()),
                ("thought".to_string(), String::new(), "hmm more".to_string()),
            ]
        );

        // Fresh bubbles Append; same-bubble accumulation Updates.
        assert_eq!(
            &*t_evts.lock(),
            &[
                "append:user:m1:Hello",
                "update:user:m1:Hello world",
                "append:user:m2:!",
                "append:agent:m2:Sure",
                "update:agent:m2:Sure thing",
                "append:thought::hmm",
                "update:thought::hmm more",
            ]
        );
        // Every chunk is echoed on the stream.
        assert_eq!(
            &*s_evts.lock(),
            &[
                "user:m1:Hello",
                "user:m1: world",
                "user:m2:!",
                "agent:m2:Sure",
                "agent:m2: thing",
                "thought:hmm",
                "thought: more",
            ]
        );
    }

    #[test]
    fn toolgroup_collapse() {
        let (l, t_evts, s_evts, _i_evts) = listener();
        let store = TranscriptStore::new(l);

        store.tool_call("Bash", "ls", "t1", ToolCallKind::Plain);
        store.tool_call("Bash", "pwd", "t2", ToolCallKind::Plain);
        store.tool_call("Read", "file.txt", "t3", ToolCallKind::Plain);

        // The three consecutive calls collapse into ONE tool bubble anchored
        // on the first call.
        assert_eq!(
            msgs(&store),
            vec![("tool".to_string(), "t1".to_string(), "Bash".to_string())]
        );
        let rows = store.rows_for_test();
        match &rows[0] {
            Bubble::ToolGroup(calls) => assert_eq!(calls.len(), 3),
            other => panic!("expected a toolgroup, got {other:?}"),
        }

        // First call appends, each later call updates the same bubble.
        assert_eq!(
            &*t_evts.lock(),
            &[
                "append:tool:t1:Bash",
                "update:tool:t1:Bash",
                "update:tool:t1:Bash",
            ]
        );
        // Every call is echoed on the stream.
        assert_eq!(
            &*s_evts.lock(),
            &[
                "toolcall:Bash:ls:t1:plain",
                "toolcall:Bash:pwd:t2:plain",
                "toolcall:Read:file.txt:t3:plain",
            ]
        );

        // A tool call breaks the text stream: the next chunk opens a fresh
        // bubble even for a role+id already seen before the call.
        store.append_chunk("agent", "done", Some("m1"), false);
        assert_eq!(
            msgs(&store),
            vec![
                ("tool".to_string(), "t1".to_string(), "Bash".to_string()),
                ("agent".to_string(), "m1".to_string(), "done".to_string()),
            ]
        );
    }

    #[test]
    fn tool_app_promotes_late_hydrated_calls() {
        let (l, t_evts, s_evts, _i_evts) = listener();
        let store = TranscriptStore::new(l);

        // Standalone plain call promoted when the completing update carries mcpApp.
        store.tool_call("Dashboard", "{}", "d1", ToolCallKind::Plain);
        store.tool_app("d1", "ui://x/dashboard", "monitorext");
        assert!(s_evts.lock().iter().any(|e| e == "toolcall:Dashboard:{}:d1:mcpapp"),
                "re-issued stream ToolCall must carry McpApp kind: {:?}", s_evts.lock());
        assert!(t_evts.lock().iter().any(|e| e.starts_with("update:tool:d1:")));
        match &store.rows_for_test()[0] {
            Bubble::McpApp(r) => assert_eq!(r.tool_call_id, "d1"),
            other => panic!("expected McpApp bubble, got {other:?}"),
        }

        // Idempotent: replays re-issue the update; the stream must not re-announce.
        let n = s_evts.lock().len();
        store.tool_app("d1", "ui://x/dashboard", "monitorext");
        assert_eq!(n, s_evts.lock().len(), "re-promote must be a no-op");

        // Group extraction: a promoted call leaves the group and gets its own
        // bubble (Append), and later status updates still find it.
        let (l2, t2, s2, _i2) = listener();
        let store2 = TranscriptStore::new(l2);
        store2.tool_call("A", "x", "g1", ToolCallKind::Plain);
        store2.tool_call("B", "y", "g2", ToolCallKind::Plain);
        store2.tool_app("g2", "ui://b", "extb");
        assert!(t2.lock().iter().any(|e| e.starts_with("append:tool:g2:B")));
        match &store2.rows_for_test()[0] {
            Bubble::ToolGroup(calls) => assert_eq!(calls.len(), 1),
            other => panic!("group should shrink to one call, got {other:?}"),
        }
        s2.lock().clear(); t2.lock().clear();
        store2.tool_update("g2", "completed", "out", false);
        assert!(s2.lock().iter().any(|e| e.starts_with("toolupdate:g2:completed:")));
    }

    #[test]
    fn chart_and_mcpapp_calls_never_collapse() {
        let (l, _t, _s, _i_evts) = listener();
        let store = TranscriptStore::new(l);

        store.tool_call("Chart", "", "c1", ToolCallKind::Chart { spec: "{}".into() });
        store.tool_call("Bash", "ls", "t4", ToolCallKind::Plain); // chart in between → no merge
        store.tool_call("App", "input", "a1", ToolCallKind::McpApp {
            app_key: "k".into(),
            uri: "u".into(),
            extension: "x".into(),
            input: "i".into(),
        });

        assert_eq!(
            msgs(&store),
            vec![
                ("tool".to_string(), "c1".to_string(), "Chart".to_string()),
                ("tool".to_string(), "t4".to_string(), "Bash".to_string()),
                ("tool".to_string(), "a1".to_string(), "App".to_string()),
            ]
        );
    }

    #[test]
    fn tool_output_live_appends_completion_replaces() {
        let (l, t_evts, s_evts, _i_evts) = listener();
        let store = TranscriptStore::new(l);

        store.tool_call("Bash", "echo hi", "t1", ToolCallKind::Plain);
        store.tool_update("t1", "in_progress", "line1\n", true);
        store.tool_update("t1", "in_progress", "line2\n", true);
        // Live output accumulates.
        let rows = store.rows_for_test();
        let out = match &rows[0] {
            Bubble::Tool(r) => &r.output,
            other => panic!("expected a tool bubble, got {other:?}"),
        };
        assert_eq!(out, "line1\nline2\n");

        // The completion update REPLACES the accumulated output.
        store.tool_update("t1", "completed", "final output", false);
        let rows = store.rows_for_test();
        match &rows[0] {
            Bubble::Tool(r) => {
                assert_eq!(r.output, "final output");
                assert_eq!(r.status, "completed");
            }
            other => panic!("expected a tool bubble, got {other:?}"),
        }

        // An empty completion output leaves the accumulator untouched.
        store.tool_update("t1", "completed", "", false);
        let rows = store.rows_for_test();
        match &rows[0] {
            Bubble::Tool(r) => assert_eq!(r.output, "final output"),
            other => panic!("expected a tool bubble, got {other:?}"),
        }

        // Updates inside a collapsed group behave the same way.
        store.tool_call("Bash", "ls", "t2", ToolCallKind::Plain);
        store.tool_update("t2", "in_progress", "a", true);
        store.tool_update("t2", "completed", "done", false);
        let rows = store.rows_for_test();
        match &rows[0] {
            Bubble::ToolGroup(calls) => {
                assert_eq!(calls[1].output, "done");
                assert_eq!(calls[1].status, "completed");
            }
            other => panic!("expected a toolgroup, got {other:?}"),
        }

        assert_eq!(
            &*s_evts.lock(),
            &[
                "toolcall:Bash:echo hi:t1:plain",
                "toolupdate:t1:in_progress:line1\n:true",
                "toolupdate:t1:in_progress:line2\n:true",
                "toolupdate:t1:completed:final output:false",
                "toolupdate:t1:completed::false",
                "toolcall:Bash:ls:t2:plain",
                "toolupdate:t2:in_progress:a:true",
                "toolupdate:t2:completed:done:false",
            ]
        );
        // Every successful tool update fires a transcript Update.
        assert!(t_evts
            .lock()
            .iter()
            .filter(|e| e.starts_with("update:tool:"))
            .count()
            >= 5);
    }

    #[test]
    fn tool_update_status_only_for_chart_bubbles() {
        let (l, _t, _s, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.tool_call("Chart", "", "c1", ToolCallKind::Chart { spec: "{}".into() });
        store.tool_update("c1", "failed", "ignored output", false);
        let rows = store.rows_for_test();
        match &rows[0] {
            Bubble::Chart(r) => {
                assert_eq!(r.status, "failed");
                // Desktop quirk: chart/mcp-app bubbles never carry output.
                assert_eq!(r.output, "");
            }
            other => panic!("expected a chart bubble, got {other:?}"),
        }
    }

    #[test]
    fn tool_update_unknown_id_emits_stream_only() {
        let (l, t_evts, s_evts, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.tool_update("ghost", "completed", "out", false);
        assert!(t_evts.lock().is_empty());
        assert_eq!(&*s_evts.lock(), &["toolupdate:ghost:completed:out:false"]);
    }

    #[test]
    fn usage_and_run_ended_are_stream_only() {
        let (l, t_evts, s_evts, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("agent", "hi", Some("m1"), false);
        store.usage(123, 456, 0.0012, "USD");
        store.run_ended("end_turn");
        assert!(t_evts.lock().iter().all(|e| e.starts_with("append:")));
        assert_eq!(
            &*s_evts.lock(),
            &[
                "agent:m1:hi",
                "usage:123:456:0.0012:USD",
                "runended:end_turn",
            ]
        );
    }

    #[test]
    fn clear_wipes_transcript_and_resets_stream() {
        let (l, t_evts, _s, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("agent", "one", Some("m1"), false);
        store.tool_call("Bash", "ls", "t1", ToolCallKind::Plain);
        assert_eq!(store.transcript().len(), 2);

        store.clear();
        assert!(store.transcript().is_empty());
        assert_eq!(
            &*t_evts.lock(),
            &["append:agent:m1:one", "append:tool:t1:Bash", "clear"]
        );

        // Stream state reset: the same id now starts a fresh bubble.
        store.append_chunk("agent", "again", Some("m1"), false);
        assert_eq!(
            msgs(&store),
            vec![("agent".to_string(), "m1".to_string(), "again".to_string())]
        );
    }

    #[test]
    fn replace_rebuilds_and_emits_clear_once() {
        let (l, t_evts, _s, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("user", "old", Some("m1"), false);

        store.replace(vec![
            Message { id: "m1".into(), role: "user".into(), content: "hi".into() , output: String::new() },
            Message { id: "m2".into(), role: "agent".into(), content: "hello".into() , output: String::new() },
            Message { id: "t1".into(), role: "tool".into(), content: "Bash".into() , output: String::new() },
            Message { id: String::new(), role: "thought".into(), content: "hmm".into() , output: String::new() },
        ]);

        // The rebuild is faithful: the projection reproduces the input.
        assert_eq!(
            msgs(&store),
            vec![
                ("user".to_string(), "m1".to_string(), "hi".to_string()),
                ("agent".to_string(), "m2".to_string(), "hello".to_string()),
                ("tool".to_string(), "t1".to_string(), "Bash".to_string()),
                ("thought".to_string(), String::new(), "hmm".to_string()),
            ]
        );
        // Exactly one Clear, no per-message appends.
        assert_eq!(&*t_evts.lock(), &["append:user:m1:old", "clear"]);

        // Stream state reset: a chunk for an already-loaded id starts fresh.
        store.append_chunk("user", "new", Some("m1"), false);
        let t = store.transcript();
        assert_eq!(t.len(), 5);
        assert_eq!(t[4].content, "new");
    }

    #[test]
    fn replace_with_identical_content_is_a_noop() {
        let (l, t_evts, _s, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        let snapshot = vec![
            Message { id: "m1".into(), role: "user".into(), content: "hi".into() , output: String::new() },
            Message { id: "m2".into(), role: "agent".into(), content: "hello".into() , output: String::new() },
        ];
        store.replace(snapshot.clone());
        assert_eq!(&*t_evts.lock(), &["clear"]);

        // A cold start paints the cache twice (load_cached_transcript, then
        // open_session's fresh path): the second replace of the SAME content
        // must not emit another Clear — that emptied the list and re-painted
        // every row (flicker + recomposition storm).
        store.replace(snapshot.clone());
        assert_eq!(&*t_evts.lock(), &["clear"], "identical replace must be a no-op");

        // A genuinely different snapshot still rebuilds.
        store.replace(vec![
            Message { id: "m1".into(), role: "user".into(), content: "hi".into() , output: String::new() },
            Message { id: "m2".into(), role: "agent".into(), content: "changed".into() , output: String::new() },
        ]);
        assert_eq!(&*t_evts.lock(), &["clear", "clear"]);
    }

    #[test]
    fn provisional_paint_is_superseded_by_the_first_real_row() {
        // A stale cache is painted provisionally, and the replay APPENDS.
        // Without the drop the transcript holds painted rows followed by
        // replayed rows — not the server's order — which is how a 19-hour-old
        // message came to read as the newest thing on screen.
        let (l, _t, _s, i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.set_session("s1");
        store.replace_provisional(vec![
            Message { id: "old2".into(), role: "user".into(), content: "old prompt".into(), output: String::new() },
            Message { id: "old1".into(), role: "agent".into(), content: "19h-old reply".into(), output: String::new() },
        ]);
        assert_eq!(msgs(&store).len(), 2, "painted rows render instantly");
        i_evts.lock().clear();

        // First replayed row: painted rows go, the replay's order wins.
        store.append_chunk("user", "replayed", Some("m1"), false);
        assert_eq!(
            msgs(&store),
            vec![("user".to_string(), "m1".to_string(), "replayed".to_string())],
            "a replayed row must never land after painted rows"
        );
        // The item stream is told the paint is gone BEFORE the replay's Upsert,
        // or a client following on_item would keep both.
        assert_eq!(
            &*i_evts.lock(),
            &["reset", "upsert:m1:User:replayed:"],
            "drop the provisional paint on the item stream first"
        );
    }

    #[test]
    fn provisional_paint_survives_promotion_to_fresh() {
        // Cold start paints the cache (provisional), then open_session finds it
        // CURRENT and replaces identical content. The paint must stop being
        // droppable, or the first live row would wipe the cached history.
        let (l, _t, _s, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        let snapshot = vec![
            Message { id: "m1".into(), role: "user".into(), content: "hi".into(), output: String::new() },
            Message { id: "m2".into(), role: "agent".into(), content: "hello".into(), output: String::new() },
        ];
        store.replace_provisional(snapshot.clone());
        store.replace(snapshot);
        store.append_chunk("user", "next", Some("m3"), false);
        assert_eq!(
            msgs(&store),
            vec![
                ("user".to_string(), "m1".to_string(), "hi".to_string()),
                ("agent".to_string(), "m2".to_string(), "hello".to_string()),
                ("user".to_string(), "m3".to_string(), "next".to_string()),
            ]
        );
    }

    #[test]
    fn authoritative_replace_is_not_dropped_by_later_rows() {
        let (l, _t, _s, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.replace(vec![Message {
            id: "a".into(),
            role: "agent".into(),
            content: "authoritative".into(),
            output: String::new(),
        }]);
        store.append_chunk("user", "live", Some("m2"), false);
        assert_eq!(msgs(&store).len(), 2, "a non-provisional snapshot must survive real rows");
    }

    #[test]
    fn tool_rows_also_supersede_a_provisional_paint() {
        let (l, _t, _s, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.replace_provisional(vec![Message {
            id: "old".into(),
            role: "agent".into(),
            content: "stale".into(),
            output: String::new(),
        }]);
        store.tool_call("Bash", "ls", "t1", ToolCallKind::Plain);
        let t = msgs(&store);
        assert_eq!(t.len(), 1, "the painted row must be gone");
        assert_eq!(t[0].0, "tool");
        assert_eq!(t[0].1, "t1");
    }

    #[test]
    fn replay_merge_keeps_the_older_prefix_and_replaces_the_tail() {
        let (l, _t, _s, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        // A stale cache: one row older than the tail, two the replay re-delivers.
        store.replace_for_merge(vec![
            Message { id: "old".into(), role: "user".into(), content: "ancient".into(), output: String::new() },
            Message { id: "m1".into(), role: "user".into(), content: "hi".into(), output: String::new() },
            Message { id: "m2".into(), role: "agent".into(), content: "hello".into(), output: String::new() },
        ]);
        // The bounded tail starts at m1 (present in the cache) and adds m3.
        store.append_chunk("user", "hi", Some("m1"), false);
        store.append_chunk("agent", "world", Some("m3"), false);
        assert_eq!(
            msgs(&store),
            vec![
                ("user".to_string(), "old".to_string(), "ancient".to_string()),
                ("user".to_string(), "m1".to_string(), "hi".to_string()),
                ("agent".to_string(), "m3".to_string(), "world".to_string()),
            ]
        );
        assert!(!store.end_merge(), "the tail anchored on m1: no gap");
    }

    #[test]
    fn replay_merge_anchors_on_a_tool_call_id_too() {
        let (l, _t, _s, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.replace_for_merge(vec![
            Message { id: "m1".into(), role: "user".into(), content: "hi".into(), output: String::new() },
            Message { id: "t1".into(), role: "tool".into(), content: "Bash".into(), output: String::new() },
        ]);
        store.tool_call("Bash", "ls", "t1", ToolCallKind::Plain);
        store.append_chunk("agent", "done", Some("m2"), false);
        assert_eq!(
            msgs(&store),
            vec![
                ("user".to_string(), "m1".to_string(), "hi".to_string()),
                ("tool".to_string(), "t1".to_string(), "Bash".to_string()),
                ("agent".to_string(), "m2".to_string(), "done".to_string()),
            ]
        );
        assert!(!store.end_merge());
    }

    #[test]
    fn replay_merge_without_overlap_reports_a_gap() {
        let (l, _t, _s, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.replace_for_merge(vec![Message {
            id: "old".into(),
            role: "user".into(),
            content: "ancient".into(),
            output: String::new(),
        }]);
        // The tail's first id is not in the cache: the replay starts past it, so
        // the caller must clear and load the whole transcript rather than leave
        // a hole.
        store.append_chunk("agent", "new", Some("z9"), false);
        assert!(store.end_merge(), "no overlap => a full reload is owed");
    }

    #[test]
    fn a_merge_paint_is_not_dropped_by_supersede_provisional() {
        let (l, _t, _s, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.replace_for_merge(vec![
            Message { id: "old".into(), role: "user".into(), content: "ancient".into(), output: String::new() },
            Message { id: "m1".into(), role: "agent".into(), content: "hello".into(), output: String::new() },
        ]);
        // A tool row that does not overlap must NOT wipe the painted prefix the
        // way a provisional paint would.
        store.tool_call("Bash", "ls", "t9", ToolCallKind::Plain);
        let t = msgs(&store);
        assert_eq!(t.len(), 3, "the painted prefix survives an unanchored row");
        assert_eq!(t[0].1, "old");
        assert_eq!(t[1].1, "m1");
        assert_eq!(t[2].1, "t9");
        assert!(store.end_merge());
    }

    // -----------------------------------------------------------------------
    // The rich item model (docs/TRANSCRIPT_MODEL.md)
    // -----------------------------------------------------------------------

    fn text_item(id: &str, kind: ItemKind, t: &str) -> Item {
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
    fn rich_items_keep_chart_app_and_toolgroup_kinds() {
        let (l, _t, _s, _i) = listener();
        let store = TranscriptStore::new(l);
        store.tool_call("Chart", "input", "c1", ToolCallKind::Chart { spec: r#"{"a":1}"#.into() });
        store.tool_call("A", "x", "g1", ToolCallKind::Plain);
        store.tool_call("B", "y", "g2", ToolCallKind::Plain);
        store.tool_call("Dashboard", "{}", "a1", ToolCallKind::McpApp {
            app_key: "ext|ui://d".into(),
            uri: "ui://d".into(),
            extension: "ext".into(),
            input: "{}".into(),
        });

        let items = store.rich_transcript();
        // Chart: the SPEC is retained (the flat projection threw it away).
        assert_eq!(items[0].kind, ItemKind::Chart);
        assert_eq!(items[0].detail, r#"{"a":1}"#);
        // Two consecutive plain calls collapse into one toolgroup item with both
        // children.
        assert_eq!(items[1].kind, ItemKind::ToolGroup);
        assert_eq!(items[1].calls.len(), 2);
        assert_eq!(items[1].calls[0].title, "A");
        assert_eq!(items[1].id, "g1", "anchored on the first call");
        // MCP app: the resource key survives.
        assert_eq!(items[2].kind, ItemKind::McpApp);
        assert_eq!(items[2].app_key, "ext|ui://d");
    }

    #[test]
    fn item_ops_emit_upsert_then_deltas_then_final_upsert() {
        let (l, _t, _s, i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("agent", "Hel", Some("m1"), false);
        store.append_chunk("agent", "lo", Some("m1"), false);
        store.tool_call("Bash", "ls", "t1", ToolCallKind::Plain);
        store.tool_update("t1", "in_progress", "out1", true);
        store.tool_update("t1", "completed", "final", false);

        assert_eq!(
            &*i_evts.lock(),
            &[
                // A fresh text row is an authoritative Upsert…
                "upsert:m1:Agent:Hel:",
                // …then cheap deltas.
                "text:m1:lo",
                "upsert:t1:Tool:Bash:in_progress",
                // Live tool output is a delta…
                "output:t1:out1",
                // …and the completion is the authoritative Upsert that replaces.
                "upsert:t1:Tool:Bash:completed",
            ]
        );
    }

    #[test]
    fn text_row_without_a_message_id_gets_a_stable_synthetic_id() {
        let (l, _t, _s, i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("thought", "hmm", None, true);
        store.append_chunk("thought", " more", None, true);
        // The server gave no id, but the item stream still needs a key: one is
        // assigned and the delta targets it.
        assert_eq!(&*i_evts.lock(), &["upsert:@1:Thought:hmm:", "text:@1: more"]);
        let items = store.rich_transcript();
        assert_eq!(items[0].id, "@1");
        assert_eq!(items[0].text, "hmm more");
    }

    #[test]
    fn rich_snapshot_round_trips_through_the_store() {
        let (l, _t, _s, _i) = listener();
        let store = TranscriptStore::new(l);
        store.set_session("s1");
        let items = vec![
            text_item("m1", ItemKind::User, "hi"),
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
                detail: "{}".into(),
                output: String::new(),
                status: "completed".into(),
                app_key: "ext|ui://d".into(),
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
                        output: "x".into(),
                        status: "completed".into(),
                    },
                    ToolCall {
                        id: "t2".into(),
                        title: "Read".into(),
                        detail: "f".into(),
                        output: "y".into(),
                        status: "completed".into(),
                    },
                ],
            },
        ];
        store.replace_rich(items.clone(), false, true);

        // The rebuild is faithful (kind + payload), so a cache paint no longer
        // degrades charts/apps.
        assert_eq!(store.rich_transcript(), items);
        assert_eq!(store.item("c1").unwrap().detail, r#"{"nodes":[]}"#);
        assert_eq!(store.item("a1").unwrap().app_key, "ext|ui://d");
        assert_eq!(store.item("t2").unwrap().kind, ItemKind::ToolGroup);
        assert_eq!(store.item("t2").unwrap().calls.len(), 2);
        // The legacy flat projection still works off the same rows.
        assert_eq!(store.item("m1").unwrap().kind, ItemKind::User);

        assert_eq!(store.window().oldest_id, "m1");
        assert_eq!(store.window().newest_id, "t1");
        assert!(store.window().has_older);
    }

    #[test]
    fn prepend_items_extends_the_window_backward() {
        let (l, _t, _s, i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.set_session("s1");
        store.replace_rich(
            vec![text_item("m2", ItemKind::Agent, "recent"), text_item("m3", ItemKind::User, "newest")],
            false,
            true,
        );
        assert_eq!(store.window().oldest_id, "m2");
        i_evts.lock().clear();

        store.prepend_items(
            vec![text_item("m1", ItemKind::User, "older"), text_item("@1", ItemKind::Thought, "hmm")],
            false,
        );

        let items = store.rich_transcript();
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["m1", "@1", "m2", "m3"], "older rows land in front");
        assert_eq!(store.window().oldest_id, "m1");
        assert!(!store.window().has_older);
        // Each older row is an Upsert, then the refreshed cursor. No Reset: the
        // client keeps what it has and prepends.
        assert_eq!(
            &*i_evts.lock(),
            &[
                "upsert:m1:User:older:",
                "upsert:@1:Thought:hmm:",
                "window:m1:false",
            ]
        );
    }

    #[test]
    fn synthetic_item_ids_do_not_leak_into_the_flat_projection() {
        // A live thought row has no server message_id; the flat projection must
        // still report an empty id (the desktop keys history on it), while the
        // rich item carries the synthetic key.
        let (l, _t, _s, _i) = listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("thought", "hmm", None, true);
        assert_eq!(msgs(&store), vec![("thought".to_string(), String::new(), "hmm".to_string())]);
        assert_eq!(store.rich_transcript()[0].id, "@1");
    }
}
