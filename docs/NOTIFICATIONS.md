# Notifications

What each Grouse client announces, and the one rule that shapes all of it:

> **A client must work completely against a stock `goose serve`.** Grouse never
> requires server-side plumbing of its own — no helper scripts, no config keys
> we depend on, no forked server behaviour. Anything that would need that is
> either built from events the client already receives, or left to the operator.

## In-session notifications (no server support)

Every connected client already sees these on its ACP connection, so they are
announced client-side, and only when the window/UI cannot show them:

| Trigger | Wire event | Desktop | Android |
|---|---|---|---|
| Turn finished | `RunEnded` / stop reason | notification carrying the reply text | notification when backgrounded and this device armed the turn |
| Approval needed | permission request | notification | notification |
| Session changed elsewhere | `session_info_update` | notification (`sid != open session`) | sidebar badge |

Desktop uses `org.freedesktop.Notifications` (`src/notifier.{h,cpp}`) and is
gated by **Settings → Notifications**, firing only when the app is not the
active window. Android uses `Notifier.kt` with two channels (`Connection`,
`Replies`) and inline Reply / Mark-as-read actions.

## Where the policy lives

`core/grouse-core/src/notify.rs` — one implementation, exposed on the contract:

- `parse_push(raw) -> PushEnvelope` — `{kind, session_id, text, run_id}`, never
  failing (a broken body is a briefing carrying the raw text). `session_id` is
  **sender-supplied and untrusted**: it says what a push claims to be about, never
  where a Reply goes.
- `decide_notify_run(envelope, ctx, announced_run) -> NotifyDecision` — the
  run-aware entry point clients should prefer; `decide_notify(envelope, ctx)` is
  the compatibility wrapper with no announced run id.
- `decide_notify*` returns `{show, summary, body, reply_session}`. `NotifyContext`
  is what the client knows: `app_visible`, `armed_session`, `session_title`,
  `announce_any_turn`, `announced_session` / `announced_secs_ago` (the last turn
  this client announced itself, so the second sighting of the same event inside
  a bounded window is suppressed), and `armed_owner`.

Both delivery paths can see the same event — a live connection watches the turn end,
while the operator's sender pushes for it seconds later — so the last two context
fields and the run id are how the policy tells the second sighting from a genuine
second turn in the same chat. Senders cannot supply the missing identity: goose's
`Stop` hook payload carries no run id.

### Reply targets and roam: the normative rule (ABI 19)

The envelope's `session` is written by whoever POSTs to the distributor endpoint;
it is not evidence of a conversation on this device. **`NotifyDecision.reply_session`
is the only valid Reply destination.** The core fills it for a `Turn` only when the
id matches a session this client armed or announced itself; it is `null` for every
briefing, every task, and every turn it cannot verify. A client must wire a turn
notification's Reply (and its conversation tap target, when there is one) from
`reply_session`, never from `PushEnvelope.session_id`; when it is `null`, open the
app instead of guessing a chat.

A `roam:<label>:<id>` id names a peer as well as a chat, and the sender chose both
parts. A roam turn is therefore announced only when `NotifyContext.armed_owner`
declares the bare label of the peer this client armed (`nas`, not `roam:nas:s1`;
a `roam:`-prefixed value is accepted too) and it matches the id's namespace; with
no matching declaration, even a single-client desktop that otherwise announces
any turn gets a hidden decision and no reply target. Briefings and tasks have no
armed owner and no reply target by definition.

ABI **19** is a JSON record-shape bump: `NotifyContext.armed_owner` and
`NotifyDecision.reply_session` were added, with no symbol-signature or listener-table
change. A desktop built for 19 must not load an older `libgrouse_core.so` whose
decisions have no `reply_session` key; Android must regenerate the uniffi bindings
so it sees `armedOwner` / `replySession`.

Android calls these through uniffi; the desktop through `grouse_push_parse`,
`grouse_push_decide` and `grouse_push_decide_run` on the C ABI. Neither client
parses an envelope or decides for itself — that is the point, and it is why the
Kotlin parser and its tests were deleted when this landed (the cases live in
`notify.rs`). What stays per-client is
*transport* (UnifiedPush connector vs `Connector2`) and *rendering*
(`NotificationCompat` vs `org.freedesktop.Notifications`), plus the two
client-local events above (approval, session-changed), which are not push kinds.

## Out-of-band push (operator-owned)

`goose serve` has no push. A client that is *not* running therefore cannot be
told anything by a stock server — that is a goose limitation, not a Grouse bug.
Where a phone or desktop wants notifications while closed, the delivery path is
the operator's own:

- The **client registers** with whatever UnifiedPush distributor the user runs
  (Android: the UnifiedPush connector; desktop: `PushClient`, which speaks
  `org.unifiedpush.Connector2` over D-Bus to e.g. `org.unifiedpush.Distributor.kde`).
- The **client receives** the envelope below and shows it. Nothing else.
- The **operator sends** — e.g. a goose hook or an MCP extension POSTing to the
  registered endpoint. No Grouse code requires this to exist.

Endpoints are per registration, so phone and desktop have different ones. A
client publishes its endpoint where an operator's sender can find it, as a
private convention (Android historically `GROUSE_PUSH_ENDPOINT`, desktop
`GROUSE_PUSH_ENDPOINT_DESKTOP`); a stock server never reads these.

### Envelope

```json
{"type":"turn","session":"<session id>","text":"<optional reply text>"}
{"type":"notify","text":"<one sentence>"}
{"type":"task","text":"<the result line>"}
```

Anything that is not a JSON object is treated as a briefing (`text` = the raw
body). `text` is optional — a sender that cannot know the reply (a server-side
hook never sees the transcript) may omit it.

The three known types differ in where a TAP lands, which is the whole reason
`task` exists apart from `briefing`:

| type | announced when | tap opens |
|---|---|---|
| `turn` | a reply you armed finished | that conversation, via the core-verified `reply_session` only |
| `notify` / anything else | a proactive message (a briefing) | the app (the assistant thread is one tap away) |
| `task` (`tasks` is accepted too) | handed-over work reached `review` | **My Day**, refreshed on arrival, because that is where the row, its result line and the rest of the thread live |

The show/don't-show decision is one implementation for every client
(`core/grouse-core/src/notify.rs`): a visible app is never interrupted, empty text
is a sender bug, and a `turn` is only announced by a client that can call it its
own (a roam turn additionally needs the armed peer label above). A `task` is
proactive like a briefing — no armed session, no reply target — but named for what
it is, so the destination can differ. A client older than the type treats it as a
briefing: a correct notification with the wrong destination, never silence.
