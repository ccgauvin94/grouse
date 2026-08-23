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

use crate::{CoreListener, Message, StreamEvent, ToolCallKind, TranscriptEvent};

/// One tool call in the transcript: a standalone bubble or one entry of a
/// collapsed toolgroup. Mirrors the desktop's tool-row fields.
#[derive(Debug, Clone, PartialEq)]
struct ToolRow {
    title: String,
    detail: String,
    tool_call_id: String,
    output: String,
    status: String,
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
    /// Emits the matching `on_stream` chunk and an `on_transcript`
    /// Append/Update.
    pub fn append_chunk(&self, role: &str, text: &str, message_id: Option<&str>, thought: bool) {
        let (stream_evt, transcript_evt) = {
            let mut st = self.state.lock();
            let fresh = st.stream_idx.is_none()
                || st.stream_role != role
                || (message_id.is_some()
                    && !st.stream_msg_id.is_empty()
                    && message_id != Some(st.stream_msg_id.as_str()));

            let idx = if fresh {
                st.bubbles.push(Bubble::Text {
                    id: message_id.unwrap_or("").to_string(),
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
            st.trim();
            (stream_evt, transcript_evt)
        };

        self.listener.on_stream(stream_evt);
        self.listener.on_transcript(transcript_evt);
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
        let transcript_evt = {
            let mut st = self.state.lock();
            let row = ToolRow::new(title, detail, tool_call_id);
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
            st.trim();

            // Desktop: every tool call resets the streaming anchor so the next
            // chunk opens a fresh bubble.
            st.stream_idx = None;
            evt
        };

        self.listener.on_stream(StreamEvent::ToolCall {
            title: title.to_string(),
            detail: detail.to_string(),
            tool_call_id: tool_call_id.to_string(),
            kind,
        });
        self.listener.on_transcript(transcript_evt);
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
        let transcript_evt = {
            let mut st = self.state.lock();
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
                }
            }
            updated
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
    /// Clear.
    pub fn clear(&self) {
        {
            let mut st = self.state.lock();
            st.bubbles.clear();
            st.tool_by_id.clear();
            st.stream_idx = None;
            st.stream_role.clear();
            st.stream_msg_id.clear();
        }
        self.listener.on_transcript(TranscriptEvent::Clear);
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
        {
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
                return;
            }
            st.bubbles.clear();
            st.tool_by_id.clear();
            st.stream_idx = None;
            st.stream_role.clear();
            st.stream_msg_id.clear();
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
                    }));
                } else {
                    st.bubbles.push(Bubble::Text {
                        id: m.id.clone(),
                        role: m.role.clone(),
                        text: m.content.clone(),
                        thought: m.role == "thought",
                    });
                }
            }
        }
        self.listener.on_transcript(TranscriptEvent::Clear);
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
        ConfigOption, ConnectionStatus, CoreListener, Message, PermissionRequest,
        ProjectSummary, SessionSummary, StreamEvent, ToolCallKind, TranscriptEvent,
    };

    use super::{Bubble, TranscriptStore};

    /// Records the events the store fans out, as plain strings so assertions
    /// stay readable. Events are consumed by value (they are not `Clone`).
    struct TestListener {
        transcript_events: Arc<Mutex<Vec<String>>>,
        stream_events: Arc<Mutex<Vec<String>>>,
    }

    fn listener() -> (Box<dyn CoreListener>, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>) {
        let t = Arc::new(Mutex::new(Vec::new()));
        let s = Arc::new(Mutex::new(Vec::new()));
        let l = TestListener { transcript_events: t.clone(), stream_events: s.clone() };
        (Box::new(l), t, s)
    }

    fn kind_desc(kind: &ToolCallKind) -> String {
        match kind {
            ToolCallKind::Plain => "plain".to_string(),
            ToolCallKind::Chart { spec } => format!("chart:{spec}"),
            ToolCallKind::McpApp { .. } => "mcpapp".to_string(),
        }
    }

    impl CoreListener for TestListener {
        fn on_turn(&self, _state: crate::TurnState) {}
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
        let (l, t_evts, s_evts) = listener();
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
        let (l, t_evts, s_evts) = listener();
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
    fn chart_and_mcpapp_calls_never_collapse() {
        let (l, _t, _s) = listener();
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
        let (l, t_evts, s_evts) = listener();
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
        let (l, _t, _s) = listener();
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
        let (l, t_evts, s_evts) = listener();
        let store = TranscriptStore::new(l);
        store.tool_update("ghost", "completed", "out", false);
        assert!(t_evts.lock().is_empty());
        assert_eq!(&*s_evts.lock(), &["toolupdate:ghost:completed:out:false"]);
    }

    #[test]
    fn usage_and_run_ended_are_stream_only() {
        let (l, t_evts, s_evts) = listener();
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
        let (l, t_evts, _s) = listener();
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
        let (l, t_evts, _s) = listener();
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
        let (l, t_evts, _s) = listener();
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
}
