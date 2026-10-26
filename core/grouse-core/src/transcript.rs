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

use std::collections::{HashMap, VecDeque};

use parking_lot::Mutex;

use crate::speak::SpeakStream;
use crate::{
    CoreListener, Item, ItemKind, Message, ToolCall, ToolCallKind, TranscriptOp, TranscriptWindow,
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
    /// The thinking that immediately preceded THIS call, absorbed from the
    /// `Thought` rows above it (issue #13). Held per call, not per run, so the
    /// expanded run shows the loop in order: think, call, think, call.
    thought: String,
    /// A delegate call's child activity: the tool calls its SUBAGENT made, as
    /// goose forwards them onto this row (`_meta.toolNotification`, logger
    /// `subagent:<id>`). Bounded — a long delegate must not grow without limit.
    steps: Vec<crate::SubagentStep>,
    /// Transcript position (see `Item::seq`).
    seq: u32,
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
            thought: String::new(),
            steps: Vec::new(),
            seq: 0,
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
            thought: self.thought.clone(),
            seq: self.seq,
            steps: self.steps.clone(),
        }
    }
}

/// A run's thinking, joined — what its label was drawn from, and what the
/// summarizer is asked to condense.
fn run_thought(calls: &[ToolRow]) -> String {
    calls
        .iter()
        .map(|c| c.thought.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A one-line label for a run, from the model's own words for it. The thinking
/// is prose ("Let me check the existing todo…"), so take its first sentence and
/// cap it — this is a chip label, not a paragraph. Provisional: the model
/// rewrite replaces it at the end of the turn.
fn summarize(thought: &str) -> String {
    let line = thought
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .trim_start_matches(['#', '-', '*', '>', ' '])
        .trim();
    let end = line
        .find(['.', '!', '?'])
        .map(|i| i + 1)
        .unwrap_or(line.len());
    let s = line[..end]
        .trim_end_matches(['…', '.', '!', '?', ' '])
        .trim();
    if s.chars().count() > 80 {
        format!("{}…", s.chars().take(79).collect::<String>().trim_end())
    } else {
        s.to_string()
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
        /// Transcript position (see `Item::seq`).
        seq: u32,
        /// User rows only: attachments that rode the prompt (see `Item::attachments`).
        attachments: u32,
    },
    /// A lone plain tool call (collapses into a `ToolGroup` on the next call).
    /// The `String` is the run's one-line label (issue #13); the thinking rides
    /// on the row.
    Tool(ToolRow, String),
    /// Consecutive plain tool calls collapsed into one run, with the run's label.
    ToolGroup(Vec<ToolRow>, String),
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
            Bubble::Tool(r, _) => Message {
                id: r.tool_call_id.clone(),
                role: "tool".to_string(),
                content: r.title.clone(),
                output: String::new(),
            },
            Bubble::ToolGroup(calls, _) => {
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
            Bubble::Text {
                id,
                uid,
                role,
                text,
                seq,
                attachments,
                ..
            } => Item {
                id: uid.clone(),
                message_id: id.clone(),
                kind: match role.as_str() {
                    "user" => ItemKind::User,
                    "thought" => ItemKind::Thought,
                    "error" => ItemKind::Error,
                    _ => ItemKind::Agent,
                },
                text: text.clone(),
                seq: *seq,
                attachments: *attachments,
                ..empty_item()
            },
            Bubble::Tool(r, label) => Item {
                id: r.tool_call_id.clone(),
                kind: ItemKind::Tool,
                text: r.title.clone(),
                detail: r.detail.clone(),
                output: r.output.clone(),
                status: r.status.clone(),
                thought: r.thought.clone(),
                summary: label.clone(),
                steps: r.steps.clone(),
                seq: r.seq,
                ..empty_item()
            },
            Bubble::ToolGroup(calls, label) => Item {
                // Anchored on the first call (the legacy projection's rule too).
                id: calls[0].tool_call_id.clone(),
                kind: ItemKind::ToolGroup,
                calls: calls.iter().map(ToolRow::to_call).collect(),
                // The run's thinking, joined. Each child also carries its own
                // (that is what puts the loop back in order when expanded).
                thought: run_thought(calls),
                summary: label.clone(),
                // A group anchors on its first call's position.
                seq: calls.first().map(|c| c.seq).unwrap_or_default(),
                ..empty_item()
            },
            Bubble::Chart(r) => Item {
                id: r.tool_call_id.clone(),
                kind: ItemKind::Chart,
                text: r.title.clone(),
                // The spec, not the tool input: a chart row has no result.
                detail: r.spec.clone(),
                status: r.status.clone(),
                thought: r.thought.clone(),
                ..empty_item()
            },
            Bubble::McpApp(r) => Item {
                id: r.tool_call_id.clone(),
                kind: ItemKind::McpApp,
                text: r.title.clone(),
                detail: r.detail.clone(),
                status: r.status.clone(),
                app_key: r.app_key.clone(),
                thought: r.thought.clone(),
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
        thought: String::new(),
        summary: String::new(),
        steps: Vec::new(),
        seq: 0,
        attachments: 0,
        message_id: String::new(),
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
    /// Index of the OLDEST bubble the client currently holds. The store keeps
    /// the whole transcript (for the cache and the legacy flat projection), but
    /// the item stream paints only `bubbles[emitted_from..]` — the window
    /// (docs/TRANSCRIPT_MODEL.md). `load_older` walks this back.
    emitted_from: usize,
    /// Whether the cache can still produce items older than `oldest_id` (the
    /// client's `load_older` cursor). Set by the core when it paints a window.
    has_older: bool,
    /// Monotonic counter for ids on live text rows the server sent without a
    /// `message_id` (see [`Bubble::Text::uid`]).
    next_uid: u64,
    /// Next transcript position stamp. Assigned once per bubble, kept across
    /// evictions, and rewritten to 0..n on a wholesale replace.
    next_seq: u32,
    /// The assistant's visible text, sanitized and chunked into speakable
    /// sentences for a client's TTS engine (see [`crate::speak`]).
    speak: SpeakStream,
    /// A REPLAYED thought, waiting for the tool call that absorbs it. A LIVE
    /// thought needs no buffer — it is a bubble, and [`State::take_thoughts`]
    /// reads it from there — but a replayed one must never be emitted as a row
    /// of its own (it has no stable id to reconcile by, and would double the
    /// trail), so it waits here until a call claims it.
    pending_thought: Option<String>,
    /// Prompts echoed locally that the server has not named yet, oldest first
    /// (`text`, row uid). goose sends no user chunk for our own prompt; when a
    /// replay later names it, the row is adopted instead of duplicated.
    ///
    /// Keyed by UID, not index: a merge anchor truncates the bubbles and a cache
    /// paint replaces them, and an index then either points at nothing — which
    /// used to leave the entry at the head of the queue forever, blocking every
    /// later prompt from matching too — or at an unrelated row.
    pending_echo: VecDeque<(String, String)>,
}

/// Hard cap on retained bubbles (S-RC-4): a pathological session must not grow
/// memory without bound, so the OLDEST are evicted in bulk past this watermark.
///
/// It is a SAFETY NET, not a screenful estimate — it was 2000, and that silently
/// ATE HISTORY: a full replay re-ingests the whole session, `replace_rich` trims
/// before painting, and measured sessions on this box produce 4,761 and 6,161
/// bubbles. Everything below the watermark was dropped from the store (so
/// `load_older` could never reach it) and then written back over the cache, which
/// is what a "hard refresh misses a lot of messages" actually was. 20k keeps
/// every real session whole; items are small strings.
const MAX_BUBBLES: usize = 20_000;

/// How many newest items a paint emits up front. The rest stay in the store
/// (and the cache) and arrive as the client asks for them via `load_older`.
/// See docs/TRANSCRIPT_MODEL.md ("Windows"). A client that paints a long chat
/// in full pays for every delegate on every open; the window bounds that.
pub(crate) const CLIENT_WINDOW: usize = 60;

/// Rebuild `tool_by_id` from scratch — used after a bulk eviction or rebuild,
/// when the surviving bubbles' absolute indices have shifted.
fn index_bubbles(map: &mut HashMap<String, usize>, bubbles: &[Bubble]) {
    for (i, b) in bubbles.iter().enumerate() {
        match b {
            Bubble::Tool(r, _) | Bubble::Chart(r) | Bubble::McpApp(r) => {
                map.insert(r.tool_call_id.clone(), i);
            }
            Bubble::ToolGroup(calls, _) => {
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
        // The window's origin shifts with the surviving rows.
        self.emitted_from = self.emitted_from.saturating_sub(drop);
        self.tool_by_id.clear();
        index_bubbles(&mut self.tool_by_id, &self.bubbles);
        self.pending_echo = self
            .pending_echo
            .drain(..)
            .filter(|(_, uid)| self.bubbles.iter().any(|b| bubble_uid(b) == *uid))
            .collect();
        self.stream_idx = None;
    }

    /// A fresh `@n` id for a live text row the server did not id.
    fn new_uid(&mut self) -> String {
        self.next_uid += 1;
        format!("@{}", self.next_uid)
    }

    /// The next transcript position. Monotonic, so a late delivery still sorts
    /// into the right place on the client.
    fn new_seq(&mut self) -> u32 {
        self.next_seq += 1;
        self.next_seq
    }

    /// Keep synthetic ids unique against ones already held: a cache written by
    /// an earlier process carries `@n` ids while `next_uid` restarts at 0, so
    /// without this a fresh id-less chunk re-issues an `@n` already in use.
    fn seed_next_uid(&mut self) {
        for b in &self.bubbles {
            if let Bubble::Text { uid, .. } = b {
                if let Some(n) = uid.strip_prefix('@').and_then(|s| s.parse::<u64>().ok()) {
                    self.next_uid = self.next_uid.max(n);
                }
            }
        }
    }

    /// Forget everything and start clean, keeping the id index consistent.
    fn reset(&mut self) {
        self.bubbles.clear();
        self.tool_by_id.clear();
        self.stream_idx = None;
        self.stream_role.clear();
        self.stream_msg_id.clear();
        self.emitted_from = 0;
        self.provisional = false;
        self.merging = false;
        self.merge_anchored = false;
        self.merge_overlap = false;
        self.speak.reset();
        self.pending_thought = None;
        self.pending_echo.clear();
    }

    /// Rebuild the echo queue from rows a cache supplied: a user row with a
    /// core-assigned `@n` id was echoed locally and may still be waiting for
    /// the server's name.
    fn rebuild_pending_echo(&mut self) {
        self.pending_echo = self
            .bubbles
            .iter()
            .filter_map(|bubble| match bubble {
                Bubble::Text {
                    id,
                    uid,
                    role,
                    text,
                    ..
                } if id.is_empty() && uid.starts_with('@') && role == "user" => {
                    Some((text.clone(), uid.clone()))
                }
                _ => None,
            })
            .collect();
    }

    /// Close the turn: a trailing `Thought` bubble is FINISHED reasoning, not
    /// something a later turn's tool call may claim (see [`State::take_thoughts`]).
    /// The bubble's `thought` flag is the absorbability marker — its item kind
    /// comes from its role, so clearing the flag keeps the row a `Thought`.
    fn seal_thoughts(&mut self) {
        self.pending_thought = None;
        for b in self.bubbles.iter_mut().rev() {
            match b {
                Bubble::Text { thought, .. } if *thought => *thought = false,
                _ => break,
            }
        }
    }

    /// Take the thinking a tool call is about to absorb: every `Thought` bubble
    /// directly above it (the model thinks before it acts, and one burst
    /// accumulates into one bubble — but it can think, narrate, then think again,
    /// so take the whole contiguous run), plus a REPLAYED thought, which never
    /// became a bubble (see [`TranscriptStore::replay_thought`]).
    ///
    /// The bubbles are popped and the client told to drop those rows, so a long
    /// loop leaves no wall of "Thinking" blocks beside the run it explains — the
    /// thinking ends up INSIDE the run, in order, next to the call it produced.
    fn take_thoughts(&mut self, ops: &mut Vec<TranscriptOp>) -> String {
        let mut parts: Vec<String> = Vec::new();
        loop {
            let popped = match self.bubbles.last() {
                Some(Bubble::Text {
                    uid,
                    thought: true,
                    text,
                    ..
                }) => Some((uid.clone(), text.clone())),
                _ => None,
            };
            let Some((uid, text)) = popped else { break };
            self.bubbles.pop();
            ops.push(TranscriptOp::Remove { id: uid });
            parts.push(text);
        }
        if let Some(pending) = self.pending_thought.take() {
            parts.push(pending);
        }
        if parts.is_empty() {
            return String::new();
        }
        // Oldest first: the loop reads in the order the model worked.
        parts.reverse();
        self.emitted_from = self.emitted_from.min(self.bubbles.len());
        // A popped bubble may have been the live stream anchor.
        if self
            .stream_idx
            .map(|i| i >= self.bubbles.len())
            .unwrap_or(false)
        {
            self.stream_idx = None;
            self.stream_role.clear();
            self.stream_msg_id.clear();
        }
        parts.join("\n")
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
            // The window's origin is an index into the bubbles; truncation can
            // move it past the end.
            self.emitted_from = self.emitted_from.min(self.bubbles.len());
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
        Bubble::Tool(r, _) | Bubble::Chart(r) | Bubble::McpApp(r) => r.tool_call_id == id,
        Bubble::ToolGroup(calls, _) => calls.iter().any(|r| r.tool_call_id == id),
    }
}

/// The stable RICH item id of a bubble (see [`Bubble::Text::uid`]).
fn bubble_uid(b: &Bubble) -> String {
    match b {
        Bubble::Text { uid, .. } => uid.clone(),
        Bubble::Tool(r, _) | Bubble::Chart(r) | Bubble::McpApp(r) => r.tool_call_id.clone(),
        Bubble::ToolGroup(calls, _) => calls
            .first()
            .map(|c| c.tool_call_id.clone())
            .unwrap_or_default(),
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
        thought: c.thought.clone(),
        steps: Vec::new(),
        seq: 0,
    }
}

/// Rebuild a bubble from a rich item — the cache-load path. `None` for an empty
/// toolgroup (nothing to draw).
fn bubble_from_item(it: &Item) -> Option<Bubble> {
    match it.kind {
        ItemKind::User | ItemKind::Agent | ItemKind::Thought | ItemKind::Error => {
            // A whitespace-only agent/thought item is goose's "\n\n" between
            // tool calls (see `append_chunk`): dropping it keeps a cache paint
            // or a replay from re-creating an empty bubble.
            if matches!(it.kind, ItemKind::Agent | ItemKind::Thought) && it.text.trim().is_empty() {
                return None;
            }
            let role = match it.kind {
                ItemKind::User => "user",
                ItemKind::Thought => "thought",
                ItemKind::Error => "error",
                _ => "agent",
            };
            // The server's own id travels separately now, so a cached row keeps
            // it (and can anchor a merge). A v7 cache has none: fall back to the
            // old inference, where a core-assigned `@n` has nothing to recover.
            let id = if !it.message_id.is_empty() {
                it.message_id.clone()
            } else if it.id.starts_with('@') {
                String::new()
            } else {
                it.id.clone()
            };
            Some(Bubble::Text {
                id,
                uid: it.id.clone(),
                role: role.to_string(),
                text: it.text.clone(),
                thought: matches!(it.kind, ItemKind::Thought),
                seq: it.seq,
                attachments: it.attachments,
            })
        }
        ItemKind::Tool => Some(Bubble::Tool(
            ToolRow {
                title: it.text.clone(),
                detail: it.detail.clone(),
                tool_call_id: it.id.clone(),
                output: it.output.clone(),
                status: it.status.clone(),
                app_key: String::new(),
                spec: String::new(),
                thought: it.thought.clone(),
                steps: it.steps.clone(),
                seq: it.seq,
            },
            it.summary.clone(),
        )),
        ItemKind::ToolGroup => {
            if it.calls.is_empty() {
                None
            } else {
                Some(Bubble::ToolGroup(
                    it.calls.iter().map(call_to_row).collect(),
                    it.summary.clone(),
                ))
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
            thought: it.thought.clone(),
            steps: Vec::new(),
            seq: it.seq,
        })),
        ItemKind::McpApp => Some(Bubble::McpApp(ToolRow {
            title: it.text.clone(),
            detail: it.detail.clone(),
            tool_call_id: it.id.clone(),
            output: String::new(),
            status: it.status.clone(),
            app_key: it.app_key.clone(),
            spec: String::new(),
            thought: it.thought.clone(),
            steps: Vec::new(),
            seq: it.seq,
        })),
    }
}

/// Accumulates streamed chunks into transcript bubbles and fans events out to
/// the `CoreListener`. Internally synchronised: the runtime thread streams
/// while `transcript()` may be read from any thread (the getters take `&self`).
pub struct TranscriptStore {
    listener: Box<dyn CoreListener>,
    state: Mutex<State>,
    /// A `session/load` replay is being ingested: mutations update the store but
    /// emit NOTHING (grouse#18, see [`TranscriptStore::hold_emissions`]). An
    /// atomic, NOT a field of `state`: every mutation path already holds (or has
    /// just released) the state lock when it emits, and re-locking there would
    /// deadlock the runtime thread.
    hold: std::sync::atomic::AtomicBool,
}

impl TranscriptStore {
    pub fn new(listener: Box<dyn CoreListener>) -> Self {
        Self {
            listener,
            hold: std::sync::atomic::AtomicBool::new(false),
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
                emitted_from: 0,
                has_older: false,
                next_uid: 0,
                next_seq: 0,
                speak: SpeakStream::new(),
                pending_thought: None,
                pending_echo: VecDeque::new(),
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
    pub fn append_chunk(&self, role: &str, text: &str, message_id: Option<&str>, thought: bool) {
        let (ops, speaks) = {
            let mut st = self.state.lock();
            st.anchor_merge(message_id.unwrap_or(""));
            let mut ops = Vec::new();
            if st.supersede_provisional() {
                ops.push(TranscriptOp::Reset {
                    session_id: st.session_id.clone(),
                });
            }
            // A prompt we echoed locally, now named by the server: adopt that
            // row instead of appending a second copy of the question. Only an
            // exact text match counts, oldest first — the server names prompts
            // in the order it received them.
            if let Some(mid) = message_id.filter(|m| !m.is_empty()) {
                if st.stream_msg_id != mid && role == "user" {
                    // A prompt the store ALREADY holds under this server id needs
                    // no adoption: `anchor_merge` above truncated the cache at
                    // that row, so the replayed one is the row. Identity first.
                    if let Some((echo_text, echo_uid)) = st.pending_echo.front().cloned() {
                        let at = st.bubbles.iter().position(|b| bubble_uid(b) == echo_uid);
                        match at {
                            Some(idx) if echo_text == text => {
                                st.pending_echo.pop_front();
                                if let Bubble::Text { id, .. } = &mut st.bubbles[idx] {
                                    *id = mid.to_string();
                                }
                                st.stream_idx = Some(idx);
                                st.stream_role = role.to_string();
                                st.stream_msg_id = mid.to_string();
                                let item = st.bubbles[idx].item();
                                drop(st);
                                self.emit_ops(vec![TranscriptOp::Upsert { item }]);
                                return;
                            }
                            Some(_) => {} // a different prompt: append this one
                            None => {
                                // The paint (or the merge anchor) replaced the row
                                // this echo was waiting on, so it can never be
                                // adopted. Left at the head it also blocked every
                                // LATER prompt from matching, which is how one
                                // missed adoption turned into a run of prompts
                                // appended a second time at the bottom.
                                st.pending_echo.pop_front();
                            }
                        }
                    }
                }
            }
            let fresh = st.stream_idx.is_none()
                || st.stream_role != role
                || (message_id.is_some()
                    && !st.stream_msg_id.is_empty()
                    && message_id != Some(st.stream_msg_id.as_str()));

            // A whitespace-only chunk that would OPEN a bubble is goose's empty
            // assistant message between tool calls (its text is "\n\n"). It
            // renders as a bare "Goose" bubble with no content, so drop it.
            // Whitespace folded into an ALREADY-OPEN bubble is kept: that is
            // real intra-message spacing ("a" + "\n\n" + "b" must not become
            // "ab"). Leaving the stream anchor untouched lets the next real
            // chunk finalize the previous bubble as usual.
            if fresh && text.trim().is_empty() {
                return self.emit_ops(ops);
            }

            let idx = if fresh {
                // The previous streamed bubble is done: emit its authoritative
                // final state (the delta-only stream never did), so a client can
                // render its markdown once instead of per chunk.
                if let Some(prev) = st.stream_idx {
                    if let Some(b) = st.bubbles.get(prev) {
                        ops.push(TranscriptOp::Upsert { item: b.item() });
                    }
                }
                let uid = match message_id.filter(|m| !m.is_empty()) {
                    Some(m) => m.to_string(),
                    None => st.new_uid(),
                };
                let seq = st.new_seq();
                st.bubbles.push(Bubble::Text {
                    id: message_id.unwrap_or("").to_string(),
                    uid,
                    role: role.to_string(),
                    text: text.to_string(),
                    thought,
                    seq,
                    attachments: 0,
                });
                let idx = st.bubbles.len() - 1;
                st.stream_idx = Some(idx);
                st.stream_role = role.to_string();
                st.stream_msg_id = message_id.unwrap_or("").to_string();
                idx
            } else {
                let idx = st
                    .stream_idx
                    .expect("non-fresh implies an open stream bubble");
                if let Bubble::Text {
                    text: acc,
                    thought: t,
                    ..
                } = &mut st.bubbles[idx]
                {
                    acc.push_str(text);
                    *t = thought;
                }
                idx
            };

            // A fresh bubble is an authoritative Upsert; an accumulated chunk is
            // the O(chunk) delta.
            let op = if fresh {
                TranscriptOp::Upsert {
                    item: st.bubbles[idx].item(),
                }
            } else {
                match &st.bubbles[idx] {
                    Bubble::Text { uid, .. } => TranscriptOp::AppendText {
                        id: uid.clone(),
                        chunk: text.to_string(),
                    },
                    _ => TranscriptOp::Upsert {
                        item: st.bubbles[idx].item(),
                    },
                }
            };
            ops.push(op);
            // The assistant's *visible* text is what gets spoken; thoughts and
            // user text are not. Emitted after the lock is released.
            let speaks = if role == "agent" && !thought {
                st.speak.push(text)
            } else {
                Vec::new()
            };
            st.trim();
            (ops, speaks)
        };

        self.emit_ops(ops);
        for sentence in speaks {
            self.listener.on_speak(sentence);
        }
    }

    /// Echo a prompt we sent: ALWAYS a new row, and a turn boundary.
    ///
    /// goose sends no `user_message_chunk` for a prompt a client sent, so the
    /// core writes the question itself at send time. It is queued in
    /// `pending_echo`; when the server later names it (a replay carries the
    /// message id), [`Self::append_chunk`] adopts this row instead of appending
    /// a duplicate. Closing the live stream is what makes the reply that
    /// follows its own row rather than an extension of the previous turn.
    pub fn echo_prompt(&self, text: &str, attachments: u32) {
        // An attachment-only prompt still needs its row; an empty prompt with
        // no attachments is the one case with nothing to show.
        if text.is_empty() && attachments == 0 {
            return;
        }
        let ops = {
            let mut st = self.state.lock();
            st.anchor_merge("");
            let mut ops = Vec::new();
            if st.supersede_provisional() {
                ops.push(TranscriptOp::Reset {
                    session_id: st.session_id.clone(),
                });
            }
            // Finalize the reply the previous turn left open.
            if let Some(prev) = st.stream_idx {
                if let Some(b) = st.bubbles.get(prev) {
                    ops.push(TranscriptOp::Upsert { item: b.item() });
                }
            }
            let uid = st.new_uid();
            st.stream_idx = None;
            let seq = st.new_seq();
            st.bubbles.push(Bubble::Text {
                id: String::new(),
                uid,
                role: "user".to_string(),
                text: text.to_string(),
                thought: false,
                seq,
                attachments,
            });
            let idx = st.bubbles.len() - 1;
            let uid = bubble_uid(&st.bubbles[idx]);
            st.pending_echo.push_back((text.to_string(), uid));
            st.trim();
            let idx = idx.min(st.bubbles.len().saturating_sub(1));
            ops.push(TranscriptOp::Upsert {
                item: st.bubbles[idx].item(),
            });
            ops
        };
        self.emit_ops(ops);
    }

    /// A thought from a `session/load` replay. goose re-sends the reasoning trail
    /// on every replay, but a standalone replayed row would double it and carries
    /// no stable id to reconcile by — so it is held here (never emitted as a row)
    /// until the next tool call absorbs it as that call's thinking. A run's
    /// label therefore rebuilds identically from a replay.
    pub fn replay_thought(&self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        self.state.lock().pending_thought = Some(text.to_string());
    }

    /// A tool call. Consecutive `Plain` calls collapse into one toolgroup
    /// bubble (first call appended, later calls update it); `Chart`/`McpApp`
    /// calls always append their own standalone bubble (desktop
    /// `onToolCall` / `onChartToolCall` / `onMcpAppToolCall`).
    ///
    /// A tool call breaks the text stream: the next chunk starts a fresh
    /// bubble.
    pub fn tool_call(&self, title: &str, detail: &str, tool_call_id: &str, kind: ToolCallKind) {
        let ops = {
            let mut st = self.state.lock();
            st.anchor_merge(tool_call_id);
            let mut ops = Vec::new();
            if st.supersede_provisional() {
                ops.push(TranscriptOp::Reset {
                    session_id: st.session_id.clone(),
                });
            }
            // The thinking this call absorbs: every `Thought` row directly above
            // it (plus a replayed one). Folding it in here, per call, is what
            // makes a 20-call loop read as ONE run with the loop inside it,
            // instead of a wall of "Thinking" blocks beside per-call chips.
            let thought = st.take_thoughts(&mut ops);
            // A tool call ends the text stream: finalize the pending bubble.
            if let Some(prev) = st.stream_idx {
                if let Some(b) = st.bubbles.get(prev) {
                    ops.push(TranscriptOp::Upsert { item: b.item() });
                }
            }
            let mut row = ToolRow::new(title, detail, tool_call_id);
            row.seq = st.new_seq();
            row.thought = thought;
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
                Bubble::Tool(..) => LastRow::Tool,
                Bubble::ToolGroup(..) => LastRow::Group,
                _ => LastRow::Other,
            });
            // A thought does NOT open a run: the run is the whole LOOP, so a call
            // joins the run above it even though the model thought in between
            // (issue #13). The run's label comes from the first thinking it saw,
            // and the model rewrite replaces it at the end of the turn.
            let label = summarize(&row.thought);

            let idx = if plain {
                match last {
                    Some(LastRow::Tool) => {
                        // Convert the lone first call into a run, keeping the
                        // label of the call that opened it.
                        let idx = st.bubbles.len() - 1;
                        let first = match &st.bubbles[idx] {
                            Bubble::Tool(r, existing) => {
                                let label = if existing.is_empty() {
                                    label
                                } else {
                                    existing.clone()
                                };
                                (r.clone(), label)
                            }
                            _ => unreachable!("last row is Tool"),
                        };
                        st.bubbles[idx] = Bubble::ToolGroup(vec![first.0, row], first.1);
                        idx
                    }
                    Some(LastRow::Group) => {
                        let idx = st.bubbles.len() - 1;
                        if let Bubble::ToolGroup(calls, existing) = &mut st.bubbles[idx] {
                            if existing.is_empty() && !label.is_empty() {
                                *existing = label.clone();
                            }
                            calls.push(row);
                        }
                        idx
                    }
                    _ => {
                        st.bubbles.push(Bubble::Tool(row, label));
                        st.bubbles.len() - 1
                    }
                }
            } else {
                let bubble = match &kind {
                    ToolCallKind::Chart { .. } => Bubble::Chart(row),
                    _ => Bubble::McpApp(row),
                };
                st.bubbles.push(bubble);
                st.bubbles.len() - 1
            };
            st.tool_by_id.insert(row_id, idx);
            ops.push(TranscriptOp::Upsert {
                item: st.bubbles[idx].item(),
            });
            st.trim();

            // Desktop: every tool call resets the streaming anchor so the next
            // chunk opens a fresh bubble.
            st.stream_idx = None;
            ops
        };

        self.emit_ops(ops);
    }

    /// Record one child tool call a SUBAGENT made under a delegate row.
    ///
    /// goose keeps subagent sessions out of ACP (`ACP_VISIBLE_SESSION_TYPES`), so
    /// the child's activity is only ever visible as these forwarded
    /// notifications; the row is where it belongs. Bounded to the last
    /// [`MAX_SUBAGENT_STEPS`] calls so a long delegate cannot grow without limit,
    /// and emitted as an authoritative Upsert (a step is a discrete fact, not a
    /// delta).
    pub fn tool_step(&self, id: &str, step: crate::SubagentStep) {
        const MAX_SUBAGENT_STEPS: usize = 200;
        let ops = {
            let mut st = self.state.lock();
            st.anchor_merge(id);
            let mut ops = Vec::new();
            if st.supersede_provisional() {
                ops.push(TranscriptOp::Reset {
                    session_id: st.session_id.clone(),
                });
            }
            let idx = st
                .tool_by_id
                .get(id)
                .copied()
                .filter(|i| *i < st.bubbles.len());
            if let Some(idx) = idx {
                let push = |steps: &mut Vec<crate::SubagentStep>, step| {
                    if steps.len() >= MAX_SUBAGENT_STEPS {
                        steps.remove(0);
                    }
                    steps.push(step);
                };
                let mut found = false;
                match &mut st.bubbles[idx] {
                    Bubble::Tool(r, _) if r.tool_call_id == id => {
                        push(&mut r.steps, step);
                        found = true;
                    }
                    // A delegate call that joined a run lives in the group, not
                    // in its own bubble; sharing the group's anchor must not
                    // lose its child activity.
                    Bubble::ToolGroup(calls, _) => {
                        if let Some(call) = calls.iter_mut().find(|c| c.tool_call_id == id) {
                            push(&mut call.steps, step);
                            found = true;
                        }
                    }
                    _ => {}
                }
                if found {
                    ops.push(TranscriptOp::Upsert {
                        item: st.bubbles[idx].item(),
                    });
                }
            }
            ops
        };
        self.emit_ops(ops);
    }

    /// A tool lifecycle update. Searches from the newest bubble backwards for
    /// the tool call (tool / toolgroup / chart / mcpapp all carry their id).
    /// Status always updates; `output` follows the desktop rule: live output
    /// APPENDS to the accumulated output, the completion update (live=false)
    /// REPLACES it, and an empty output leaves it untouched. Chart/mcp-app
    /// bubbles take status only (desktop quirk).
    pub fn tool_update(&self, id: &str, status: &str, output: &str, live: bool) {
        let ops = {
            let mut st = self.state.lock();
            st.anchor_merge(id);
            let mut ops = Vec::new();
            if st.supersede_provisional() {
                ops.push(TranscriptOp::Reset {
                    session_id: st.session_id.clone(),
                });
            }
            // O(1) lookup via the tool_by_id index (S-RC-4) — the previous
            // newest-backwards scan was O(n) per update, quadratic over a long
            // chat. Fall back to a scan only if the index is somehow stale.
            let idx = st
                .tool_by_id
                .get(id)
                .copied()
                .filter(|i| *i < st.bubbles.len());
            if let Some(idx) = idx {
                let found = match &mut st.bubbles[idx] {
                    Bubble::Tool(r, _) if r.tool_call_id == id => {
                        r.status = status.to_string();
                        apply_output(&mut r.output, output, live);
                        true
                    }
                    Bubble::Chart(r) | Bubble::McpApp(r) if r.tool_call_id == id => {
                        r.status = status.to_string();
                        true
                    }
                    Bubble::ToolGroup(calls, _) => {
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
                    // A live append to a standalone tool is the O(chunk) delta;
                    // everything else (completion replace, group child, chart /
                    // app status) is an authoritative Upsert.
                    let standalone_live = live
                        && !output.is_empty()
                        && matches!(&st.bubbles[idx], Bubble::Tool(r, _) if r.tool_call_id == id);
                    ops.push(if standalone_live {
                        TranscriptOp::AppendOutput {
                            id: id.to_string(),
                            chunk: output.to_string(),
                        }
                    } else {
                        TranscriptOp::Upsert {
                            item: st.bubbles[idx].item(),
                        }
                    });
                }
            }
            ops
        };

        self.emit_ops(ops);
    }

    /// Promote a previously-plain tool call to an MCP App. The server does not
    /// know a call is an app at `tool_call` time — it hydrates `_meta.goose.mcpApp`
    /// onto the COMPLETING `tool_call_update` — so the core first announced a
    /// Plain call, and the app identity arrives late.
    ///
    /// Clients learn via an `Upsert` carrying `kind=McpApp` for the same
    /// `tool_call_id`, which replaces the row in place (and, when the call was
    /// pulled out of a collapsed toolgroup, an Upsert for the shrunk group).
    /// Re-promoting is a no-op (replays re-issue the update).
    pub fn tool_app(&self, id: &str, uri: &str, extension: &str) {
        // NOTE: no provisional drop here. The promoting update is always
        // preceded by the call / lifecycle update that supersedes a paint (and
        // emits the item `Reset`), so dropping here would clear the very row we
        // need to promote.
        let ops = {
            let mut st = self.state.lock();
            st.anchor_merge(id);
            let Some(&idx) = st.tool_by_id.get(id) else {
                return;
            };
            if idx >= st.bubbles.len() {
                return;
            }
            let row = match &st.bubbles[idx] {
                Bubble::Tool(r, _) if r.tool_call_id == id => Some(r.clone()),
                Bubble::ToolGroup(calls, _) => calls.iter().find(|c| c.tool_call_id == id).cloned(),
                _ => None, // already an app/chart bubble, or a stale index
            };
            let Some(mut row) = row else { return };
            // The app's resource key travels on the row so the cache keeps it.
            row.app_key = format!("{extension}|{uri}");

            if matches!(&st.bubbles[idx], Bubble::Tool(..)) {
                st.bubbles[idx] = Bubble::McpApp(row);
                vec![TranscriptOp::Upsert {
                    item: st.bubbles[idx].item(),
                }]
            } else {
                let Bubble::ToolGroup(calls, label) = &st.bubbles[idx] else {
                    unreachable!()
                };
                let label = label.clone();
                let pos = calls.iter().position(|c| c.tool_call_id == id).unwrap();
                let mut calls = calls.clone();
                calls.remove(pos);
                if calls.is_empty() {
                    st.bubbles[idx] = Bubble::McpApp(row);
                    vec![TranscriptOp::Upsert {
                        item: st.bubbles[idx].item(),
                    }]
                } else {
                    st.bubbles[idx] = Bubble::ToolGroup(calls, label);
                    st.bubbles.insert(idx + 1, Bubble::McpApp(row));
                    for j in st.tool_by_id.values_mut() {
                        if *j > idx {
                            *j += 1;
                        }
                    }
                    st.tool_by_id.insert(id.to_string(), idx + 1);
                    let group_item = st.bubbles[idx].item();
                    let app_item = st.bubbles[idx + 1].item();
                    vec![
                        TranscriptOp::Upsert { item: group_item },
                        TranscriptOp::Upsert { item: app_item },
                    ]
                }
            }
        };

        self.emit_ops(ops);
    }

    /// Context-window usage + cost.
    pub fn usage(&self, used: i64, size: i64, cost: f64, currency: &str) {
        self.listener
            .on_usage(used, size, cost, currency.to_string());
    }

    /// A turn finished. The last text bubble is final: emit its authoritative
    /// `Upsert` (the streaming deltas never carried a completion) so a client
    /// renders the markdown once, then end the turn on the stream.
    pub fn run_ended(&self, stop_reason: &str) {
        self.seal_turn();
        self.announce_run_ended(stop_reason);
    }

    /// Close the turn's streamed row and flush its speech — the MUTATING half
    /// of [`Self::run_ended`], split out so a caller can persist a settled
    /// snapshot between the two (a cache written before the seal would keep an
    /// unsealed thought). Emits the row's final `Upsert` and any sentences.
    pub fn seal_turn(&self) {
        let (op, speak) = {
            let mut st = self.state.lock();
            // Thinking that led nowhere must not frame the NEXT turn's tools.
            st.seal_thoughts();
            let op = st
                .stream_idx
                .and_then(|idx| st.bubbles.get(idx))
                .map(|b| TranscriptOp::Upsert { item: b.item() });
            let speak = st.speak.flush();
            (op, speak)
        };
        if let Some(op) = op {
            self.listener.on_item(op);
        }
        for sentence in speak {
            self.listener.on_speak(sentence);
        }
    }

    /// Tell the client the turn is over (fires `on_run_ended` with this
    /// store's session id). The non-mutating half of [`Self::run_ended`].
    pub fn announce_run_ended(&self, stop_reason: &str) {
        let session_id = self.state.lock().session_id.clone();
        self.listener
            .on_run_ended(session_id, stop_reason.to_string());
    }

    /// Wipe the transcript and reset the stream state; emits an item `Reset` +
    /// empty `Window` (suppressed while a replay is being ingested — the
    /// release's own paint carries the wipe).
    pub fn clear(&self) {
        let session_id = {
            let mut st = self.state.lock();
            st.reset();
            st.session_id.clone()
        };
        self.emit_ops(vec![
            TranscriptOp::Reset { session_id },
            TranscriptOp::Window {
                oldest_id: String::new(),
                has_older: false,
            },
        ]);
    }

    /// The session the held rows belong to (stamped on `Reset` ops).
    pub fn set_session(&self, session_id: &str) {
        self.state.lock().session_id = session_id.to_string();
    }

    /// The id of the oldest item the CLIENT holds (the window's origin).
    pub fn oldest_id(&self) -> String {
        let st = self.state.lock();
        st.bubbles
            .get(st.emitted_from)
            .map(bubble_uid)
            .unwrap_or_default()
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
            oldest_id: st
                .bubbles
                .get(st.emitted_from)
                .map(bubble_uid)
                .unwrap_or_default(),
            newest_id: st.bubbles.last().map(bubble_uid).unwrap_or_default(),
            has_older: st.has_older,
        }
    }

    /// Walk the window back by `count` items: emit them oldest-first as
    /// `Upsert`s, then a refreshed `Window`. The client prepends them, so they
    /// keep the transcript's order (see docs/TRANSCRIPT_MODEL.md).
    pub fn load_older(&self, count: usize) {
        let (session_id, batch, oldest, has_older) = {
            let mut st = self.state.lock();
            let from = st.emitted_from.saturating_sub(count);
            // An empty batch (nothing older) is NOT an early return: the client's
            // in-flight load needs the refreshed Window to settle.
            let batch: Vec<Item> = st.bubbles[from..st.emitted_from]
                .iter()
                .map(Bubble::item)
                .collect();
            st.emitted_from = from;
            st.has_older = from > 0;
            let oldest = st.bubbles.get(from).map(bubble_uid).unwrap_or_default();
            let has_older = st.has_older;
            (st.session_id.clone(), batch, oldest, has_older)
        };
        let _ = session_id; // no Reset: the client keeps its window and prepends
        for item in batch {
            self.listener.on_item(TranscriptOp::Upsert { item });
        }
        self.listener.on_item(TranscriptOp::Window {
            oldest_id: oldest,
            has_older,
        });
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

    /// The run (the tool / toolgroup ITEM) a tool call landed in, as its item
    /// id. A toolgroup is anchored on its first call, so a call in the middle of
    /// a run answers with the run's own id — which is what the summarizer
    /// addresses a run by.
    pub fn run_id_for(&self, tool_call_id: &str) -> Option<String> {
        let st = self.state.lock();
        let b = st.bubbles.get(*st.tool_by_id.get(tool_call_id)?)?;
        match b {
            Bubble::Tool(r, _) => Some(r.tool_call_id.clone()),
            Bubble::ToolGroup(calls, _) => calls.first().map(|c| c.tool_call_id.clone()),
            _ => None,
        }
    }

    /// A run's framing for the summarizer: the thinking it absorbed and its call
    /// titles. `None` when the run has no thinking — there is nothing to condense,
    /// so the inference would be spent for nothing.
    pub fn run_framing(&self, id: &str) -> Option<(String, Vec<String>)> {
        let st = self.state.lock();
        let b = st.bubbles.get(*st.tool_by_id.get(id)?)?;
        let calls: &[ToolRow] = match b {
            Bubble::Tool(r, _) => std::slice::from_ref(r),
            Bubble::ToolGroup(calls, _) => calls,
            _ => return None,
        };
        let thought = run_thought(calls);
        if thought.trim().is_empty() {
            return None;
        }
        Some((thought, calls.iter().map(|c| c.title.clone()).collect()))
    }

    /// Replace a run's label with the model's rewrite, emitting the `Upsert` the
    /// client needs. The absorbed thinking is untouched: it is still what the
    /// expanded run shows.
    pub fn set_run_summary(&self, id: &str, summary: &str) {
        let op = {
            let mut st = self.state.lock();
            let Some(&idx) = st.tool_by_id.get(id) else {
                return;
            };
            {
                let Some(b) = st.bubbles.get_mut(idx) else {
                    return;
                };
                let label = match b {
                    Bubble::Tool(_, label) => label,
                    Bubble::ToolGroup(_, label) => label,
                    _ => return,
                };
                if *label == summary {
                    return;
                }
                *label = summary.to_string();
            }
            Some(st.bubbles[idx].item())
        };
        if let Some(item) = op {
            self.emit_ops(vec![TranscriptOp::Upsert { item }]);
        }
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
            // Tell the client the painted rows are gone, before the replay's
            // Upserts arrive. Suppressed while the replay is held: the release
            // emits its own Reset as part of the single windowed paint.
            self.emit_ops(vec![TranscriptOp::Reset { session_id }]);
        }
    }

    fn emit_ops(&self, ops: Vec<TranscriptOp>) {
        // While a replay is being ingested the rows update this store and stay
        // OFF the wire: painting each one as it arrives is what made a reload
        // walk the history from the top (grouse#18). The release emits one
        // windowed paint of the result.
        if self.hold.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        for op in ops {
            self.listener.on_item(op);
        }
    }

    /// Hold (or drop) the per-mutation emissions for a replay (grouse#18). Held
    /// rows still land in the store, the cache and the window.
    pub fn hold_emissions(&self, hold: bool) {
        self.hold.store(hold, std::sync::atomic::Ordering::SeqCst);
    }

    /// End a held replay with ONE windowed paint — `Reset` + the newest
    /// `CLIENT_WINDOW` items + the cursor (exactly what a fresh paint emits), so
    /// the client lands on the most recent messages and walks back with
    /// `load_older`. A no-op when nothing was held.
    pub fn release_held_replay(&self) {
        if self.hold.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.repaint_window();
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
        self.replace_rich(items, false);
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
                if merged {
                    st.session_id.clone()
                } else {
                    String::new()
                },
            )
        };
        if !merged_session.is_empty() {
            // The store was reconciled in place: repaint the window, since a
            // Reset alone would leave the client empty until some later event.
            self.repaint_window();
        }
        gap
    }

    /// Reset the item client and re-paint the current window from the store.
    /// Used when the store was reconciled in place (a replay merge) rather than
    /// wholesale replaced.
    fn repaint_window(&self) {
        let (session_id, window, oldest, has_older) = {
            let mut st = self.state.lock();
            st.emitted_from = st.bubbles.len().saturating_sub(CLIENT_WINDOW);
            let has_older = st.emitted_from > 0;
            st.has_older = has_older;
            let window: Vec<Item> = st.bubbles[st.emitted_from..]
                .iter()
                .map(Bubble::item)
                .collect();
            let oldest = window.first().map(|i| i.id.clone()).unwrap_or_default();
            (st.session_id.clone(), window, oldest, has_older)
        };
        self.listener.on_item(TranscriptOp::Reset { session_id });
        for item in window {
            self.listener.on_item(TranscriptOp::Upsert { item });
        }
        self.listener.on_item(TranscriptOp::Window {
            oldest_id: oldest,
            has_older,
        });
    }

    /// Rebuild the transcript from a rich snapshot (the rich cache paint):
    /// clear + rebuild, emitting Clear + `Reset` + one `Upsert` per item + a
    /// `Window`. Full fidelity — charts, MCP apps and toolgroups survive.
    pub fn replace_rich(&self, items: Vec<Item>, provisional: bool) {
        // Drop whitespace-only agent/thought items before the identical-paint
        // check below so it compares like with like: `bubble_from_item` drops
        // them too, and a stale cache carrying them must not defeat the no-op.
        let items: Vec<Item> = items
            .into_iter()
            .filter(|it| {
                !(matches!(it.kind, ItemKind::Agent | ItemKind::Thought)
                    && it.text.trim().is_empty())
            })
            .collect();
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
                st.has_older = st.emitted_from > 0;
                return;
            }
        }
        let (session_id, window, oldest, has_older) = {
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
            // A wholesale paint re-stamps the order it just built: the client
            // clears on Reset, so these positions are what it inserts by. A
            // group is ONE bubble but N client rows, so each call consumes its
            // own position — sharing the group's anchor put every call in a run
            // at the same place.
            let mut next = 0u32;
            for b in st.bubbles.iter_mut() {
                match b {
                    Bubble::Text { seq, .. } => {
                        *seq = next;
                        next += 1;
                    }
                    Bubble::Tool(r, _) | Bubble::Chart(r) | Bubble::McpApp(r) => {
                        r.seq = next;
                        next += 1;
                    }
                    Bubble::ToolGroup(calls, _) => {
                        for c in calls.iter_mut() {
                            c.seq = next;
                            next += 1;
                        }
                    }
                }
            }
            st.next_seq = next;
            st.seed_next_uid();
            st.rebuild_pending_echo();
            st.provisional = provisional;
            st.trim();
            // Paint only the newest CLIENT_WINDOW items; the store keeps all (for
            // the cache and the legacy flat projection), load_older walks back.
            st.emitted_from = st.bubbles.len().saturating_sub(CLIENT_WINDOW);
            let has_older = st.emitted_from > 0;
            st.has_older = has_older;
            let window: Vec<Item> = st.bubbles[st.emitted_from..]
                .iter()
                .map(Bubble::item)
                .collect();
            let oldest = window.first().map(|i| i.id.clone()).unwrap_or_default();
            (st.session_id.clone(), window, oldest, has_older)
        };
        self.listener.on_item(TranscriptOp::Reset { session_id });
        for item in window {
            self.listener.on_item(TranscriptOp::Upsert { item });
        }
        self.listener.on_item(TranscriptOp::Window {
            oldest_id: oldest,
            has_older,
        });
    }

    fn replace_inner(&self, messages: Vec<Message>, provisional: bool) {
        // See `replace_rich`: drop whitespace-only agent/thought rows up front
        // so the identical-paint check compares like with like.
        let messages: Vec<Message> = messages
            .into_iter()
            .filter(|m| {
                !((m.role == "agent" || m.role == "thought") && m.content.trim().is_empty())
            })
            .collect();
        let (session_id, oldest, items, has_older) = {
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
                    let seq = st.new_seq();
                    let idx = st.bubbles.len();
                    st.tool_by_id.insert(m.id.clone(), idx);
                    st.bubbles.push(Bubble::Tool(
                        ToolRow {
                            seq,
                            title: m.content.clone(),
                            detail: String::new(),
                            tool_call_id: m.id.clone(),
                            output: String::new(),
                            status: String::new(),
                            app_key: String::new(),
                            spec: String::new(),
                            thought: String::new(),
                            steps: Vec::new(),
                        },
                        String::new(),
                    ));
                } else {
                    let uid = if m.id.is_empty() {
                        st.new_uid()
                    } else {
                        m.id.clone()
                    };
                    let seq = st.new_seq();
                    st.bubbles.push(Bubble::Text {
                        id: m.id.clone(),
                        uid,
                        role: m.role.clone(),
                        text: m.content.clone(),
                        thought: m.role == "thought",
                        seq,
                        attachments: 0,
                    });
                }
            }
            st.seed_next_uid();
            st.rebuild_pending_echo();
            st.provisional = provisional;
            st.emitted_from = st.bubbles.len().saturating_sub(CLIENT_WINDOW);
            let has_older = st.emitted_from > 0;
            st.has_older = has_older;
            let items: Vec<Item> = st.bubbles[st.emitted_from..]
                .iter()
                .map(Bubble::item)
                .collect();
            let oldest = items.first().map(|i| i.id.clone()).unwrap_or_default();
            (st.session_id.clone(), oldest, items, has_older)
        };
        self.listener.on_item(TranscriptOp::Reset { session_id });
        for item in items {
            self.listener.on_item(TranscriptOp::Upsert { item });
        }
        self.listener.on_item(TranscriptOp::Window {
            oldest_id: oldest,
            has_older,
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
    use parking_lot::Mutex;
    use std::sync::Arc;

    use crate::{
        ConfigOption, ConnectionStatus, CoreListener, Item, ItemKind, Message, PermissionRequest,
        ProjectSummary, SessionSummary, ToolCall, ToolCallKind,
    };

    use super::{summarize, Bubble, TranscriptStore};

    /// Records the item ops the store fans out, as plain strings so assertions
    /// stay readable.
    struct TestListener {
        item_events: Arc<Mutex<Vec<String>>>,
        speak_events: Arc<Mutex<Vec<String>>>,
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
            Op::Window {
                oldest_id,
                has_older,
            } => format!("window:{oldest_id}:{has_older}"),
        }
    }

    fn listener() -> (Box<dyn CoreListener>, Arc<Mutex<Vec<String>>>) {
        let i = Arc::new(Mutex::new(Vec::new()));
        let l = TestListener {
            item_events: i.clone(),
            speak_events: Arc::new(Mutex::new(Vec::new())),
        };
        (Box::new(l), i)
    }

    /// A listener that records the speak stream instead of the item ops.
    fn speak_listener() -> (Box<dyn CoreListener>, Arc<Mutex<Vec<String>>>) {
        let s = Arc::new(Mutex::new(Vec::new()));
        let l = TestListener {
            item_events: Arc::new(Mutex::new(Vec::new())),
            speak_events: s.clone(),
        };
        (Box::new(l), s)
    }

    impl CoreListener for TestListener {
        fn on_status(&self, _status: ConnectionStatus) {}
        fn on_sessions(&self, _sessions: Vec<SessionSummary>) {}
        fn on_item(&self, op: crate::TranscriptOp) {
            self.item_events.lock().push(op_desc(&op));
        }
        fn on_usage(&self, _used: i64, _size: i64, _cost: f64, _currency: String) {}
        fn on_run_ended(&self, _session: String, _stop_reason: String) {}
        fn on_speak(&self, text: String) {
            self.speak_events.lock().push(text);
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
        fn on_session_open_failed(&self, _session_id: String, _message: String) {}
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
        let (l, _i_evts) = listener();
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
                (
                    "user".to_string(),
                    "m1".to_string(),
                    "Hello world".to_string()
                ),
                ("user".to_string(), "m2".to_string(), "!".to_string()),
                (
                    "agent".to_string(),
                    "m2".to_string(),
                    "Sure thing".to_string()
                ),
                ("thought".to_string(), String::new(), "hmm more".to_string()),
            ]
        );

        // Fresh bubbles Append; same-bubble accumulation Updates.
        // Every chunk is echoed on the stream.
    }

    #[test]
    fn toolgroup_collapse() {
        let (l, _i_evts) = listener();
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
            Bubble::ToolGroup(calls, _) => assert_eq!(calls.len(), 3),
            other => panic!("expected a toolgroup, got {other:?}"),
        }

        // First call appends, each later call updates the same bubble.
        // Every call is echoed on the stream.

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
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);

        // Standalone plain call promoted when the completing update carries mcpApp.
        store.tool_call("Dashboard", "{}", "d1", ToolCallKind::Plain);
        store.tool_app("d1", "ui://x/dashboard", "monitorext");
        match &store.rows_for_test()[0] {
            Bubble::McpApp(r) => assert_eq!(r.tool_call_id, "d1"),
            other => panic!("expected McpApp bubble, got {other:?}"),
        }

        // Idempotent: replays re-issue the update; the stream must not re-announce.
        store.tool_app("d1", "ui://x/dashboard", "monitorext");

        // Group extraction: a promoted call leaves the group and gets its own
        // bubble (Append), and later status updates still find it.
        let (l2, _i2) = listener();
        let store2 = TranscriptStore::new(l2);
        store2.tool_call("A", "x", "g1", ToolCallKind::Plain);
        store2.tool_call("B", "y", "g2", ToolCallKind::Plain);
        store2.tool_app("g2", "ui://b", "extb");
        match &store2.rows_for_test()[0] {
            Bubble::ToolGroup(calls, _) => assert_eq!(calls.len(), 1),
            other => panic!("group should shrink to one call, got {other:?}"),
        }
        store2.tool_update("g2", "completed", "out", false);
    }

    #[test]
    fn chart_and_mcpapp_calls_never_collapse() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);

        store.tool_call("Chart", "", "c1", ToolCallKind::Chart { spec: "{}".into() });
        store.tool_call("Bash", "ls", "t4", ToolCallKind::Plain); // chart in between → no merge
        store.tool_call(
            "App",
            "input",
            "a1",
            ToolCallKind::McpApp {
                app_key: "k".into(),
                uri: "u".into(),
                extension: "x".into(),
                input: "i".into(),
            },
        );

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
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);

        store.tool_call("Bash", "echo hi", "t1", ToolCallKind::Plain);
        store.tool_update("t1", "in_progress", "line1\n", true);
        store.tool_update("t1", "in_progress", "line2\n", true);
        // Live output accumulates.
        let rows = store.rows_for_test();
        let out = match &rows[0] {
            Bubble::Tool(r, _) => &r.output,
            other => panic!("expected a tool bubble, got {other:?}"),
        };
        assert_eq!(out, "line1\nline2\n");

        // The completion update REPLACES the accumulated output.
        store.tool_update("t1", "completed", "final output", false);
        let rows = store.rows_for_test();
        match &rows[0] {
            Bubble::Tool(r, _) => {
                assert_eq!(r.output, "final output");
                assert_eq!(r.status, "completed");
            }
            other => panic!("expected a tool bubble, got {other:?}"),
        }

        // An empty completion output leaves the accumulator untouched.
        store.tool_update("t1", "completed", "", false);
        let rows = store.rows_for_test();
        match &rows[0] {
            Bubble::Tool(r, _) => assert_eq!(r.output, "final output"),
            other => panic!("expected a tool bubble, got {other:?}"),
        }

        // Updates inside a collapsed group behave the same way.
        store.tool_call("Bash", "ls", "t2", ToolCallKind::Plain);
        store.tool_update("t2", "in_progress", "a", true);
        store.tool_update("t2", "completed", "done", false);
        let rows = store.rows_for_test();
        match &rows[0] {
            Bubble::ToolGroup(calls, _) => {
                assert_eq!(calls[1].output, "done");
                assert_eq!(calls[1].status, "completed");
            }
            other => panic!("expected a toolgroup, got {other:?}"),
        }

        // Every successful tool update fires a transcript Update.
    }

    #[test]
    fn items_carry_their_transcript_position() {
        // Clients insert by `seq`, not by arrival: an item can be delivered late
        // (a prompt echo after the reply it prompted, or a page of older rows
        // drained back in) and must still sort into place.
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("agent", "an answer", Some("m1"), false);
        store.echo_prompt("a question", 0);
        store.tool_call("shell", "{}", "t1", ToolCallKind::Plain);

        let seqs: Vec<u32> = store.rich_transcript().iter().map(|i| i.seq).collect();
        assert_eq!(seqs.len(), 3, "{seqs:?}");
        assert!(
            seqs.windows(2).all(|w| w[0] < w[1]),
            "positions must ascend as the transcript grows: {seqs:?}"
        );
    }

    #[test]
    fn a_replayed_message_keeps_its_position() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("agent", "Hello", Some("m1"), false);
        store.append_chunk("agent", " world", Some("m1"), false);
        let before = store.rich_transcript()[0].seq;

        // The same message re-delivered by a replay updates the row in place.
        store.append_chunk("agent", "Hello", Some("m1"), false);
        let after = store.rich_transcript();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].seq, before, "identity means the position does not move");
    }

    #[test]
    fn a_wholesale_paint_restamps_positions_in_order() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        let items = vec![
            crate::Item {
                id: "a".into(),
                kind: ItemKind::User,
                text: "one".into(),
                seq: 99,
                ..super::empty_item()
            },
            crate::Item {
                id: "b".into(),
                kind: ItemKind::Agent,
                text: "two".into(),
                seq: 99,
                ..super::empty_item()
            },
        ];
        store.replace_rich(items, false);
        let seqs: Vec<u32> = store.rich_transcript().iter().map(|i| i.seq).collect();
        assert_eq!(seqs, vec![0, 1], "a repaint owns the order it just built");
    }

    #[test]
    fn a_delegate_row_carries_its_subagent_steps_through_a_reload() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.tool_call("delegate", "{}", "d1", ToolCallKind::Plain);
        store.tool_step(
            "d1",
            crate::SubagentStep {
                name: "developer__shell".into(),
                detail: "git status".into(),
            },
        );
        store.tool_step(
            "d1",
            crate::SubagentStep {
                name: "read_file".into(),
                detail: "src/main.rs".into(),
            },
        );
        let items = store.rich_transcript();
        assert_eq!(items.len(), 1, "{items:?}");
        let steps: Vec<&str> = items[0].steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(steps, vec!["developer__shell", "read_file"]);

        // The cache is the record: a reload must show what the delegate did.
        let (l, _i_evts) = listener();
        let reloaded = TranscriptStore::new(l);
        reloaded.replace_rich(items, false);
        let after = reloaded.rich_transcript();
        assert_eq!(after[0].steps.len(), 2, "{after:?}");
        assert_eq!(after[0].steps[1].detail, "src/main.rs");
    }

    #[test]
    fn a_grouped_run_gives_every_call_its_own_position() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.tool_call("shell", "{}", "t1", ToolCallKind::Plain);
        store.tool_call("read", "{}", "t2", ToolCallKind::Plain);
        let items = store.rich_transcript();
        let group = items
            .iter()
            .find(|i| i.kind == ItemKind::ToolGroup)
            .expect("two consecutive calls form a run");
        let seqs: Vec<u32> = group.calls.iter().map(|c| c.seq).collect();
        assert_eq!(seqs.len(), 2, "{group:?}");
        assert!(seqs[0] < seqs[1], "each call needs its own position: {seqs:?}");

        // A repaint stamps them consecutively, not with the group's anchor. Use a
        // fresh store: an identical re-paint is a no-op by design.
        let (l2, _e2) = listener();
        let repainted_store = TranscriptStore::new(l2);
        repainted_store.replace_rich(items, false);
        let repainted = repainted_store.rich_transcript();
        let group = repainted
            .iter()
            .find(|i| i.kind == ItemKind::ToolGroup)
            .expect("the run survives the repaint");
        let seqs: Vec<u32> = group.calls.iter().map(|c| c.seq).collect();
        assert_eq!(seqs, vec![0, 1], "a repaint owns the order it built");
    }

    #[test]
    fn a_grouped_delegate_keeps_its_subagent_steps() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.tool_call("shell", "{}", "t1", ToolCallKind::Plain);
        store.tool_call("delegate", "{}", "d1", ToolCallKind::Plain);
        store.tool_step(
            "d1",
            crate::SubagentStep {
                name: "read_file".into(),
                detail: "src/lib.rs".into(),
            },
        );
        let items = store.rich_transcript();
        let group = items
            .iter()
            .find(|i| i.kind == ItemKind::ToolGroup)
            .expect("the delegate joined the run");
        let step_names: Vec<&str> = group
            .calls
            .iter()
            .flat_map(|c| c.steps.iter().map(|s| s.name.as_str()))
            .collect();
        assert_eq!(step_names, vec!["read_file"], "{group:?}");
    }

    #[test]
    fn an_attachment_only_prompt_gets_a_row() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.echo_prompt("", 2);
        let items = store.rich_transcript();
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].kind, ItemKind::User);
        assert_eq!(items[0].attachments, 2);
    }

    #[test]
    fn tool_update_status_only_for_chart_bubbles() {
        let (l, _i_evts) = listener();
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
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.tool_update("ghost", "completed", "out", false);
    }

    #[test]
    fn usage_and_run_ended_are_stream_only() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("agent", "hi", Some("m1"), false);
        store.usage(123, 456, 0.0012, "USD");
        store.run_ended("end_turn");
    }

    #[test]
    fn clear_wipes_transcript_and_resets_stream() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("agent", "one", Some("m1"), false);
        store.tool_call("Bash", "ls", "t1", ToolCallKind::Plain);
        assert_eq!(store.transcript().len(), 2);

        store.clear();
        assert!(store.transcript().is_empty());

        // Stream state reset: the same id now starts a fresh bubble.
        store.append_chunk("agent", "again", Some("m1"), false);
        assert_eq!(
            msgs(&store),
            vec![("agent".to_string(), "m1".to_string(), "again".to_string())]
        );
    }

    #[test]
    fn replace_rebuilds_and_emits_clear_once() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("user", "old", Some("m1"), false);

        store.replace(vec![
            Message {
                id: "m1".into(),
                role: "user".into(),
                content: "hi".into(),
                output: String::new(),
            },
            Message {
                id: "m2".into(),
                role: "agent".into(),
                content: "hello".into(),
                output: String::new(),
            },
            Message {
                id: "t1".into(),
                role: "tool".into(),
                content: "Bash".into(),
                output: String::new(),
            },
            Message {
                id: String::new(),
                role: "thought".into(),
                content: "hmm".into(),
                output: String::new(),
            },
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

        // Stream state reset: a chunk for an already-loaded id starts fresh.
        store.append_chunk("user", "new", Some("m1"), false);
        let t = store.transcript();
        assert_eq!(t.len(), 5);
        assert_eq!(t[4].content, "new");
    }

    #[test]
    fn replace_with_identical_content_is_a_noop() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        let snapshot = vec![
            Message {
                id: "m1".into(),
                role: "user".into(),
                content: "hi".into(),
                output: String::new(),
            },
            Message {
                id: "m2".into(),
                role: "agent".into(),
                content: "hello".into(),
                output: String::new(),
            },
        ];
        store.replace(snapshot.clone());

        // A cold start paints the cache twice (load_cached_transcript, then
        // open_session's fresh path): the second replace of the SAME content
        // must not emit another Clear — that emptied the list and re-painted
        // every row (flicker + recomposition storm).
        store.replace(snapshot.clone());

        // A genuinely different snapshot still rebuilds.
        store.replace(vec![
            Message {
                id: "m1".into(),
                role: "user".into(),
                content: "hi".into(),
                output: String::new(),
            },
            Message {
                id: "m2".into(),
                role: "agent".into(),
                content: "changed".into(),
                output: String::new(),
            },
        ]);
    }

    #[test]
    fn provisional_paint_is_superseded_by_the_first_real_row() {
        // A stale cache is painted provisionally, and the replay APPENDS.
        // Without the drop the transcript holds painted rows followed by
        // replayed rows — not the server's order — which is how a 19-hour-old
        // message came to read as the newest thing on screen.
        let (l, i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.set_session("s1");
        store.replace_provisional(vec![
            Message {
                id: "old2".into(),
                role: "user".into(),
                content: "old prompt".into(),
                output: String::new(),
            },
            Message {
                id: "old1".into(),
                role: "agent".into(),
                content: "19h-old reply".into(),
                output: String::new(),
            },
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
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        let snapshot = vec![
            Message {
                id: "m1".into(),
                role: "user".into(),
                content: "hi".into(),
                output: String::new(),
            },
            Message {
                id: "m2".into(),
                role: "agent".into(),
                content: "hello".into(),
                output: String::new(),
            },
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
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.replace(vec![Message {
            id: "a".into(),
            role: "agent".into(),
            content: "authoritative".into(),
            output: String::new(),
        }]);
        store.append_chunk("user", "live", Some("m2"), false);
        assert_eq!(
            msgs(&store).len(),
            2,
            "a non-provisional snapshot must survive real rows"
        );
    }

    #[test]
    fn tool_rows_also_supersede_a_provisional_paint() {
        let (l, _i_evts) = listener();
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
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        // A stale cache: one row older than the tail, two the replay re-delivers.
        store.replace_for_merge(vec![
            Message {
                id: "old".into(),
                role: "user".into(),
                content: "ancient".into(),
                output: String::new(),
            },
            Message {
                id: "m1".into(),
                role: "user".into(),
                content: "hi".into(),
                output: String::new(),
            },
            Message {
                id: "m2".into(),
                role: "agent".into(),
                content: "hello".into(),
                output: String::new(),
            },
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
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.replace_for_merge(vec![
            Message {
                id: "m1".into(),
                role: "user".into(),
                content: "hi".into(),
                output: String::new(),
            },
            Message {
                id: "t1".into(),
                role: "tool".into(),
                content: "Bash".into(),
                output: String::new(),
            },
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
        let (l, _i_evts) = listener();
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
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.replace_for_merge(vec![
            Message {
                id: "old".into(),
                role: "user".into(),
                content: "ancient".into(),
                output: String::new(),
            },
            Message {
                id: "m1".into(),
                role: "agent".into(),
                content: "hello".into(),
                output: String::new(),
            },
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
            thought: String::new(),
            summary: String::new(),
            steps: Vec::new(),
            seq: 0,
            attachments: 0,
            message_id: String::new(),
        }
    }

    #[test]
    fn rich_items_keep_chart_app_and_toolgroup_kinds() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.tool_call(
            "Chart",
            "input",
            "c1",
            ToolCallKind::Chart {
                spec: r#"{"a":1}"#.into(),
            },
        );
        store.tool_call("A", "x", "g1", ToolCallKind::Plain);
        store.tool_call("B", "y", "g2", ToolCallKind::Plain);
        store.tool_call(
            "Dashboard",
            "{}",
            "a1",
            ToolCallKind::McpApp {
                app_key: "ext|ui://d".into(),
                uri: "ui://d".into(),
                extension: "ext".into(),
                input: "{}".into(),
            },
        );

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
        let (l, i_evts) = listener();
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
                // The tool call ends the text stream: finalize it.
                "upsert:m1:Agent:Hello:",
                "upsert:t1:Tool:Bash:in_progress",
                // Live tool output is a delta…
                "output:t1:out1",
                // …and the completion is the authoritative Upsert that replaces.
                "upsert:t1:Tool:Bash:completed",
            ]
        );
    }

    #[test]
    fn a_turn_end_finalizes_the_streamed_row() {
        let (l, i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("agent", "Hel", Some("m1"), false);
        store.append_chunk("agent", "lo", Some("m1"), false);
        store.run_ended("end_turn");
        // The deltas never carried the completion; run_ended emits it once so a
        // client renders markdown a single time.
        assert_eq!(
            &*i_evts.lock(),
            &[
                "upsert:m1:Agent:Hel:",
                "text:m1:lo",
                "upsert:m1:Agent:Hello:"
            ]
        );
    }

    #[test]
    fn text_row_without_a_message_id_gets_a_stable_synthetic_id() {
        let (l, i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("thought", "hmm", None, true);
        store.append_chunk("thought", " more", None, true);
        // The server gave no id, but the item stream still needs a key: one is
        // assigned and the delta targets it.
        assert_eq!(
            &*i_evts.lock(),
            &["upsert:@1:Thought:hmm:", "text:@1: more"]
        );
        let items = store.rich_transcript();
        assert_eq!(items[0].id, "@1");
        assert_eq!(items[0].text, "hmm more");
    }

    #[test]
    fn a_whitespace_only_message_opens_no_bubble() {
        let (l, i_evts) = listener();
        let store = TranscriptStore::new(l);
        // goose emits the assistant's text as "\n\n" between tool calls; that is
        // not content and must not paint a bare "Goose" bubble.
        store.append_chunk("agent", "\n\n", Some("m1"), false);
        store.tool_call("Bash", "ls", "t1", ToolCallKind::Plain);
        assert_eq!(&*i_evts.lock(), &["upsert:t1:Tool:Bash:in_progress"]);
        assert!(store
            .rich_transcript()
            .iter()
            .all(|i| i.kind != ItemKind::Agent));
    }

    #[test]
    fn intra_message_whitespace_is_kept() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        // Whitespace folded into an open bubble is real spacing, not a message.
        store.append_chunk("agent", "a", Some("m1"), false);
        store.append_chunk("agent", "\n\n", Some("m1"), false);
        store.append_chunk("agent", "b", Some("m1"), false);
        let items = store.rich_transcript();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "a\n\nb");
    }

    #[test]
    fn a_whitespace_only_item_is_dropped_from_a_rich_paint() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.replace_rich(
            vec![
                text_item("m1", ItemKind::Agent, "\n\n"),
                text_item("t1", ItemKind::Tool, "Bash"),
                text_item("m2", ItemKind::Thought, "   "),
                text_item("m3", ItemKind::Agent, "real"),
            ],
            false,
        );
        let items = store.rich_transcript();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, ItemKind::Tool);
        assert_eq!(items[1].text, "real");
    }

    #[test]
    fn a_provisional_live_transcript_is_replaced_by_the_replay() {
        let (l, _e) = listener();
        let store = TranscriptStore::new(l);
        // A live turn lands before the socket drops.
        store.append_chunk("user", "hi", Some("u1"), false);
        store.append_chunk("agent", "old", Some("a1"), false);
        store.tool_call("Bash", "ls", "call_0", ToolCallKind::Plain);
        // The reconnect keeps those rows but arms the provisional drop (goose
        // replays the WHOLE session and re-assigns tool_call_ids from call_0).
        store.mark_provisional();
        store.append_chunk("user", "hi", Some("u1"), false);
        store.append_chunk("agent", "new", Some("a1"), false);
        store.tool_call("Bash", "ls -la", "call_0", ToolCallKind::Plain);

        let items = store.rich_transcript();
        let ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
        // The replay REPLACED the live rows — no duplicated id survives.
        assert_eq!(ids, vec!["u1", "a1", "call_0"]);
        assert_eq!(items[1].text, "new");
        assert_eq!(items[2].text, "Bash");
        assert_eq!(items[2].detail, "ls -la");
    }

    #[test]
    fn synthetic_ids_do_not_collide_with_a_loaded_cache() {
        let (l, _e) = listener();
        let store = TranscriptStore::new(l);
        // A cache from an earlier process carries `@n` ids; `next_uid` restarts
        // at 0, so a fresh id-less chunk must not re-issue one already held.
        store.replace_rich(vec![text_item("@1", ItemKind::Thought, "cached")], false);
        store.append_chunk("thought", "fresh", None, true);
        let items = store.rich_transcript();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "@1");
        assert_ne!(items[1].id, "@1");
        assert_eq!(items[1].id, "@2");
    }

    #[test]
    fn agent_text_emits_sanitized_sentences_to_the_speak_stream() {
        let (l, speaks) = speak_listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("agent", "Here is the plan. It has ", Some("m1"), false);
        store.append_chunk("agent", "`code` in it.", Some("m1"), false);
        store.run_ended("end_turn");
        // The first sentence streams before the turn ends; the flush emits the
        // rest, with inline code stripped.
        assert_eq!(&*speaks.lock(), &["Here is the plan.", "It has in it."]);
    }

    #[test]
    fn user_and_thought_text_are_not_spoken() {
        let (l, speaks) = speak_listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("user", "Do the thing.", Some("u1"), false);
        store.append_chunk("thought", "Thinking about it.", None, true);
        store.run_ended("end_turn");
        assert!(speaks.lock().is_empty());
    }

    #[test]
    fn rich_snapshot_round_trips_through_the_store() {
        let (l, _i_evts) = listener();
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
                thought: String::new(),
                summary: String::new(),
                steps: Vec::new(),
                seq: 0,
                attachments: 0,
                message_id: String::new(),
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
                thought: String::new(),
                summary: String::new(),
                steps: Vec::new(),
                seq: 0,
                attachments: 0,
                message_id: String::new(),
            },
            Item {
                id: "t1".into(),
                kind: ItemKind::ToolGroup,
                text: String::new(),
                detail: String::new(),
                output: String::new(),
                status: String::new(),
                app_key: String::new(),
                seq: 0,
                attachments: 0,
                message_id: String::new(),
                calls: vec![
                    ToolCall {
                        id: "t1".into(),
                        title: "Bash".into(),
                        detail: "ls".into(),
                        output: "x".into(),
                        status: "completed".into(),
                        thought: String::new(),
                        seq: 0,
                        steps: Vec::new(),
                    },
                    ToolCall {
                        id: "t2".into(),
                        title: "Read".into(),
                        detail: "f".into(),
                        output: "y".into(),
                        status: "completed".into(),
                        thought: String::new(),
                        seq: 1,
                        steps: Vec::new(),
                    },
                ],
                thought: String::new(),
                summary: String::new(),
                steps: Vec::new(),
            },
        ];
        store.replace_rich(items.clone(), false);

        // The rebuild is faithful (kind + payload), so a cache paint no longer
        // degrades charts/apps.
        assert_eq!(store.item("c1").unwrap().detail, r#"{"nodes":[]}"#);
        assert_eq!(store.item("a1").unwrap().app_key, "ext|ui://d");
        // The legacy flat projection still works off the same rows.
        assert_eq!(store.item("m1").unwrap().kind, ItemKind::User);

        assert_eq!(store.window().oldest_id, "m1");
        assert_eq!(store.window().newest_id, "t1");
        // A short transcript fits the whole window: nothing older.
        assert!(!store.window().has_older);
    }

    #[test]
    fn a_merge_repaints_the_window_for_the_item_client() {
        let (l, i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.set_session("s1");
        store.replace_for_merge(vec![
            Message {
                id: "old".into(),
                role: "user".into(),
                content: "ancient".into(),
                output: String::new(),
            },
            Message {
                id: "m1".into(),
                role: "agent".into(),
                content: "hello".into(),
                output: String::new(),
            },
        ]);
        // The bounded tail anchors on the cached m1 and re-delivers it.
        store.append_chunk("agent", "hi", Some("m1"), false);
        i_evts.lock().clear();
        assert!(!store.end_merge(), "the tail overlapped the cache");

        // A merge reconciles the store IN PLACE, so the client must be told to
        // rebuild — a bare Reset would leave it empty (there is no getter
        // rebuild on the client anymore).
        let ops = i_evts.lock().clone();
        assert!(ops.first().unwrap().starts_with("reset"), "{ops:?}");
        assert!(ops.iter().any(|o| o.starts_with("upsert:")), "{ops:?}");
        assert!(ops.last().unwrap().starts_with("window:"), "{ops:?}");
    }

    #[test]
    fn a_replayed_prompt_is_recognised_by_its_server_id() {
        // The duplicate-at-the-bottom report, in three steps.
        //
        // 1. A live turn echoes the prompt. goose never ids a prompt we sent, so
        //    the row carries only the core's `@n` key.
        // 2. A reopen's tail re-delivers it with the server's own id. The merged
        //    cache has no id on that row, so the merge can only fall back to
        //    TEXT — and its anchor misses, which is the gap that forces a full
        //    reload (`end_merge` true): correct, expensive, and what kept the
        //    prompts from duplicating only because the caller then repaints.
        // 3. Now the row HAS the server id, so the next reopen anchors by
        //    identity: the tail folds in place instead of reloading.
        let (l, _evts) = listener();
        let store = TranscriptStore::new(l);
        store.set_session("roam:Phaethon:s1");
        store.echo_prompt("huh. I can use the remote", 0);
        store.tool_call("Bash", "ha state", "t7", ToolCallKind::Plain);
        store.append_chunk("agent", "It works because…", Some("m_a1"), false);
        store.run_ended("end_turn");
        let first_paint = store.rich_transcript();

        // Reopen #1: no server id on the prompt, so the tail cannot anchor.
        let one = TranscriptStore::new(listener().0);
        one.set_session("roam:Phaethon:s1");
        one.replace_rich_for_merge(first_paint);
        one.append_chunk("user", "huh. I can use the remote", Some("m_u1"), false);
        assert!(
            one.end_merge(),
            "a tail whose first row the paint cannot name is a gap: full reload"
        );
        // The adopted row now carries the id, and the cache keeps it.
        let prompts = |s: &TranscriptStore| -> Vec<(String, String)> {
            s.rich_transcript()
                .into_iter()
                .filter(|i| i.kind == ItemKind::User)
                .map(|i| (i.text, i.message_id))
                .collect()
        };
        assert_eq!(
            prompts(&one),
            vec![("huh. I can use the remote".to_string(), "m_u1".to_string())]
        );

        // Reopen #2: the cache carries the id, so the tail anchors ON the prompt
        // — the suffix is replaced in place and nothing duplicates.
        let second_paint = one.rich_transcript();
        let two = TranscriptStore::new(listener().0);
        two.set_session("roam:Phaethon:s1");
        two.replace_rich_for_merge(second_paint);
        two.append_chunk("user", "huh. I can use the remote", Some("m_u1"), false);
        two.append_chunk("agent", "It works because…", Some("m_a1"), false);
        assert!(
            !two.end_merge(),
            "the tail overlapped the paint, so the merge folds in place"
        );
        assert_eq!(
            prompts(&two),
            vec![("huh. I can use the remote".to_string(), "m_u1".to_string())],
            "the prompt must appear exactly once"
        );
    }

    #[test]
    fn a_paint_emits_only_the_window_and_load_older_walks_back() {
        let (l, i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.set_session("s1");
        let items: Vec<Item> = (0..100)
            .map(|n| {
                let kind = if n % 2 == 0 {
                    ItemKind::User
                } else {
                    ItemKind::Agent
                };
                text_item(&format!("m{n}"), kind, &format!("t{n}"))
            })
            .collect();
        store.replace_rich(items, false);

        // A paint emits Reset + only the newest CLIENT_WINDOW items + Window.
        let ops = i_evts.lock().clone();
        assert_eq!(ops.first().unwrap(), "reset");
        assert_eq!(ops.iter().filter(|o| o.starts_with("upsert:")).count(), 60);
        assert_eq!(ops.last().unwrap(), "window:m40:true");
        assert_eq!(store.window().oldest_id, "m40");
        assert!(store.window().has_older);

        i_evts.lock().clear();
        store.load_older(20);
        let ops = i_evts.lock().clone();
        assert_eq!(ops.iter().filter(|o| o.starts_with("upsert:")).count(), 20);
        assert_eq!(ops.last().unwrap(), "window:m20:true");
        assert_eq!(store.window().oldest_id, "m20");

        // The store (and so the cache and the flat projection) is NOT truncated.
        assert!(store.window().has_older);

        // Walking to the start: the last load returns nothing older but STILL
        // answers with a Window, so a client's in-flight load settles.
        i_evts.lock().clear();
        store.load_older(1000);
        let ops = i_evts.lock().clone();
        assert_eq!(ops.iter().filter(|o| o.starts_with("upsert:")).count(), 20);
        assert_eq!(ops.last().unwrap(), "window:m0:false");
        assert!(!store.window().has_older);

        i_evts.lock().clear();
        store.load_older(40);
        assert_eq!(
            &*i_evts.lock(),
            &["window:m0:false"],
            "an empty load still answers"
        );
    }

    #[test]
    fn synthetic_item_ids_do_not_leak_into_the_flat_projection() {
        // A live thought row has no server message_id; the flat projection must
        // still report an empty id (the desktop keys history on it), while the
        // rich item carries the synthetic key.
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("thought", "hmm", None, true);
    }

    // -----------------------------------------------------------------------
    // Run summaries (issue #13): the thinking that precedes a tool run frames it
    // -----------------------------------------------------------------------

    #[test]
    fn summarize_takes_the_first_sentence_and_caps_it() {
        assert_eq!(
            summarize("Let me check the calendar. Then reply."),
            "Let me check the calendar"
        );
        assert_eq!(summarize("\n\n  ## Look it up  \nmore"), "Look it up");
        assert_eq!(summarize("   "), "");
        assert_eq!(summarize("No terminator here"), "No terminator here");
        let capped = summarize(&"x".repeat(200));
        assert_eq!(capped.chars().count(), 80);
        assert!(capped.ends_with('…'));
    }

    #[test]
    fn a_thought_frames_the_run_it_precedes() {
        let (l, i_evts) = listener();
        let store = TranscriptStore::new(l);
        i_evts.lock().clear();

        store.append_chunk(
            "thought",
            "Let me check the calendar. Then reply.",
            None,
            true,
        );
        store.tool_call("Bash", "date", "t1", ToolCallKind::Plain);

        // The thinking is absorbed: its own row is folded away and the run carries
        // it, so a long loop reads as one run with the reasoning inside it.
        let ops = i_evts.lock().clone();
        assert!(
            ops.iter().any(|o| o.starts_with("remove:@1")),
            "thought row dropped: {ops:?}"
        );
        assert_eq!(msgs(&store).len(), 1, "only the tool row survives");
        match &store.rows_for_test()[0] {
            Bubble::Tool(r, label) => {
                assert_eq!(label, "Let me check the calendar");
                assert!(r.thought.starts_with("Let me check the calendar"));
            }
            other => panic!("expected a tool bubble, got {other:?}"),
        }
    }

    #[test]
    fn a_thought_that_leads_nowhere_stays_its_own_block() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("thought", "Hmm, let me think about this.", None, true);
        store.append_chunk("agent", "Here is the answer.", Some("m1"), false);

        // Nothing followed the thinking, so it is not folded into anything: it
        // stays a collapsed "Thinking" block above the reply.
        assert_eq!(
            msgs(&store),
            vec![
                // The flat projection reports no id for a synthetic row (the
                // rich item carries `@1`); the desktop keys history on it.
                (
                    "thought".to_string(),
                    String::new(),
                    "Hmm, let me think about this.".to_string()
                ),
                (
                    "agent".to_string(),
                    "m1".to_string(),
                    "Here is the answer.".to_string()
                ),
            ]
        );
    }

    #[test]
    fn the_whole_loop_is_one_run() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("thought", "First, look it up.", None, true);
        store.tool_call("Search", "q1", "t1", ToolCallKind::Plain);
        store.append_chunk("thought", "Now read the page.", None, true);
        store.tool_call("Read", "u1", "t2", ToolCallKind::Plain);

        // A thought does NOT split the run: the model thinking between calls is
        // still the SAME loop, so this is one run with the loop inside it — not
        // one chip per call (issue #13).
        let rows = store.rows_for_test();
        assert_eq!(rows.len(), 1, "one run, got {rows:?}");
        match &rows[0] {
            Bubble::ToolGroup(calls, label) => {
                assert_eq!(calls.len(), 2);
                // The label comes from the first thinking the run saw...
                assert_eq!(label, "First, look it up");
                // ...and each call keeps its own, so the run expands in order.
                assert_eq!(calls[0].thought, "First, look it up.");
                assert_eq!(calls[1].thought, "Now read the page.");
            }
            other => panic!("expected one run, got {other:?}"),
        }
    }

    #[test]
    fn a_visible_message_ends_the_run() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("thought", "Look it up.", None, true);
        store.tool_call("Search", "q", "t1", ToolCallKind::Plain);
        // The model narrates. That narration is a VISIBLE message, and it is where
        // one loop ends and the next begins — thinking does not hop across it.
        store.append_chunk("agent", "Let me check the results.", Some("m1"), false);
        store.tool_call("Read", "u", "t2", ToolCallKind::Plain);

        let rows = store.rows_for_test();
        assert_eq!(rows.len(), 3, "run, narration, run: {rows:?}");
        assert!(matches!(rows[0], Bubble::Tool(..)), "first loop");
        assert!(
            matches!(rows[1], Bubble::Text { .. }),
            "narration stays visible"
        );
        match &rows[2] {
            Bubble::Tool(r, label) => {
                assert_eq!(r.thought, "", "the narration is a boundary");
                assert_eq!(label, "");
            }
            other => panic!("expected a second run, got {other:?}"),
        }
    }

    #[test]
    fn calls_without_a_thought_still_collapse_into_one_run() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.tool_call("Bash", "ls", "t1", ToolCallKind::Plain);
        store.tool_call("Bash", "pwd", "t2", ToolCallKind::Plain);
        match &store.rows_for_test()[0] {
            Bubble::ToolGroup(calls, label) => {
                assert_eq!(calls.len(), 2);
                assert_eq!(label, "", "no framing to draw on");
            }
            other => panic!("expected a toolgroup, got {other:?}"),
        }
    }

    #[test]
    fn a_replayed_thought_frames_the_run_without_a_row_of_its_own() {
        // goose re-sends the reasoning trail on every `session/load`, but a
        // replayed thought has no stable id to reconcile by and must not render
        // as its own row. It is held only as the framing for the run it precedes,
        // so a run's label rebuilds identically from a replay.
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.replay_thought("Look up the charging issue.");
        store.tool_call("Search", "q", "t1", ToolCallKind::Plain);
        store.tool_call("Read", "u", "t2", ToolCallKind::Plain);

        let rows = store.rows_for_test();
        assert_eq!(rows.len(), 1, "no standalone thought row: {rows:?}");
        match &rows[0] {
            Bubble::ToolGroup(calls, label) => {
                assert_eq!(calls.len(), 2);
                assert_eq!(label, "Look up the charging issue");
            }
            other => panic!("expected a toolgroup, got {other:?}"),
        }
    }

    /// The turn-final `[thinking + text]` message: goose sends one `AgentThoughtChunk`
    /// and one `AgentMessageChunk` carrying the SAME message id (both come from
    /// `content_chunk_for_message`, which copies `message.id`). Keyed by that id the
    /// answer used to append into the thinking row, so every refresh showed thinking
    /// and tool calls and then no answer at all.
    #[test]
    fn a_thought_and_the_answer_sharing_one_message_id_stay_separate() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);

        // Live: a thought chunk that carries no id (its message id is not a row
        // identity), then the answer under the message's id.
        store.append_chunk("thought", "weighing it", None, true);
        store.append_chunk("agent", "the answer", Some("m1"), false);
        let items = store.rich_transcript();
        let agent: Vec<&Item> = items.iter().filter(|i| i.kind == ItemKind::Agent).collect();
        assert_eq!(agent.len(), 1, "{items:?}");
        assert_eq!(agent[0].text, "the answer", "the answer is its own row: {items:?}");

        // Replayed: the thought is framing, the answer is still a row under its id.
        let (l2, _e2) = listener();
        let replayed = TranscriptStore::new(l2);
        replayed.replay_thought("weighing it");
        replayed.append_chunk("agent", "the answer", Some("m1"), false);
        let items = replayed.rich_transcript();
        let agent: Vec<&Item> = items.iter().filter(|i| i.kind == ItemKind::Agent).collect();
        assert_eq!(agent.len(), 1, "{items:?}");
        assert_eq!(agent[0].text, "the answer", "the answer is its own row: {items:?}");
        assert!(
            items.iter().all(|i| i.kind != ItemKind::Thought || i.text != "weighing itthe answer"),
            "nothing folded the answer into the thinking row: {items:?}"
        );
    }

    #[test]
    fn a_run_answers_with_its_own_id_and_framing() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("thought", "Look it up. Then read the page.", None, true);
        store.tool_call("Search", "q", "t1", ToolCallKind::Plain);
        store.tool_call("Read", "u", "t2", ToolCallKind::Plain);

        // A run is anchored on its first call, so either call answers with it.
        assert_eq!(store.run_id_for("t1").as_deref(), Some("t1"));
        assert_eq!(store.run_id_for("t2").as_deref(), Some("t1"));
        let (thought, tools) = store.run_framing("t2").expect("framing");
        assert_eq!(thought, "Look it up. Then read the page.");
        assert_eq!(tools, vec!["Search".to_string(), "Read".to_string()]);
    }

    #[test]
    fn a_run_without_thinking_has_no_framing() {
        // Nothing to condense: the inference would be spent for nothing.
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.tool_call("Bash", "ls", "t1", ToolCallKind::Plain);
        assert!(store.run_framing("t1").is_none());
    }

    #[test]
    fn the_model_rewrite_replaces_the_label_and_upserts() {
        let (l, i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("thought", "Let me check the calendar.", None, true);
        store.tool_call("Bash", "date", "t1", ToolCallKind::Plain);
        i_evts.lock().clear();

        store.set_run_summary("t1", "Check the calendar");
        match &store.rows_for_test()[0] {
            Bubble::Tool(r, label) => {
                assert_eq!(label, "Check the calendar");
                // The absorbed thinking is untouched: the run still shows it.
                assert_eq!(r.thought, "Let me check the calendar.");
            }
            other => panic!("expected a tool bubble, got {other:?}"),
        }
        let ops = i_evts.lock().clone();
        assert_eq!(ops.len(), 1, "{ops:?}");
        assert!(ops[0].starts_with("upsert:t1"), "{ops:?}");

        // Idempotent: the same label does not re-announce.
        i_evts.lock().clear();
        store.set_run_summary("t1", "Check the calendar");
        assert!(i_evts.lock().is_empty());
    }

    #[test]
    fn thinking_that_leads_nowhere_does_not_frame_the_next_turn() {
        let (l, _i_evts) = listener();
        let store = TranscriptStore::new(l);
        store.append_chunk("thought", "Just musing.", None, true);
        store.run_ended("end_turn");
        store.tool_call("Bash", "ls", "t1", ToolCallKind::Plain);

        // The turn is over: its trailing thinking is a finished block, not
        // something a later turn's tool call may claim.
        let rows = store.rows_for_test();
        match rows.last() {
            Some(Bubble::Tool(r, label)) => {
                assert_eq!(
                    label, "",
                    "a finished turn's thinking must not label a later run"
                );
                assert_eq!(r.thought, "");
            }
            other => panic!("expected a tool bubble last, got {other:?}"),
        }
        assert!(
            rows.iter()
                .any(|b| matches!(b, Bubble::Text { role, .. } if role == "thought")),
            "and it is still on screen as its own block: {rows:?}"
        );
    }

    #[test]
    fn an_echoed_prompt_is_adopted_when_the_server_names_it() {
        let (l, _) = listener();
        let store = TranscriptStore::new(l);
        store.echo_prompt("what time is it", 0);
        assert_eq!(store.transcript().len(), 1);

        // goose never sends our own prompt live; a replay names it. The named
        // chunk must adopt the echoed row, not append a duplicate.
        store.append_chunk("user", "what time is it", Some("m1"), false);
        let rows = store.rows_for_test();
        assert_eq!(rows.len(), 1, "{rows:?}");
        match &rows[0] {
            Bubble::Text { id, text, .. } => {
                assert_eq!(id, "m1");
                assert_eq!(text, "what time is it");
            }
            other => panic!("expected the prompted row, got {other:?}"),
        }

        // Later chunks of the same message accumulate into it.
        store.append_chunk("user", " (again)", Some("m1"), false);
        assert_eq!(store.transcript().len(), 1);
        assert_eq!(store.transcript()[0].content, "what time is it (again)");
    }

    #[test]
    fn two_echoed_prompts_are_two_rows() {
        let (l, _) = listener();
        let store = TranscriptStore::new(l);
        store.echo_prompt("first", 0);
        store.echo_prompt("second", 0);
        assert_eq!(store.transcript().len(), 2);
        // The oldest echo is adopted first (the server names prompts in order).
        store.append_chunk("user", "first", Some("m1"), false);
        store.append_chunk("user", "second", Some("m2"), false);
        assert_eq!(store.transcript().len(), 2);
    }

    #[test]
    fn a_cached_echo_is_adopted_after_a_paint() {
        let (l, _) = listener();
        let store = TranscriptStore::new(l);
        store.echo_prompt("hello", 0);
        let items = store.rich_transcript();

        let (l, _) = listener();
        let reloaded = TranscriptStore::new(l);
        reloaded.replace_rich(items, false);
        reloaded.append_chunk("user", "hello", Some("m9"), false);
        assert_eq!(reloaded.transcript().len(), 1);
        // The RICH row keeps its stable `@1` id (the client's key); the flat
        // projection now carries the server's message id.
        assert_eq!(reloaded.rich_transcript()[0].id, "@1");
        assert_eq!(reloaded.transcript()[0].id, "m9");
    }
}
