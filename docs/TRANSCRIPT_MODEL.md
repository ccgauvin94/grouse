# The transcript model

How a conversation is represented in the core and drawn by the clients.

> **One idea:** the core emits a single ordered stream of *rich items*, keyed by
> stable ids, in *windows*. The clients draw items; they never reconcile two
> streams, never rebuild on a "clear", and never re-render the whole history.

**Status.** Phases 1 and 2 are implemented; `core/CONTRACT.md` §3.5 is the
normative surface.

- **Core (phase 1):** `on_item` alongside the legacy events, the rich versioned
  cache, `rich_transcript()` / `item()` / `window()` / `load_older()`, and a
  stale-cache open that merges a bounded `replayTail`.
- **Core windowing:** the store keeps the whole transcript but emits only the
  newest `CLIENT_WINDOW` (60) items on a paint; `load_older` walks the window
  back from memory (no cache read, no wire call). A streamed text row is
  finalized with one authoritative `Upsert` when it ends.
- **Desktop (phase 2):** renders from `on_item` alone — id-keyed upsert /
  append / remove; scroll-back calls `load_older()` and prepends the batch. The
  legacy `on_transcript`/`on_stream` handlers remain only for usage and
  run-ended, and the client-side transcript buffer/reconciliation is gone.
- **Android (phase 3):** the same — `onCoreItem` maps items to `ChatMessage`
  rows keyed by item id (a ToolGroup expands to one `tool` row per call so the
  chat UI keeps grouping them), and the list asks for older items on scroll-back.
- **Every transport, one store (phase 4):** `SessionHub` keeps exactly one
  `TranscriptStore` per display session id, the main WebSocket's and every roam
  peer's alike. A per-store `SessionGate` paints (`on_item`, `on_usage`,
  `on_speak`) only while that key is displayed; a background session accumulates
  silently, is cached, and sets the drawer's green dot via `mark_new`. Both
  wires feed the one `session::dispatch` pipeline, so peer items are built by the
  same code and a peer chart/app survives promotion and restart. A peer differs
  only in dial/framing, reconnect budget, browse list and the
  `roam:<label>:<raw>` id namespace (which is also its cache key). Live text
  mints a stable id, Upserts once, then deltas; a replay that touches a row
  below the painted window is not emitted (the client does not hold it).

**Phase 4 (in progress).** The legacy channels are deleted: `on_transcript`,
`on_stream`, `TranscriptEvent` and `StreamEvent` are gone, and usage /
run-ended — the two things that are not transcript rows — have their own
`on_usage` / `on_run_ended` listener methods. Still to come: the flat `Message`
projection + `transcript()` getter, and the provisional-merge machinery
(removed once replay merges by id in place).

## Why we want to replace the current model

1. **Two overlapping streams.** The core emits `on_stream`
   (chunks/tool calls) *and* `on_transcript` (Append/Update/Clear) for the same
   content. Every client must reconcile them: desktop renders text from one and
   tools from the other; Android carries stashes, deferred commits and
   convert-in-place logic. Most transcript bugs live in that reconciliation.
2. **The canonical projection is lossy.** `Message` keeps text and a title, but
   not the row's kind, nor chart specs, tool inputs, toolgroup children, or
   MCP-app keys. Anything rebuilt from the cache — or the cache itself — degrades
   (an MCP-app dashboard returns as a plain "tool" chip).
3. **Load is all-or-nothing.** `session/load` streams the whole transcript and
   there is no cursor, so "show the recent part first" is impossible; a stale
   cache replays hundreds of messages from the top.
4. **Clients keep their own copies.** Desktop has its own transcript cache and
   does not even redraw from the core's store on `Clear`, so the core's cache
   frequently cannot be used at all.

## Goals

- **One stream.** The client applies ops to a keyed store. No dual-stream
  reconciliation, no stashes, no "clear and rebuild".
- **Full fidelity.** A chart stays a chart and an MCP app keeps its key across
  streaming, replay, cache, and restart.
- **Recent first, older on demand.** Cold open paints the recent window from
  cache; scrolling up extends the window backward.
- **Smooth streaming.** Text grows with O(chunk) appends, not O(message)
  re-emissions.
- **One implementation.** Desktop and Android run the same semantics; the core
  owns order, ids, and windows.

## Non-goals

- Server-side paging beyond a bounded catch-up tail. A stock `goose serve` has no
  history cursor; older-than-cache falls back to a full load (see *Windowing*).
- Changing the ACP wire. This is entirely a core/client model.

## The model

### Items

An **Item** is the only unit the client knows. It is self-describing and
round-trips through the cache.

| kind | text | detail | output | app_key | calls | thought / summary |
|---|---|---|---|---|---|---|
| user / agent / thought / error | body | — | — | — | — | — |
| tool | title | tool input | tool result | — | — | the run's framing |
| toolgroup | — | — | — | — | children | the run's framing |
| chart | title | chart spec | — | — | — |
| mcpapp | title | app input | — | `<ext>\|<uri>` | — |

`id` is the server's `message_id` for text, or the `tool_call_id` for tool-ish
rows. A `toolgroup`'s id is its first child's id.

**The core echoes the prompt it sends.** goose does not send a
`user_message_chunk` for a prompt *we* sent — the prompt next crosses the wire in
a later `session/load` replay (verified against a live serve). So without a local
copy the transcript, and the cache saved from it, holds no record of what was
asked, and a reopen that trusts the cache shows the replies with the questions
missing. `spawn_prompt` therefore appends the prompt to the store as a `user`
item — its text when it has one, and an empty caption for an attachment-only
prompt. The item carries only the attachment COUNT (`attachments`); the client
matches its own pixels to the row by that count. Gate the match on the count: a
later text-only prompt can share its text with an earlier attachment prompt and
must not inherit its pixels.

A `Tool` row may also carry `steps`: the child tool calls a SUBAGENT made under
a delegate (goose forwards them as `_meta.toolNotification`; subagent sessions
are not ACP-visible). A `ToolCall` inside a group carries its own `steps` for the
same reason — a delegate often joins a run.

**`seq` is the transcript position.** Clients MUST insert by it, never by
arrival: a prompt echo can land after the reply it prompted, and a page of older
rows drains in after newer ones. A `toolgroup` is one item but several client
rows, so each `ToolCall` carries its own `seq`; a wholesale paint re-stamps the
whole list in order.

A whitespace-only agent/thought message is **not** an item: goose emits the
assistant's text as `"\n\n"` between tool calls, and the core drops a chunk that
would open a bubble with only whitespace (whitespace folded into an already-open
bubble is kept — that is real intra-message spacing). Cache paints and replays
drop such rows too, so a stale cache cannot re-create an empty bubble.

### Step summaries

A run of consecutive tool calls collapses into a chip, and a chip labelled with a
bare count — "11 tool calls" — says nothing about what the agent was doing. So a
run is **one think → act STEP**: a burst of thinking and the calls it ran. A turn
therefore reads as its intentions in order — "Looking up the game time",
"Checking your calendar", then the answer — instead of one chip swallowing the
whole loop.

- **`ToolCall.thought`** — the thinking that immediately preceded that call,
  absorbed from the `Thought` item it used to render as (which is dropped with a
  `Remove`). A new step opens on it.
- **`Item.thought`** — the step's thinking, joined (what the label was drawn from).
- **`Item.summary`** — the step's one-line label, from the thinking that opened it
  (first sentence, capped). Empty when the step has nothing to draw on, and the
  client then falls back to the call titles.

A thought therefore **does** split a run. Calls that share a step — parallel
calls with nothing thought between them — collapse into it (that is one decision,
so one chip). A **visible message** also ends a step: the model's own narration,
or your next prompt, is where one loop stops and the next begins. Thinking that
leads nowhere (no call follows before the turn ends) stays a `Thought` item of its
own, and the turn's end seals it so a later turn's call can never claim it.

This is exactly the shape goose emits: a burst of thinking, a whitespace-only
agent message (`"\n\n"`, dropped — see above), then the call, repeated. Because
those agent chunks are empty they do not break a step; a real narration does.

(Issue #13 collapsed the whole loop into one run when a count was all a chip
showed. With a label per step the loop reads better as its steps, so the rule is
one chip per thinking burst.)

**The rewrite.** The label above is *provisional*. At the end of a turn the core
asks the model — on a **hidden session** (`_meta.hidden`, so goose's unlistable
`hidden` session type keeps it out of `session/list` and it can never appear as a
chat) — for one short line per step, in the register of an INTENTION ("Looking up
the game time", not "The user is asking what time…"), and the answer replaces the
label with an `Upsert`. A turn's steps are capped (`MAX_SUMMARIZED_STEPS`, 40) so
an unusually long loop cannot turn this into a large request; steps past the cap
keep their provisional label. It is one small inference per turn, sent after the turn is over (so it
never delays a reply), skipped when the client turns it off
(`set_run_summaries`), and skipped for a run with no thinking to condense.
Wherever the model does not answer, the provisional label stands. The hidden
session is emptied after every request
(`_goose/unstable/session/conversation/truncate`) so its context cannot grow
turn on turn.

**Replay.** goose re-sends the reasoning trail on every `session/load`, but a
standalone replayed thought would double the trail and carries no stable id to
reconcile by (a merge cannot match it). It is therefore held only as the thinking
of the call that follows it, never emitted as a row: a run's label is *derived
from the replayed rows*, never persisted against a `tool_call_id` (which the
server re-assigns on every replay). Both fields round-trip through the cache, so
a reloaded chat keeps its labels and its loops.

### Ops

The core emits **one** op stream (`CoreListener::on_item`), replacing both
`on_stream` and `on_transcript`:

```
Reset   { session_id }          // the window is being replaced wholesale
Upsert  { item }                // insert or replace by id (system of record)
AppendText   { id, chunk }      // O(chunk) live text
AppendOutput { id, chunk }      // O(chunk) live tool output
Remove  { id }
Window  { oldest_id, has_older } // the client's pagination cursor
```

Rules:

- **Upsert is idempotent and authoritative.** Every item is eventually Upserted
  with its final state; deltas are an optimization. A client that drops a delta
  still converges on the next Upsert.
- **Unknown id on a delta** creates a stub, then the Upsert fills it. (Or the
  core guarantees an Upsert precedes deltas; pick one and test it.)
- **Ops for items outside the client's window are ignored.** The core keeps
  them; they appear if the window grows.
- **Reset** happens only on a session switch or a cache miss that forces a full
  reload.

### Speaking (the TTS stream)

Alongside `on_item`, the core emits `on_speak(text)` — a sanitized,
sentence-sized chunk of the assistant's **visible** text, ready for a TTS engine.
It is derived from the same text that feeds the agent bubbles, but:

- code fences and inline code are stripped (never spoken),
- markdown links read their text, images their alt,
- headings / bullets / emphasis markers are removed,
- chunks are cut at sentence boundaries *as the reply streams*, so speech can
  start before the turn ends; `on_run_ended` flushes the remainder.

One implementation lives in the core (`speak.rs`), so desktop and Android agree
on what gets spoken. Playback is the client's — an on-device engine today, an
optional streaming TTS backend later (the voice workstream). The stream is
independent of the transcript: a client that is not narrating a live turn simply
ignores it.

### Windows

The core holds the full session transcript, but emits only a window of the most
recent `N` items (default 50) plus any earlier windows the client has asked for.

- `window()` → `{ oldest_id, newest_id, has_older }`.
- `load_older(count)` → the core emits the next `count` items behind
  `oldest_id`, oldest-first, then a refreshed `Window`.
- Windowing is **cache-first**: earlier items come from the local cache. If the
  cache does not reach the start of the session, the core reports
  `has_older: false` and the client may request a full load (the one remaining
  all-or-nothing path, and the only one a stock server forces).

Cold open:

1. `Reset` + the cached recent window as Upserts + `Window`.
2. A `session/load` catch-up, reconciled by id: overlapping items are Upserted
   (authoritative), new items Upsert in order.
3. New live ops as they arrive.

**The catch-up is ingested SILENTLY** (grouse#18). While the replay is in flight
the store takes every row — transcript, cache and window all end up complete —
but emits nothing; the load's completion paints the newest `CLIENT_WINDOW` items
once (`Reset` + Upserts + `Window`). Painting each replayed row as it arrived made
a reopen/refresh re-render the whole history from the first prompt, one row at a
time, and left the newest messages below the fold. A failed load releases the
hold *without* painting, so the provisional cache paint stays on screen (see
`open_session`'s contract in `core/CONTRACT.md` §3.2).

**A stock goose ignores `_meta.replayTail` and replays the WHOLE session, and it
re-assigns `tool_call_id`s from `call_0` on every replay.** A replayed row's id
therefore does NOT reliably match the live row it restates, so a replay must
always REPLACE the rows already held, never append onto them: appending
duplicates every replayed row and collides tool ids. A cache paint is
provisional (dropped wholesale by the first replayed row), and the reconnect
path arms that same drop — the store holds the LIVE transcript across a drop,
not a merge base, so there is nothing to merge into. (The `merge` mode still
exists for a painted stale cache, but against this server the anchor is the
first replayed row, so it is a full rebuild in practice.)

## Types (sketch)

```rust
// uniffi records/enums on the contract; the C ABI carries the same shape as JSON.
pub enum ItemKind { User, Agent, Thought, Tool, ToolGroup, Chart, McpApp, Error }

pub struct ToolCall {
    pub id: String, pub title: String, pub detail: String,
    pub output: String, pub status: String,
    pub thought: String,  // the thinking that preceded THIS call
}

pub struct Item {
    pub id: String,
    pub kind: ItemKind,
    pub text: String,     // text body / tool title
    pub detail: String,   // tool input / chart spec / app input
    pub output: String,   // tool result
    pub status: String,   // tool lifecycle
    pub app_key: String,  // mcpapp: "<ext>|<uri>"
    pub calls: Vec<ToolCall>, // toolgroup children
    pub thought: String,  // tool/toolgroup: the thinking the run absorbed
    pub summary: String,  // tool/toolgroup: the run's one-line label
}

pub enum TranscriptOp {
    Reset { session_id: String },
    Upsert { item: Item },
    AppendText { id: String, chunk: String },
    AppendOutput { id: String, chunk: String },
    Remove { id: String },
    Window { oldest_id: String, has_older: bool },
}
```

Contract surface:

- Listener: `on_item(op)`.
- Getters: `window()`, `item(id)`.
- Intents: `open_session(id)`, `load_older(count)`, `flush_caches()`.

## Caching

The cache becomes the rich item list, versioned, per session: `{ v, updatedAt,
items: [Item] }`. It is the source for

- cold-start paint of the recent window, and
- `load_older` when the cache reaches back far enough.

**A cache is only trusted as a *settled* revision.** A transcript saved while a
turn is in flight — or after it ended in error because the wire dropped
mid-stream — is written with an EMPTY `updatedAt`, so it can never compare equal
to the server's stamp and suppress the replay that would complete it. A
settled-but-stale cache merges a bounded tail; an incomplete one takes a full
replay (its rows are painted provisionally and dropped wholesale). `cache v4`
exists because a v3 file could carry a poisoned stamp over a truncated
transcript. `refresh_session(id)` forces the full replay on demand (the clients'
"reload chat" action).

Because items carry their kind and payload, restart fidelity is total: charts,
MCP apps, and tool groups survive untouched. The desktop's separate transcript
cache is deleted.

The cache key is the display session id: a main-wire chat caches under its bare
`sessionId`, a peer chat under `roam:<label>:<raw>`. The save policy is the
core's alone (`Core::save_cache_for`, reached by a peer through
`DispatchHost::cache_session`), and the paint decision is
`session::plan_paint` for both — so a peer chat cold-opens from its cached items
exactly like a main one instead of replaying its whole history over the wire.

## Threading and performance

- The core assigns no global order; order is the client's insertion order (new
  ops append, `load_older` prepends). Deltas are applied to the id's buffer.
- **Coalesce text/output deltas** (e.g. ~50 ms or every N chunks) so a fast
  model cannot flood the UI thread with ops; the final Upsert is exact.
- The core never re-emits the whole window for a single change; one edit is one
  op.
- The clients realize only what is on screen (desktop `ListView` with a bounded
  `cacheBuffer` and `reuseItems`, Android `LazyColumn`). The desktop used to keep
  every delegate realized to stabilise `contentHeight`; each `load_older` batch
  prepends 40 rows, so that grew the live set for the life of a session. Rows are
  recycled instead, and the view re-pins itself until the list reports `atYEnd`
  (the heights of pooled rows are estimates, not measurements).

## Client shape

Both clients reduce to the same small store:

```
items: Map<id, Item>
order: [id, ...]        // newest at the end
apply(op):
  Reset        -> items.clear(); order.clear()
  Upsert(i)    -> if i.id in items: items[i.id] = i
                  else: items[i.id] = i; order.push(i.id)
  AppendText   -> items[id].text += chunk
  AppendOutput -> items[id].output += chunk
  Remove(id)   -> items.remove(id); order.remove(id)
  Window       -> hasOlder = w.has_older; oldestId = w.oldest_id
render:
  for id in order: draw item by kind
onScrollNearTop: Mgr.loadOlder(50)   // once per Window, not per pixel
```

- **Desktop:** `MessageListModel` → `ItemModel` (upsert/remove/prepend). Tool
  groups, charts and MCP apps are just kinds in the delegate. The current
  text-from-transcript / tools-from-stream split disappears, as do the manager's
  convert-in-place and stash paths.
- **Android:** identical; `LazyColumn(reverseLayout)` triggers `loadOlder` at
  the top. `McpAppView`, `ChartView`, `ToolChipGroup` become kind renderers with
  no change to their internals.

### What the client-facing behaviours become

- *Recent-first open*: the cached window paints synchronously; the wire tail
  merges in. No top-to-bottom replay.
- *Scroll-back history*: `load_older`; items are prepended, scroll position is
  preserved (anchor by `oldest_id`).
- *Streaming*: `AppendText`/`AppendOutput` grow the item in place.
- *Charts / MCP apps / tool groups*: first-class kinds, identical live, replayed,
  and restored from cache.
- *Pinning / docking*: unchanged — the dock renders an item by kind.

## Migration

Each phase is shippable; the old path stays until the last client moves.

1. **Core API alongside.** Add `Item`, `TranscriptOp`, `on_item`, `window()`,
   `item()`, `load_older()`; build items from the existing bubbles; make the
   cache rich + bump `TRANSCRIPT_CACHE_VERSION`. The old `on_stream` /
   `on_transcript` / `Message` still drive today's clients.
2. **Desktop.** Replace `MessageListModel` with `ItemModel`; render kinds; wire
   `load_older`; delete the desktop transcript cache and the stream/transcript
   reconciliation in `manager.cpp`.
3. **Android.** Same store + `LazyColumn`; delete the stash/deferred-commit
   logic in `ConnectionManager`.
4. **Delete the old surface.** Remove `on_stream`/`on_transcript`, the flat
   `Message` projection, and the provisional/supersede/anchor machinery; update
   `core/CONTRACT.md` and `core/grouse-core/INTERNAL.md`.

Every transport carries the same items through the same store: `spine::Conn`
(WebSocket) and `roam::RoamPeer` (iroh) both hand their notifications to
`session::dispatch` and share one `SessionHub` store per display id, so a peer
needs no item code of its own and the merge/dedupe rules are "Upsert by id" once.

## Testing

- **Core:** op-sequence tests — open emits Reset + window + Upserts; a live turn
  emits Upsert then deltas then a final Upsert; `load_older` prepends exactly the
  requested window and updates `has_older`; a rich cache round-trips charts,
  MCP apps, and tool groups byte-for-byte.
- **Spine e2e (scripted server):** a stale cache asks for a bounded tail, merges
  by id, and never sends a full replay; a cache that cannot reach the start
  reports `has_older: false` and falls back to a full load.
- **Desktop:** `tst_itemmodel` (apply ops, order, prepend, remove).
- **Android:** an `ItemStoreTest` mirroring it.

## Open questions

- **Delta before Upsert?** Decide whether the core guarantees an Upsert precedes
  deltas for an id, or clients must create stubs. (Recommend the guarantee.)
- **Coalescing rate.** Fixed ~50 ms timer vs. "every K chunks" vs. adaptive.
- **`has_older: false` UX.** A "Load entire conversation" button, or refuse to
  scroll past what is cached and say so?
- **Compaction.** If the server rewrites ids on compact, treat it as a `Reset`
  (new transcript), not a merge.
- **Export.** Serializes the full item list (superset of today's `Message`).
