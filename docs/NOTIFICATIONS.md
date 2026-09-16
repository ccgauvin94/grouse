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

- `parse_push(raw) -> PushEnvelope` — the envelope, never failing (a broken body is
  a briefing carrying the raw text).
- `decide_notify(envelope, ctx) -> NotifyDecision` — `{show, summary, body}`, from
  what the client knows: `app_visible`, `armed_session`, `session_title`,
  `announce_any_turn`.

Android calls these through uniffi; the desktop through `grouse_push_parse` /
`grouse_push_decide` on the C ABI. Neither client parses an envelope or decides
for itself — that is the point, and it is why the Kotlin parser and its tests were
deleted when this landed (the cases live in `notify.rs`). What stays per-client is
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
```

Anything that is not a JSON object is treated as a briefing (`text` = the raw
body). `turn` is a finished-turn nudge; every other type is proactive/briefing.
`text` is optional — a sender that cannot know the reply (a server-side hook
never sees the transcript) may omit it.
