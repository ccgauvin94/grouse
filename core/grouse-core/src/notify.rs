//! Notification policy — the ONE implementation every client shares.
//!
//! Clients differ in three things and nothing else: *transport* (Android's
//! UnifiedPush connector, the desktop's D-Bus `Connector2`), *rendering*
//! (`NotificationCompat` vs `org.freedesktop.Notifications`), and what their
//! platform knows about the user's attention. Everything decidable lives here, so a
//! sender's payload behaves the same on the phone and on the desktop — before this,
//! the envelope parser and the show/don't-show rule existed once per client and had
//! already drifted (the phone stayed quiet for an unfocused app, the desktop did not;
//! only the phone knew which session it had armed).
//!
//! Nothing here touches the network or the OS: it is pure policy over a payload a
//! client already received, which is what makes it testable and shareable.

use serde::{Deserialize, Serialize};

/// What a push is: a finished-turn nudge, or a proactive briefing.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PushKind {
    Turn,
    Briefing,
}

/// A decoded push envelope. Senders produce `{type,session,text}` JSON; anything
/// that is not a JSON object is a briefing whose text is the raw body (the oldest
/// senders posted bare text).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PushEnvelope {
    pub kind: PushKind,
    pub session_id: Option<String>,
    pub text: String,
}

/// What the client knows when a payload arrives.
#[derive(uniffi::Record, Clone, Debug, Serialize, Deserialize)]
pub struct NotifyContext {
    /// True when the user is looking at this client right now — Android: the app is
    /// in the foreground; desktop: the window is active. Each platform defines
    /// "visible" for itself; the policy only needs the answer.
    pub app_visible: bool,
    /// The session this client last sent a turn to, when it tracks one.
    pub armed_session: Option<String>,
    /// The session's title, when the client knows it (the desktop's sidebar does; a
    /// push delivered to a sleeping phone carries ids only). Used as the notification's
    /// summary so "Daily Digest" beats "Grouse replied" wherever it is available.
    pub session_title: Option<String>,
        /// The session and age of the last notification THIS client showed for a finished
    /// turn. The two delivery paths overlap by design (a live connection sees the turn
    /// end; the operator's sender pushes for the same turn), so the same event would be
    /// announced twice. Senders cannot disambiguate — goose's hook payload carries no run
    /// id — so recency is the available identity: the same session inside the window is
    /// the same turn.
    pub announced_session: Option<String>,
    pub announced_secs_ago: Option<u32>,
    /// True when a finished turn the client did NOT start is still worth announcing —
    /// a single-client desktop, where any turn is effectively yours. False on the
    /// phone: a push can arrive for work another client (or a scheduled run) started,
    /// and buzzing for it is noise; the phone only announces turns it armed.
    pub announce_any_turn: bool,
}

/// Whether to show anything, and with what wording. Wording lives here too: the two
/// clients used to say different things for the same event.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NotifyDecision {
    pub show: bool,
    pub summary: String,
    pub body: String,
}

impl NotifyDecision {
    fn hidden() -> Self {
        Self { show: false, summary: String::new(), body: String::new() }
    }
}

/// Decode a push body. Never fails: an unparseable body is a briefing carrying the
/// raw text, because a sender that got the envelope wrong should still reach the user.
#[uniffi::export]
pub fn parse_push(raw: &str) -> PushEnvelope {
    let trimmed = raw.trim();
    let value: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => return briefing(trimmed),
    };
    let Some(obj) = value.as_object() else {
        return briefing(trimmed);
    };
    let text = obj
        .get("text")
        .and_then(serde_json::Value::as_str)
        .filter(|t| !t.is_empty())
        .unwrap_or(trimmed)
        .to_string();
    let session_id = obj
        .get("session")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    match obj.get("type").and_then(serde_json::Value::as_str) {
        Some("turn") => PushEnvelope { kind: PushKind::Turn, session_id, text },
        _ => PushEnvelope { kind: PushKind::Briefing, session_id, text },
    }
}

/// How long after announcing a turn a push for the same session is still considered the
/// same event. The hook fires at turn end and the connection sees the same end within
/// seconds, so this only has to absorb that skew — not a whole conversation.
const SAME_TURN_WINDOW_SECS: u32 = 120;

fn already_announced(envelope: &PushEnvelope, ctx: &NotifyContext) -> bool {
    let Some(session) = envelope.session_id.as_deref() else { return false };
    ctx.announced_session.as_deref() == Some(session)
        && ctx
            .announced_secs_ago
            .is_some_and(|secs| secs <= SAME_TURN_WINDOW_SECS)
}

fn briefing(text: &str) -> PushEnvelope {
    PushEnvelope { kind: PushKind::Briefing, session_id: None, text: text.to_string() }
}

/// The show/don't-show rule, shared by push delivery and by the in-session paths
/// (a client that watched a turn end constructs a `Turn` envelope from what it has
/// and asks the same question).
#[uniffi::export]
pub fn decide_notify(envelope: PushEnvelope, ctx: NotifyContext) -> NotifyDecision {
    // The user is looking at the conversation: the UI already shows this.
    if ctx.app_visible {
        return NotifyDecision::hidden();
    }
    if envelope.text.is_empty() {
        // Nothing to say — an empty payload is a sender bug, not a notification.
        return NotifyDecision::hidden();
    }
    match envelope.kind {
        PushKind::Turn => {
            // Both paths can see the same turn end; the first one to announce wins.
            if already_announced(&envelope, &ctx) {
                return NotifyDecision::hidden();
            }
            // Attribution: a turn can only be announced when this client can call it
            // its own. A client that does not announce unattributed turns (the phone)
            // needs an armed session that matches; one that does (the desktop) still
            // narrows to the armed session when it has one.
            match (ctx.armed_session.as_deref(), ctx.announce_any_turn) {
                (Some(armed), _) if envelope.session_id.as_deref() != Some(armed) => {
                    return NotifyDecision::hidden()
                }
                (None, false) => return NotifyDecision::hidden(),
                _ => {}
            }
            NotifyDecision {
                show: true,
                summary: ctx
                    .session_title
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "Grouse replied".to_string()),
                body: envelope.text,
            }
        }
        PushKind::Briefing => NotifyDecision {
            show: true,
            summary: "Grouse briefing".to_string(),
            body: envelope.text,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(visible: bool, armed: Option<&str>) -> NotifyContext {
        // Desktop-like by default: announces turns it did not arm.
        NotifyContext {
            app_visible: visible,
            armed_session: armed.map(str::to_owned),
            session_title: None,
            announced_session: None,
            announced_secs_ago: None,
            announce_any_turn: true,
        }
    }

    fn phone_ctx(visible: bool, armed: Option<&str>) -> NotifyContext {
        NotifyContext {
            app_visible: visible,
            armed_session: armed.map(str::to_owned),
            session_title: None,
            announced_session: None,
            announced_secs_ago: None,
            announce_any_turn: false,
        }
    }

    #[test]
    fn parses_the_envelope() {
        let e = parse_push(r#"{"type":"turn","session":"s1","text":"done"}"#);
        assert_eq!(e.kind, PushKind::Turn);
        assert_eq!(e.session_id.as_deref(), Some("s1"));
        assert_eq!(e.text, "done");

        let b = parse_push(r#"{"type":"notify","text":"brief"}"#);
        assert_eq!(b.kind, PushKind::Briefing);
        assert_eq!(b.session_id, None);
        assert_eq!(b.text, "brief");
    }

    #[test]
    fn unparseable_bodies_still_reach_the_user() {
        // A sender that posted bare text (the oldest ones did) is a briefing.
        let e = parse_push("plain sentence");
        assert_eq!(e.kind, PushKind::Briefing);
        assert_eq!(e.text, "plain sentence");

        // Broken JSON keeps the raw body rather than dropping the message.
        let e = parse_push(r#"{"type":"turn","text":"#);
        assert_eq!(e.kind, PushKind::Briefing);
        assert_eq!(e.text, r#"{"type":"turn","text":"#);

        // An envelope with a missing/empty text falls back to the raw body.
        let e = parse_push(r#"{"type":"turn","session":"s1"}"#);
        assert_eq!(e.kind, PushKind::Turn);
        assert_eq!(e.text, r#"{"type":"turn","session":"s1"}"#);
    }

    #[test]
    fn a_visible_client_is_never_notified() {
        let e = parse_push(r#"{"type":"notify","text":"x"}"#);
        assert!(!decide_notify(e, ctx(true, None)).show);
    }

    #[test]
    fn armed_sessions_gate_turn_nudges_only() {
        let turn = parse_push(r#"{"type":"turn","session":"mine","text":"done"}"#);
        assert!(decide_notify(turn.clone(), ctx(false, Some("mine"))).show);

        let other = parse_push(r#"{"type":"turn","session":"theirs","text":"done"}"#);
        assert!(!decide_notify(other, ctx(false, Some("mine"))).show);

        // Without an armed session (single-client desktop) every turn is ours.
        assert!(decide_notify(turn, ctx(false, None)).show);

        // A briefing is never gated by the armed session: it is addressed to you.
        let brief = parse_push(r#"{"type":"notify","text":"brief"}"#);
        assert!(decide_notify(brief, ctx(false, Some("mine"))).show);
    }

    #[test]
    fn the_phone_suppresses_turns_it_did_not_arm() {
        // A scheduled run finishing while the phone is in your pocket must not buzz:
        // only a turn this device sent (and is waiting on) is announced.
        let scheduled = parse_push(r#"{"type":"turn","session":"heartbeat","text":"done"}"#);
        assert!(!decide_notify(scheduled.clone(), phone_ctx(false, None)).show);
        // Once this device has armed a session, that session's turn is announced...
        assert!(decide_notify(scheduled, phone_ctx(false, Some("heartbeat"))).show);
        // ...and another session's still is not.
        let other = parse_push(r#"{"type":"turn","session":"elsewhere","text":"done"}"#);
        assert!(!decide_notify(other, phone_ctx(false, Some("heartbeat"))).show);
        // The live path on the phone (the turn is its own) announces normally.
        let mine = parse_push(r#"{"type":"turn","session":"open","text":"done"}"#);
        assert!(decide_notify(mine, ctx(false, None)).show);
    }

    #[test]
    fn wording_is_shared_and_stable() {
        let turn = parse_push(r#"{"type":"turn","session":"s","text":"the answer"}"#);
        let d = decide_notify(turn, ctx(false, None));
        assert_eq!(d.summary, "Grouse replied");
        assert_eq!(d.body, "the answer");

        let brief = parse_push(r#"{"type":"notify","text":"a briefing"}"#);
        let d = decide_notify(brief, ctx(false, None));
        assert_eq!(d.summary, "Grouse briefing");
        assert_eq!(d.body, "a briefing");
    }

    #[test]
    fn the_second_path_to_the_same_turn_stays_quiet() {
        // The connection announced this turn five seconds ago; the operator's sender
        // pushes for the very same turn end. One notification, not two.
        let turn = parse_push(r#"{"type":"turn","session":"s","text":"done"}"#);
        let mut c = ctx(false, None);
        c.announced_session = Some("s".to_string());
        c.announced_secs_ago = Some(5);
        assert!(!decide_notify(turn.clone(), c).show);

        // A different session, or a stale window, is a new event.
        let mut other = ctx(false, None);
        other.announced_session = Some("elsewhere".to_string());
        other.announced_secs_ago = Some(5);
        assert!(decide_notify(turn.clone(), other).show);

        let mut stale = ctx(false, None);
        stale.announced_session = Some("s".to_string());
        stale.announced_secs_ago = Some(SAME_TURN_WINDOW_SECS + 1);
        assert!(decide_notify(turn.clone(), stale).show);

        // Briefings are never deduped against a turn: they are a different message.
        let brief = parse_push(r#"{"type":"notify","text":"brief"}"#);
        let mut c = ctx(false, None);
        c.announced_session = Some("s".to_string());
        c.announced_secs_ago = Some(1);
        assert!(decide_notify(brief, c).show);
    }

    #[test]
    fn a_known_session_title_leads_the_notification() {
        let turn = parse_push(r#"{"type":"turn","session":"s","text":"the answer"}"#);
        let mut titled = ctx(false, None);
        titled.session_title = Some("Daily Digest".to_string());
        let d = decide_notify(turn.clone(), titled);
        assert_eq!(d.summary, "Daily Digest");
        assert_eq!(d.body, "the answer");

        // An empty title is not a title.
        let mut blank = ctx(false, None);
        blank.session_title = Some(String::new());
        assert_eq!(decide_notify(turn, blank).summary, "Grouse replied");
    }

    #[test]
    fn empty_payloads_are_not_notifications() {
        let e = PushEnvelope { kind: PushKind::Turn, session_id: None, text: String::new() };
        assert!(!decide_notify(e, ctx(false, None)).show);
    }
}
