# The transcript model

How a conversation is represented in the core and drawn by the clients.

> **One idea:** the core emits a single ordered stream of *rich items*, keyed by
> stable ids, in *windows*. The clients draw items; they never reconcile two
> streams, never rebuild on a "clear", and never re-render the whole history.

**Status.** Phase 1 (the core API alongside the legacy events, plus the rich,
versioned cache) is implemented — `core/CONTRACT.md` §3.5 is the normative
surface. The item stream (`on_item`), `rich_transcript()` / `item()` /
`window()`, and `load_older()` all exist; `on_transcript` / `on_stream` /
`Message` still drive today's clients until the platform migrations land
(phases 2–4 below). Windowing is not yet enforced on the paint path.

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

| kind | text | detail | output | app_key | calls |
|---|---|---|---|---|---|
| user / agent / thought / error | body | — | — | — | — |
| tool | title | tool input | tool result | — | — |
| toolgroup | — | — | — | — | children |
| chart | title | chart spec | — | — | — |
| mcpapp | title | app input | — | `<ext>\|<uri>` | — |

`id` is the server's `message_id` for text, or the `tool_call_id` for tool-ish
rows. A `toolgroup`'s id is its first child's id.

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
2. A bounded `session/load` catch-up (`_meta.replayTail: N`), merged into the
   window by id: overlapping items are Upserted (authoritative), new tail items
   Upsert in order.
3. New live ops as they arrive.

A stale cache therefore costs a bounded tail on the wire, never a full replay,
and the older window stays available locally.

## Types (sketch)

```rust
// uniffi records/enums on the contract; the C ABI carries the same shape as JSON.
pub enum ItemKind { User, Agent, Thought, Tool, ToolGroup, Chart, McpApp, Error }

pub struct ToolCall {
    pub id: String, pub title: String, pub detail: String,
    pub output: String, pub status: String,
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

Because items carry their kind and payload, restart fidelity is total: charts,
MCP apps, and tool groups survive untouched. The desktop's separate transcript
cache is deleted.

## Threading and performance

- The core assigns no global order; order is the client's insertion order (new
  ops append, `load_older` prepends). Deltas are applied to the id's buffer.
- **Coalesce text/output deltas** (e.g. ~50 ms or every N chunks) so a fast
  model cannot flood the UI thread with ops; the final Upsert is exact.
- The core never re-emits the whole window for a single change; one edit is one
  op.
- The clients realize only what is on screen (desktop `ListView`, Android
  `LazyColumn`); there is no `cacheBuffer: 20000` equivalent.

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

Roam peers carry the same items: `RoamPeer`'s `Vec<Message>` becomes the item
list, and the merge/dedupe code collapses into "Upsert by id".

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
