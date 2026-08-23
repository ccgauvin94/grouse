# Turn state moves into the core

**Status:** in progress · **Driver:** every wedge fixed on 2026-08-23 lived in
`ConnectionManager.kt`, one layer away from the facts that should have cleared it.

## Why

The core owns the socket, reconnect, transcript, and caches (CONTRACT §1). But
the *turn* — busy, the in-flight run id, the steer-vs-prompt decision, the
send queue — lives in each client. The client can only learn a turn ended from
an event; when the connection dies, no event comes, and every client must
reconstruct "the turn can never end now" from lifecycle edges it hears about
separately. The 2026-08-23 ledger, all client-side latch bugs of exactly this
shape:

- stale `busy` suppressed sends after reconnect (only the main path cleared it)
- sends steered into a dead run id instead of prompting
- terminal peer states tore the UI down but left `busy` latched ("thinking"
  hours later, messages queued behind a turn that could never end)
- the foreground-recovery nudge had to be added app-side because the core has
  the reconnect intents but no lifecycle hook

The desktop client duplicates or misses all of the same logic independently.

## The rule

**A turn is owned by the connection that started it and dies with it.** The
core is the only layer that knows both halves, so the core owns the turn.

## Core model

Per chat owner (main connection or roam peer), at most one in-flight turn:

```rust
#[uniffi::record]
pub struct TurnState {
    pub session_id: String,
    /// The live run id when the server surfaced it (steer key), else empty.
    pub run_id: String,
    /// A turn is in flight (send accepted or run streaming).
    pub busy: bool,
    /// Prompts waiting behind the turn / behind a reconnect.
    pub queued: u32,
}
```

New listener event, one per family as usual: `on_turn(state: TurnState)`.
Emitted on every transition: send accepted, run id surfaced, run ended, queue
length change, and — the point of the exercise — cleared by the core when the
owning connection ends, in the same code path that already runs the reconnect
policy. No client will ever hear about a turn the core knows is dead.

## Send semantics (core-owned)

`Core::send(prompt)` replaces client-side dispatch:

1. Append the user message to the core transcript (optimistic bubble comes
   from `on_transcript` like every other row — clients stop keeping a parallel
   message list for it).
2. Route: owner live + ready + no turn in flight for the target session →
   `session/prompt`, mark busy. Same-session turn in flight + run id known →
   steer (`expected_run_id`, server-validated). Turn in flight but no run id
   yet, or owner not ready/reconnecting → queue.
3. Flush the queue on run end and on ready (reconnect included). Drop the
   queue only on explicit disconnect or session switch.

`send_prompt(prompt, expect)` remains as the raw op (unstable surface) for the
transition; clients migrate to `send`.

## Lifecycle

`Core::on_foreground()`: clients forward the platform resume event, nothing
else. The core redials dead roam intents with a fresh backoff budget (the
budget only burns while the process is awake, so dozing exhausts it) and
re-nudges the main connection if it gave up. The Kotlin ON_RESUME redial
shipped 2026-08-23 is the stopgap this replaces.

## Client contract after migration

Clients render `TurnState` (composer busy, "N queued" chip, steer affordance)
and forward lifecycle. They hold no turn latches, no run ids, no queues, and
make no prompt-vs-steer decisions. The ~200 lines of dispatch/latch logic in
`ConnectionManager.kt` (send/dispatch/queue/turnInFlight/activeRunId and the
clearing sites in open()/openSession()/onRoamPeerStatus/onAppResumed) are
deleted, not mirrored.

## Migration phases

- **A (core)**: `TurnState` + `on_turn` + core-cleared latches wired into the
  existing connection-end/ready paths (spine + roam peer), `on_foreground`,
  `send` with routing + queue. Tests: turn cleared on connection death (the
  regression that mattered), steer only with live run id + same session,
  queue flush on ready, foreground redial.
- **B (android)**: consume `on_turn`, forward ON_RESUME, delete the latch and
  dispatch code. Behavior parity pass on the phone.
- **C (desktop)**: same consumption; delete its duplicate.
- **CONTRACT.md**: §3.2 gains `on_turn`; §3.1 gains `send` + `on_foreground`;
  §4 gains the turn-ownership rule. Amended when A+B land, since the contract
  records locked decisions, not intentions.

## Invariants (test-enforced)

1. No `TurnState.busy == true` survives its owning connection's end.
2. A steer is only issued with a server-surfaced run id, same session, same
   connection generation.
3. Queued prompts are never silently dropped except by explicit disconnect or
   session switch; every drop empties `queued` via an `on_turn` emission.
4. `on_foreground` is idempotent and never dials a peer that is ready,
   dialing, or mid-reconnect.
