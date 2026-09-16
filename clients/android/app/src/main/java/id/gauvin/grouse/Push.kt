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
import uniffi.grouse_core.decideNotify
import uniffi.grouse_core.parsePush

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
        val decision = decideNotify(
            envelope,
            NotifyContext(
                appVisible = cm.isForeground,
                armedSession = cm.store.pendingPushSessionId,
                sessionTitle = cm.currentSession.value?.let {
                    cm.sessions.value.firstOrNull { s -> s.sessionId == it }?.title
                },
                // The live path may have announced this very turn seconds ago.
                announcedSession = announcedSession,
                announcedSecsAgo = announcedSecsAgo?.toUInt(),
                announceAnyTurn = false,
            ),
        )
        if (envelope.kind == PushKind.BRIEFING) {
            // A briefing is recorded for the Assistant status/dialog even while the app is
            // in front — otherwise one that lands while you are looking is lost and the
            // dialog wrongly reads "none yet".
            cm.store.lastBriefingAt = System.currentTimeMillis()
            cm.store.lastBriefingText = envelope.text
        }
        if (decision.show) {
            if (envelope.kind == PushKind.TURN) cm.store.pendingPushSessionId = null
            Notifier(this).postMessage(decision.summary, decision.body, envelope.sessionId,
                proactive = envelope.kind != PushKind.TURN)
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
