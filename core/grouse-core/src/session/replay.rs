// SPDX-License-Identifier: AGPL-3.0-or-later

//! `session/load` paint policy and replay lifecycle, shared by both transports.
//!
//! The paint decision, its verification against the server, and the held replay
//! that lands one window live in this module; transports only supply an
//! [`super::ChatEndpoint`] and a [`super::DispatchHost`].

use super::*;

/// Trailing messages requested from `session/load` for a stale-but-settled
/// cache. Counted in server-side messages (tool requests/responses included);
/// the server widens to the nearest turn boundary. Large enough that the
/// replayed window always overlaps the cached tail it merges against.
pub(crate) const REPLAY_TAIL: usize = 100;

/// Trailing messages a FIRST load asks for when there is no cache to paint
/// (docs/TRANSCRIPT_MODEL.md, "Windows": recent first, older on demand).
///
/// This used to be unbounded — no `_meta.replayTail` at all — so opening a chat
/// this device had not seen streamed its entire history, held it silently, and
/// painted 60 rows at the end (grouse#18's single windowed paint). Measured on
/// the Assistant thread: 4,221 messages before the first row appeared. Bounded,
/// the newest turn paints in one round trip and the rest arrives while the user
/// reads it, a page at a time, when it is asked for.
pub(crate) const OPEN_TAIL: usize = 80;

/// The tail a load owes for a paint mode. `0` means the whole session (a full
/// rebuild that must see everything, e.g. a resync after a remote change).
pub(crate) fn first_tail(suppress: bool, merge: bool) -> usize {
    if suppress {
        1
    } else if merge {
        REPLAY_TAIL
    } else {
        OPEN_TAIL
    }
}

/// What one `session/load` owes: the two paint gates and the size of the window
/// it asks the wire for. Grouped because they are read as one decision — a
/// caller that sets `suppress` but a full `tail` (or the reverse) is a bug, and
/// a value makes that visible at one site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReplayAsk {
    /// The cache is current and authoritative: discard the replayed rows.
    pub suppress: bool,
    /// The cache is the merge BASE for the replayed tail.
    pub merge: bool,
    /// Trailing messages to request (`0` = the whole session).
    pub tail: usize,
}

impl ReplayAsk {
    /// The ask implied by a paint mode: a fresh cache needs one message (only to
    /// bind the session), a stale one needs the overlapping tail, and nothing to
    /// paint owes the last [`OPEN_TAIL`] messages.
    pub(crate) fn for_mode(suppress: bool, merge: bool) -> Self {
        Self {
            suppress,
            merge,
            tail: first_tail(suppress, merge),
        }
    }

    /// The ask for a session whose STORE is already the newest copy: bind the
    /// session on the wire with the smallest legal replay and discard it. One
    /// message is enough to bind — the server widens it to the last turn, and a
    /// suppression drops every row of it.
    pub(crate) fn bind_only() -> Self {
        Self {
            suppress: true,
            merge: false,
            tail: 1,
        }
    }
}

/// Confirm a "the cache is current, throw the replay away" decision against the
/// server before honouring it, and turn it into a bounded merge when it is wrong.
///
/// A suppression is the ONE load that discards what the wire sends, so it can
/// never learn it was wrong. `plan_paint` decides it from the cache's stamp and
/// the baseline the core last saw in `session/list` (or a `session_info_update`)
/// — and that baseline does not move when ANOTHER client writes to the session:
/// nothing obliges the server to notify us, and a roam peer only re-lists on a
/// reconnect or an explicit relist. So the everyday "refresh" (`RefreshMode::
/// Quick`) compared a saved stamp with a stale copy of the same stamp, called
/// the cache fresh, dropped the replay, and fetched nothing — a refresh that
/// silently does nothing, worst on a chat another client (Desktop, the CLI, a
/// scheduled run, the assistant's thread append) is actively writing to.
///
/// One `session/info` probe settles it. Cheap, and only on the suppress path:
/// a cache that already knows it is stale takes the merge directly.
pub(crate) async fn verified_ask(
    endpoint: &dyn ChatEndpoint,
    host: &dyn DispatchHost,
    key: &str,
    ask: ReplayAsk,
) -> ReplayAsk {
    if !ask.suppress {
        return ask;
    }
    let params = json!({ "sessionId": endpoint.wire_id(key) });
    let probed = request(endpoint, "_goose/unstable/session/info", params)
        .await
        .ok()
        .and_then(|reply| {
            reply
                .pointer("/session/updatedAt")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let listed = host.listed_stamp(key);
    let ask = ask_with_stamp(ask, &listed, probed.as_deref());
    // The server has moved on since the baseline the paint plan was built from,
    // so record the truth: the next plan reads this baseline instead of the
    // stale one that produced the (wrong) "fresh" verdict.
    if !ask.suppress {
        if let Some(stamp) = probed.as_deref().filter(|stamp| !stamp.is_empty() && *stamp != listed)
        {
            host.session_stamp(key, stamp);
        }
    }
    ask
}

/// The decision half of [`verified_ask`], pure so it can be read and tested
/// without a wire: `probed` is the server's own `updatedAt` (`None` when the
/// probe failed or the reply carried no stamp).
pub(crate) fn ask_with_stamp(ask: ReplayAsk, listed: &str, probed: Option<&str>) -> ReplayAsk {
    if !ask.suppress {
        return ask;
    }
    let Some(stamp) = probed.filter(|stamp| !stamp.is_empty()) else {
        // A probe that answers nothing (or fails) is not evidence the cache is
        // stale: keep the suppression rather than repaint a chat on a hiccup.
        return ask;
    };
    if !listed.is_empty() && listed == stamp {
        return ask;
    }
    ReplayAsk {
        suppress: false,
        merge: true,
        tail: REPLAY_TAIL,
    }
}

// ---------------------------------------------------------------------------
// Cache paint policy (one decision for both transports)
// ---------------------------------------------------------------------------

/// How to paint a session's cached transcript and which wire load it owes.
pub(crate) struct PaintPlan {
    /// The cache is a settled, up-to-date copy: paint authoritatively and
    /// suppress the replay.
    pub suppress: bool,
    /// The cache is settled but stale: paint and merge a bounded tail.
    pub merge: bool,
    /// The STORE is already the newest copy of this session (a turn is being
    /// written into it), so nothing is painted and the wire is only asked to
    /// bind. See [`PaintPlan::bind_only`].
    pub keep: bool,
    /// How much history this load asks the wire for (`0` = the whole session).
    /// A bounded tail is what makes an open recent-first instead of a wait for
    /// the entire transcript.
    pub tail: usize,
    pub items: Option<Vec<crate::Item>>,
    /// Whether the cached transcript reaches the session's origin. A painted
    /// cache that does not still owes the server a page, even when it is fresh
    /// enough to suppress the replay.
    pub complete: bool,
}

/// Decide the paint from the cache stamp alone. `force` (a full refresh)
/// ignores freshness; `incomplete` (a store known to hold a cut-off turn) may
/// not anchor a merge.
pub(crate) fn plan_paint(
    cache: &crate::cache::CacheStore,
    key: &str,
    listed_at: &str,
    force: bool,
    incomplete: bool,
) -> PaintPlan {
    match cache.load_transcript(key) {
        Some((items, cached_at, complete)) => {
            let settled = !cached_at.is_empty();
            // An incomplete store may not be trusted as current either: a
            // dropped turn's cache is written with an empty stamp, but a
            // mid-turn save could still carry the server's stamp.
            let fresh =
                !force && !incomplete && settled && !listed_at.is_empty() && cached_at == listed_at;
            let merge = !force && settled && !fresh && !incomplete;
            PaintPlan {
                suppress: fresh,
                merge,
                keep: false,
                tail: first_tail(fresh, merge),
                items: Some(items),
                complete,
            }
        }
        None => PaintPlan {
            suppress: false,
            merge: false,
            keep: false,
            // Nothing to paint: ask for a bounded tail, not the whole session.
            tail: OPEN_TAIL,
            items: None,
            complete: false,
        },
    }
}

impl PaintPlan {
    /// The plan for a session the core is ALREADY writing into.
    ///
    /// This is not a cache verdict — it is the one case where the server cannot
    /// be the truth: goose persists a message only when it FINISHES, so a
    /// `session/load` replay can never contain the reply currently being
    /// streamed, and the replay's own supersede/`reset` deletes the rows we
    /// already hold (and clears the stream id, so the rest of the reply opens a
    /// new bubble mid-sentence). So: paint nothing, keep the store, and take a
    /// bind-only load.
    ///
    /// Measured 2026-11-21: switching away mid-reply and back left a chat with
    /// its prompt but no reply, or a reply that started a paragraph in.
    pub(crate) fn bind_only() -> Self {
        PaintPlan {
            suppress: true,
            merge: false,
            keep: true,
            tail: 1,
            items: None,
            complete: true,
        }
    }
}

/// Paint a plan into a store (the one implementation of the three modes).
///
/// `suppress`/`merge` are passed separately because they may be the VERIFIED
/// decision rather than the plan's original verdict (see [`load_with_plan`]).
pub(crate) fn apply_paint(store: &crate::transcript::TranscriptStore, plan: &PaintPlan) {
    apply_paint_decision(store, plan, plan.suppress, plan.merge);
}

fn apply_paint_decision(
    store: &crate::transcript::TranscriptStore,
    plan: &PaintPlan,
    suppress: bool,
    merge: bool,
) {
    // A kept store is authoritative: not painted, not cleared, and its cursor
    // left alone — it is the only copy of the turn being streamed.
    if plan.keep {
        return;
    }
    // The cache already knows whether it reaches the conversation's origin, so
    // the cursor is right from the first paint instead of waiting for the load
    // reply: a partial cache shows "there is more" and a complete one does not.
    // The reply corrects it either way.
    store.set_server_more(!plan.complete);
    match &plan.items {
        Some(items) if suppress => store.replace_rich(items.clone(), false),
        Some(items) if merge => store.replace_rich_for_merge(items.clone()),
        Some(items) => store.replace_rich(items.clone(), true),
        None => store.clear(),
    }
}

/// Open a session: paint its plan, then replay. ONE implementation for every
/// wire, and the load slot is claimed for the whole span — a paint that landed
/// while another load was mid-flight would leave that load's rows appended to a
/// transcript they do not belong to (see [`TranscriptStore::load_gate`]).
///
/// This is the entry point for a transport that decides paint and load
/// together (the roam peer, the remote-change resync). The main wire paints
/// before it dials, so its handshake calls [`replay_session`] directly.
pub(crate) async fn load_with_plan(
    endpoint: &dyn ChatEndpoint,
    host: &dyn DispatchHost,
    store: &crate::transcript::TranscriptStore,
    key: &str,
    cwd: &str,
    plan: &PaintPlan,
) -> Result<Value, AcpError> {
    let _load = store.begin_load().await;
    // A suppression is verified against the server before it is honoured: a
    // stale freshness baseline would otherwise discard the replay (see
    // [`verified_ask`]). VERIFY BEFORE PAINTING — a downgrade has to arm the
    // painted rows as a merge base, which `replace_rich` cannot do after the
    // fact (the replay tail would append a second copy of the newest turn).
    // A bind-only plan is NOT verified: verification exists to turn a wrong
    // "the cache is current" into a merge, and a merge would anchor on the live
    // rows this plan exists to protect — truncating the reply being streamed.
    let ask = if plan.keep {
        ReplayAsk::bind_only()
    } else {
        verified_ask(
            endpoint,
            host,
            key,
            ReplayAsk {
                suppress: plan.suppress,
                merge: plan.merge,
                tail: plan.tail,
            },
        )
        .await
    };
    apply_paint_decision(store, plan, ask.suppress, ask.merge);
    replay_held(endpoint, host, store, key, cwd, ask).await
}

/// The `session/load` replay lifecycle after the paint — one implementation for
/// every wire.
///
/// The caller paints first (`apply_paint`) and owns everything transport-shaped
/// around this: binding the session id, surfacing config, reporting an open
/// failure. What lives HERE is only the part that must be identical everywhere:
///
///   1. begin the replay,
///   2. hold emissions, so a tail lands as ONE repaint instead of the history
///      streaming in from the top (grouse#18),
///   3. request the load with the tail the plan owes,
///   4. close the merge (`end_merge`): on a gap the cached prefix does not meet
///      the replayed tail, so clear and load the whole transcript,
///   5. release the hold and disarm the replay.
///
/// This used to be hand-written once per transport, and the copies drifted: the
/// peer's lost steps 2 and 4, so a reopened roam chat appended its replayed tail
/// on top of a silently truncated cache paint — recent prompts twice, the second
/// copy at the bottom. Anything sequenced around a `session/load` belongs in
/// this function, not in a transport.
///
/// `key` is the DISPLAY id; the endpoint strips its own prefix (`wire_id`).
pub(crate) async fn replay_session(
    endpoint: &dyn ChatEndpoint,
    host: &dyn DispatchHost,
    store: &crate::transcript::TranscriptStore,
    key: &str,
    cwd: &str,
    suppress: bool,
    merge: bool,
) -> Result<Value, AcpError> {
    let _load = store.begin_load().await;
    let mut ask = ReplayAsk::for_mode(suppress, merge);
    // Cold resume used to take the plan's "fresh cache" word for it and drop the
    // replay, so messages another client wrote while the app was closed were
    // silently missing. Use the SAME verification as every other load. The one
    // exception is a live turn: the main wire's bind-only path passes
    // `suppress=true` with no merge, and the store — not the server — is the
    // newest copy there. Probing it could downgrade a bind to a merge and
    // truncate the reply being streamed, which is exactly what bind-only exists
    // to prevent (`PaintPlan::bind_only`).
    if !(ask.suppress && host.turn_in_flight(key)) {
        ask = verified_ask(endpoint, host, key, ask).await;
    }
    replay_held(endpoint, host, store, key, cwd, ask).await
}

/// [`replay_session`]'s body, for callers already holding the load slot.
/// Ends a replay that never finished.
///
/// `begin_replay` puts the session in `state.replaying` and the store into a held
/// state; both are undone by the code below, but ONLY if this future runs to the end.
/// When it does not — the peer's link dies under the load, the task is dropped, the
/// process is tearing down — nothing cleans up, and the core then believes a replay is
/// still in flight for that session **forever**:
///
/// - `refresh_session(Full)` returns immediately (`state.replaying.contains`), so a
///   chat that had one interrupted replay can never be force-reloaded again;
/// - the client's reload bar, set when the user asked, never gets a `done` event and
///   spins with nothing to report.
///
/// A cancelled await is not an edge case on roam — it is what a laptop going to sleep
/// looks like — which is why this is a drop guard rather than more `if let Err` arms.
struct ReplayGuard<'a> {
    host: &'a dyn DispatchHost,
    store: &'a crate::transcript::TranscriptStore,
    key: &'a str,
    /// `end_replay` is still owed to the host.
    ends: bool,
    /// The store's emissions are still held.
    holds: bool,
    /// A merge gate we armed and have not yet closed with `end_merge()`.
    merge: bool,
    /// The rows on screen before a merge gap forced a clear. The guard owns
    /// them so a CANCELLED rebuild (a dropped future, a dead roam link) puts
    /// them back too — not just an `Err` arm that may never run.
    painted: Option<Vec<crate::Item>>,
    /// True from the `clear()` for a gap rebuild until that rebuild answers.
    rebuilding: bool,
}

impl Drop for ReplayGuard<'_> {
    fn drop(&mut self) {
        if self.rebuilding {
            // The store was cleared for a whole-session rebuild that did not
            // finish. Restore what the client was reading and mark the hole so
            // no later cache save can call this transcript current.
            if let Some(painted) = self.painted.take() {
                self.store.replace_rich(painted, false);
            }
            self.host.mark_incomplete(self.key);
        }
        if self.merge {
            // Every exit owes `end_merge()`: leaving the store armed lets a
            // later chunk truncate it at an anchor from this abandoned load.
            let _ = self.store.end_merge();
        }
        if self.ends {
            self.host.end_replay(self.key);
        }
        if self.holds {
            self.store.release_held_replay();
        }
    }
}

/// How much history a `session/load` reply withheld: `_meta.replaySkipped` is
/// the index the replay started at, so it is exactly the backlog a page can
/// still fetch. Absent (or zero) means the store has the whole session.
pub(crate) fn replay_skipped(reply: &Value) -> usize {
    reply
        .get("_meta")
        .and_then(|meta| meta.get("replaySkipped"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize
}

async fn replay_held(
    endpoint: &dyn ChatEndpoint,
    host: &dyn DispatchHost,
    store: &crate::transcript::TranscriptStore,
    key: &str,
    cwd: &str,
    ask: ReplayAsk,
) -> Result<Value, AcpError> {
    let ReplayAsk {
        suppress,
        merge,
        tail,
    } = ask;
    // `0` = the whole session (`first_tail` says which mode wants that).
    //
    // Otherwise the tail is a LOWER BOUND on what the replay must cover: it
    // REPLACES the rows it covers, so asking for less than the store already
    // holds would truncate the transcript. That is exactly what a reconnect
    // resumes — a store that is already full — so the ask grows to the store
    // (whose rows are an upper bound on messages, since one message can split
    // into a bubble per tool call) plus one page of fresh tail.
    let asked = if tail == 0 || suppress || merge {
        tail
    } else {
        tail.max(store.len() + OPEN_TAIL)
    };
    let tail = if asked == 0 { None } else { Some(asked) };
    host.begin_replay(key, suppress);
    store.hold_emissions(true);
    // What the client is looking at while this replay runs. Kept only for a MERGE,
    // because only a merge can end in a gap — and the gap path has to clear the
    // store to rebuild it (see below), so this is the only copy of the rows the
    // screen is showing.
    let painted = if merge {
        let painted = store.rich_transcript();
        // Re-arm the paint as a merge base even when it was applied BEFORE the
        // verification decided to merge (the main wire paints in
        // `Core::open_session_inner`, then probes in its handshake). Without
        // this `end_merge()` below sees no merge gate, and the overlapping tail
        // is APPENDED to rows the client already has: the newest turn twice.
        // `replace_rich` is a no-op for identical rows, so this cannot re-paint
        // a merge that was already armed by `apply_paint_decision`.
        store.replace_rich_for_merge(painted.clone());
        Some(painted)
    } else {
        None
    };
    let mut guard = ReplayGuard {
        host,
        store,
        key,
        ends: true,
        holds: true,
        merge,
        painted,
        rebuilding: false,
    };
    let reply = load_request(endpoint, key, cwd, tail).await;
    let reply = match reply {
        Ok(reply) => reply,
        // The rows on screen are still the cache paint, which is what the client
        // should keep for a failed load. The guard disarms the merge, releases the
        // hold and ends the replay, so the chat can be reloaded again.
        Err(error) => return Err(error),
    };
    let mut reply = reply;
    // Merge gap: the tail started past the cached prefix, so extending the
    // cache would leave a hole. Clear and load the whole transcript now — but a
    // rebuild is allowed to FAIL (it is capped at `REBUILD_TIMEOUT_SECS`, and a
    // roam link can drop mid-stream), and by then the store has been cleared. If
    // it fails, or this future is DROPPED while it waits, the guard puts the
    // paint back and marks the session incomplete so a later save cannot call a
    // transcript with a hole current.
    let gap = merge && store.end_merge();
    guard.merge = false;
    if gap {
        guard.rebuilding = true;
        store.clear();
        if let Ok(full_reply) = load_request(endpoint, key, cwd, None).await {
            reply = full_reply;
            guard.rebuilding = false;
        }
        // On failure (or cancellation) the guard's `Drop` restores the painted
        // rows and marks the hole — whichever way this load ends.
    }
    // Tell the host how much history the server still holds BEFORE the paint is
    // released: a bounded replay leaves a backlog, and the cursor that paint
    // emits must already account for it — otherwise a client that drains the
    // history (a hard reload) sees "nothing older" for one frame and stops.
    // `note_backlog` also starts the background fill.
    host.replay_backlog(key, replay_skipped(&reply), asked);
    // A load that ANSWERED settles the store against the server, so a chat left
    // `incomplete` by a dropped turn is no longer owed a full replay on every
    // open — its cache can be stamped settled from now on. Not while a turn is
    // in flight: the store is then the authority, not this replay (see
    // `PaintPlan::bind_only`). This is the second half of "it reloads the whole
    // chat on every open": `incomplete` was set on send and only a clean
    // `finish_turn` cleared it, so a turn whose reply was released by
    // `turn_lost` left the flag set for the life of the process.
    if !host.turn_in_flight(key) {
        host.clear_incomplete(key);
    }
    // The replay is in: paint the newest window (or the empty transcript) once.
    store.release_held_replay();
    guard.holds = false;
    // The progress bar belongs to the WHOLE load: a merge that had to rebuild is
    // still loading, and ending it here (as this did) let the bar finish while the
    // rebuild was still running — then the screen changed after "loading" was over.
    guard.ends = false;
    host.end_replay(key);
    Ok(reply)
}

