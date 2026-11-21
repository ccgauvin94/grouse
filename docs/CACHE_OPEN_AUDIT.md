# Audit — opening and caching a chat, especially over roam (2026-11-21)

Four independent read-only investigations (cache writes, the core's open path, the
clients, the roam wire) plus a re-read of the code by hand. This records what is
true, what was wrong, what was changed, and what is deliberately still open.

The report that started it:

> if I open a roam chat, send a message, let the turn reply some, open a different
> chat, and then go back to the original, sometimes it wants a full reload
> (shouldn't need to at all but even if it should only be the delta) and then the
> full reload doesn't contain my recent messages and the turns reply.

and, later the same day:

> sometimes if I sit there and refresh by hand I get a different checkpoint every
> time lol. it cycles between recent turns and in progress turns etc.

---

## 1. The invariant the code is written around

`Core::open_session` → `plan_cache_paint` → **paint the cache, then ask the wire for
history, and let the replay's rows replace what it covers.** `PaintPlan` has three
modes (`session.rs::plan_paint`):

| cache state | mode | `_meta.replayTail` |
|---|---|---|
| settled, stamp == the server's | suppress (rows discarded) | 1 |
| settled, stamp differs | merge (cache is the anchor's base) | 100 |
| empty stamp (saved mid-turn) or `incomplete` | provisional paint, replay replaces | `max(80, store.len() + 80)` |
| no cache file | nothing to paint | 80 |
| `RefreshMode::Full` | cache file deleted, store cleared | 80 |

**The whole design rests on one premise: the server's copy of a chat is a
superset of the client's.** Every mode above is a way of reconciling toward that.

## 2. Where that premise is false — and what it cost

**goose persists a message when it FINISHES** (`agents/state_machine/session.rs`,
`AppendMessage → add_message`); only the user's prompt is written up front
(`agents/services.rs`). So for the entire duration of a turn, the server's copy
is missing the reply being streamed.

Three consequences, all observed:

1. **A mid-turn open replayed a snapshot that cannot contain the reply, and the
   replay deleted the live rows.** `plan_paint` sees `incomplete` (set at prompt
   send, `lib.rs`), so the plan is provisional; `apply_paint` → `replace_rich(items, true)`;
   the first replayed row calls `supersede_provisional` → `store.reset()`, which
   drops the partial reply *and* clears `stream_msg_id`/`stream_idx`. The live
   stream then opens a **new bubble mid-sentence**. That is "the reply is gone" and
   "the reply starts a paragraph in", in one mechanism.
2. **`asked = max(tail, store.len() + OPEN_TAIL)`** (`session.rs::replay_held`). For
   every non-suppress, non-merge open — i.e. every mid-turn reopen, and every
   refresh — the ask is *the number of rows the store already holds, plus 80*. A
   reopen of a busy chat therefore re-downloads the whole conversation, which is
   what "it wants a full reload" was. (This also contradicts `CONTRACT.md`, which
   promised a bounded 80-message tail; the code comment explains why the bound
   cannot simply be honoured while the replay *replaces* the store.)
3. **`incomplete` is sticky.** It is set on send, on `turn_lost`, and on a
   disconnect; it is cleared by a clean `finish_turn` — but `finish_turn` returns
   **before** touching it when the turn is no longer in `in_flight` (the
   `IGNORED` branch, which is exactly what happens when `turn_lost` released the
   turn and the reply arrived afterwards). A successful load also could not settle
   it: `save_cache_for` stamps the cache with an empty `updatedAt` while the flag is
   set, so the next open took the provisional branch again — forever, for that
   chat.

## 3. The change

**A session with a turn in flight is never reconciled from the server.** New fourth
mode, `PaintPlan::bind_only()`:

- `apply_paint` returns immediately — the store is not painted, not cleared, and
  its cursor (`server_more`) is left alone;
- the load is **not** run through `verified_ask` — verification exists to turn a
  wrong suppression into a *merge*, and a merge would anchor on the live rows and
  truncate them;
- the ask is `ReplayAsk::bind_only()`: `suppress: true, tail: 1`. The replay is
  discarded, and the session is still bound on the wire (so prompts route and
  config comes back).

Call sites:

| path | change |
|---|---|
| `open_session_inner` (main) | if `in_flight` → `store.repaint_window()` + bind-only plan; the live-reuse load takes a `bind_only` flag |
| `roam::load_session` (peer opens, peer **reconnects**, peer refreshes) | same, and the client's rows come back through `repaint_window` |
| `refresh_session(Full)` | a turn in flight **skips** the cache `forget` + `store.clear()` (it cannot improve on the store, and clearing is what loses the reply), then takes the same path |

Plus two smaller corrections:

- `finish_turn` clears `incomplete` on a **clean** end even when the turn was
  already released — the reply winning a race against `turn_lost` now settles the
  chat instead of leaving it permanently provisional.
- A successful load clears `incomplete` when no turn is in flight
  (`DispatchHost::clear_incomplete`), so the cache can be stamped settled and the
  chat stops replaying itself on every open.

And one for the "different checkpoint every time" report: `RoamPeer::save_open_transcript`
now **refuses to save a store that is mid-replay**. It used to run at the top of
`load_session`, so a save that landed while a previous replay had rows in flight
cached whatever prefix had arrived; the next plan then treated that prefix as the
chat and merged behind it. Repeated manual refreshes could therefore land on a
different "checkpoint" each time.

## 4. Verified

- `session::tests::a_bind_only_load_keeps_the_live_rows_and_asks_for_one_message`:
  a store holding a prompt and a partial reply, a wire that only answers
  `session/load` (a probe fails the test) → the store comes back byte-identical,
  and the tail asked for is 1.
- The full core suite (300 unit + 23 spine + roam + unstable) and
  `clippy -D warnings`.
- Not verified on hardware yet: the phone-side flow needs the core rebuilt into
  `libgrouse_core.so` (Android libs + APK) and a live roam chat.

## 5. Deliberately not changed (open items)

1. **The main-wire handshake replay.** `spine.rs` replays on (re)connect through
   `replay_session(suppress, merge)` with the flags the open stored. A reconnect
   while a turn is in flight is the same hazard as an open, and is *not* covered by
   this change — it needs the same `indistinguishability` decision at the handshake
   (a `keep` flag on `ConnectSpec`). Roam reconnects *are* covered, because they
   resume through `roam::load_session`.
2. **`asked = store.len() + OPEN_TAIL` for the cold provisional open.** With the
   in-flight case gone, this is reached only when the cache carries an empty stamp
   and no turn is running. It then genuinely owes a replay big enough to replace
   the paint, so the ask stays — but it means one full re-download of that chat.
   The real fix is a replay that MERGES by id instead of replacing the store, which
   is a larger change to `TranscriptStore`.
3. **`RefreshMode::Full` is a bounded rebuild, not a full replay**, despite its
   docstring: it clears the store and asks for `OPEN_TAIL` (80), relying on the
   client's `drainingHistory` to walk the rest back in. Two failed pages and the
   chat stays at one window. Deliberate (grouse#18's recent-first open) but the
   docstring should not say "full".
4. **`RoamCodec::MAX_FRAME_BYTES` (1 MiB) silently truncates an oversized frame**
   and the SDK drops it as malformed. A single >1 MiB tool result (or long stored
   message) is therefore a *hole* in a replay, with no signal to the core, the peer
   or the user. Not the cause of this report (a hole, not a truncated tail), but it
   is the one wire defect that fails silently by construction.
5. **`open_on_peer` passes the MAIN wire's cwd** to a peer session (and
   `refresh_session` passes an empty one, resolved differently). goose rewrites
   `working_dir` and can drop the provider when they differ — a silent mutation per
   tap on a peer chat whose cwd is not the main config's.
6. **Client-side, unchanged and worth fixing separately**: the Android client
   `messages.clear()`s before calling the core (so any failed/half open shows an
   empty chat), drops `AppendText` deltas for ids it does not hold (the documented
   stub contract says create one), and never resets `itemHasOlder` on `Reset`.
