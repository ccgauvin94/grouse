// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import android.app.Activity
import android.content.Context
import org.unifiedpush.android.connector.FailedReason
import org.unifiedpush.android.connector.PushService
import org.unifiedpush.android.connector.UnifiedPush
import org.unifiedpush.android.connector.data.PushEndpoint
import org.unifiedpush.android.connector.data.PushMessage
import uniffi.grouse_core.NotifyContext
import uniffi.grouse_core.PushKind
import uniffi.grouse_core.decideNotifyRun
import uniffi.grouse_core.parsePush

/**
 * Pure push-target policy. A push is unauthenticated input: anyone who learns the
 * distributor's endpoint URL can POST an envelope naming an arbitrary `session`.
 * Only a TURN is about a conversation, and even then the id must be one this device
 * knows and — when a turn was armed here — the exact id it armed. A briefing or a
 * task push is an announcement, never a chat target. Rejection degrades to a
 * session-less notification (open the app; reply to the chat on screen), so no
 * reply PendingIntent or `openSession` can be aimed at a server-chosen chat.
 */
internal fun validatedPushSession(
    kind: PushKind,
    session: String?,
    knownSessionIds: Collection<String>,
    armedSessionId: String?,
): String? {
    if (kind != PushKind.TURN) return null
    val id = session?.trim()?.takeIf { it.isNotEmpty() } ?: return null
    if (id !in knownSessionIds) return null
    val armed = armedSessionId?.trim()?.takeIf { it.isNotEmpty() }
    if (armed != null && armed != id) return null
    return id
}

/**
 * UnifiedPush wiring. The distributor (e.g. NextPush, backed by the uppush app on the user's
 * Nextcloud) holds the one battery-friendly connection; the server POSTs to the endpoint URL to
 * wake us — no FCM, no per-app foreground socket needed just to receive alerts.
 *
 * Only TRANSPORT lives here. The envelope parser and the show/don't-show rule are in the core
 * (notify.rs), shared with the desktop client, so a sender's payload behaves the same
 * everywhere. See ../../docs/NOTIFICATIONS.md.
 */
object Push {
    /** Turn push on: ensure a distributor is chosen, then register (→ GoosePushService.onNewEndpoint). */
    fun enable(activity: Activity) {
        SecureStore(activity).pushEnabled = true
        UnifiedPush.tryUseCurrentOrDefaultDistributor(activity) { ok ->
            if (ok) UnifiedPush.register(activity)
            else UnifiedPush.tryPickDistributor(activity) { picked -> if (picked) UnifiedPush.register(activity) }
        }
    }

    fun disable(context: Context) {
        SecureStore(context).apply { pushEnabled = false; pushEndpoint = "" }
        UnifiedPush.unregister(context)
    }

    /** Re-register on app start so the endpoint is refreshed (endpoints can rotate). */
    fun refresh(context: Context) {
        val store = SecureStore(context)
        if (store.pushEnabled && UnifiedPush.getSavedDistributor(context) != null) UnifiedPush.register(context)
    }
}

/** Receives UnifiedPush events: renders pushes as notifications, records/publishes the endpoint. */
class GoosePushService : PushService() {
    override fun onMessage(message: PushMessage, instance: String) {
        val raw = String(message.content).trim()
        if (raw.isEmpty()) return
        val cm = ConnectionManager.get(this)
        // One policy, in the core: decode the envelope and decide whether it is worth
        // interrupting the user. `announce_any_turn = false` is the phone's rule — a push
        // can arrive for work another client or a scheduled run started, and only a
        // session this device armed is announced.
        val envelope = parsePush(raw)
        val (announcedSession, announcedSecsAgo) = cm.announcedTurn
        val armedSession = cm.store.pendingPushSessionId
        val shownSession = cm.currentSession.value
        // A `roam:<label>:<id>` id is a CLIENT-side namespace: the sender writes
        // the label too, so the core hides a roam turn (and never replies to it)
        // unless we declare the bare peer label of the session we armed. Use the
        // shown chat only when nothing is armed.
        val armedOwner = ConnectionManager.roamPeer(armedSession)
            ?: ConnectionManager.roamPeer(shownSession)
        // Validate before anything can carry the id into a PendingIntent or a deep link.
        // The manager's known set includes cached ids the filtered session list hides
        // (the assistant thread), so a genuine turn is not stripped just because the
        // list is a hint. An armed turn narrows the target to that one chat.
        val targetSession = validatedPushSession(
            envelope.kind, envelope.sessionId, cm.knownPushSessionIds(), armedSession)
        if (targetSession == null && !envelope.sessionId.isNullOrBlank()) {
            cm.noteApp("push ${envelope.kind}: dropped untrusted session target")
        }
        val decision = decideNotifyRun(
            envelope,
            NotifyContext(
                appVisible = cm.isForeground,
                armedSession = armedSession,
                sessionTitle = shownSession?.let {
                    cm.sessions.value.firstOrNull { s -> s.sessionId == it }?.title
                },
                // The live path may have announced this very turn seconds ago.
                announcedSession = announcedSession,
                announcedSecsAgo = announcedSecsAgo?.toUInt(),
                announceAnyTurn = false,
                armedOwner = armedOwner,
            ),
            // Pair the sender's run id with the run this app announced so two
            // turns in one chat are not mistaken for one redelivery.
            cm.announcedTurnRunId,
        )
        // The core's replySession is the ONLY destination a Reply PendingIntent
        // or the tap deep link may use — never the sender's envelope `session`.
        // validatedPushSession stays as a SECOND gate: if the core names a chat
        // this client cannot vouch for, drop the target instead of routing it.
        val replySession = decision.replySession?.takeIf { it == targetSession }
        if (decision.replySession != null && replySession == null) {
            cm.noteApp("push ${envelope.kind}: dropped unverified reply target")
        }
        // Any proactive push is news about the assistant's state, so re-read it: a
        // briefing or a task flip changes what My Day shows, and a fresh notification
        // over a stale list is the version of this that wastes a tap. Deterministic
        // tools/call, no model turn, and harmless when the app is in front.
        if (envelope.kind != PushKind.TURN) cm.refreshTasks()
        if (envelope.kind == PushKind.BRIEFING) {
            // A briefing is recorded for the Assistant status/dialog even while the app is
            // in front — otherwise one that lands while you are looking is lost and the
            // dialog wrongly reads "none yet".
            cm.store.lastBriefingAt = System.currentTimeMillis()
            cm.store.lastBriefingText = envelope.text
        }
        // Record every arrival, shown or not, in the same in-app log the turn machinery
        // writes to (long-press the chat title). "Push is broken" is otherwise unanswerable
        // from this side: the sender only learns that uppush ACCEPTED the POST, so the two
        // failure classes — it never reached the device, or it arrived and the policy
        // suppressed it — look identical from the console.
        cm.noteApp("push ${envelope.kind} shown=${decision.show} ${decision.summary}")
        if (decision.show) {
            if (envelope.kind == PushKind.TURN) cm.store.pendingPushSessionId = null
            Notifier(this).postMessage(decision.summary, decision.body, replySession,
                proactive = envelope.kind != PushKind.TURN,
                // Handed-over work taps into My Day, where the row and its result are;
                // a briefing has no single row to land on, so it keeps opening the app.
                myDay = envelope.kind == PushKind.TASKS)
        }
    }

    override fun onNewEndpoint(endpoint: PushEndpoint, instance: String) {
        SecureStore(this).pushEndpoint = endpoint.url
        // Self-heal for rotation (the exact bug that made "test pushes not arrive": a reinstall
        // minted a fresh uppush registration while the server kept POSTing the dead token).
        // Publish the endpoint into goose's server-side config over the ACP socket, where senders
        // read it. Best-effort — if the socket is down now, the next app start re-registers
        // (Push.refresh) and lands here again. NOTE: this is the only publication path; the
        // external-registry POST that used to sit here was removed 2026-09-15 — Grouse clients
        // must not depend on server-side plumbing we invented (Grouse/Goose contract).
        ConnectionManager.get(this).publishPushEndpoint(endpoint.url)
    }

    override fun onRegistrationFailed(reason: FailedReason, instance: String) {}

    override fun onUnregistered(instance: String) { SecureStore(this).pushEndpoint = "" }
}
