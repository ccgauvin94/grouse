package id.gauvin.grouse

import android.app.Activity
import android.content.Context
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.unifiedpush.android.connector.FailedReason
import org.unifiedpush.android.connector.PushService
import org.unifiedpush.android.connector.UnifiedPush
import org.unifiedpush.android.connector.data.PushEndpoint
import org.unifiedpush.android.connector.data.PushMessage
import java.util.concurrent.Executors

/** Parse a push envelope {type,session,text}; plain text (no type) is a briefing. Malformed
 *  JSON falls through to the raw text as a briefing. Top-level internal so the JVM unit tests
 *  can exercise it without instantiating the Android service. */
internal fun parsePush(raw: String): Triple<String?, String?, String> = try {
    val o = Json.parseToJsonElement(raw).jsonObject
    Triple(
        o["type"]?.jsonPrimitive?.contentOrNull,
        o["session"]?.jsonPrimitive?.contentOrNull,
        o["text"]?.jsonPrimitive?.contentOrNull ?: raw,
    )
} catch (e: Exception) {
    Triple(null, null, raw)
}

/** True when a finished-turn push should become a notification: the app is backgrounded, the
 *  envelope names a session, and it is the session this device armed (sent a turn and is still
 *  waiting on completion). Top-level internal so the JVM unit tests can exercise it. */
internal fun shouldShowTurnNudge(pushedSessionId: String?, pendingSessionId: String?, isForeground: Boolean): Boolean =
    !isForeground && pushedSessionId != null && pushedSessionId == pendingSessionId

/**
 * UnifiedPush wiring. The distributor (e.g. NextPush, backed by the uppush app on the user's
 * Nextcloud) holds the one battery-friendly connection; the server POSTs to the endpoint URL to
 * wake us — no FCM, no per-app foreground socket needed just to receive alerts.
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
        // Envelope {type,session,text}; plain text (no type) is treated as a briefing.
        val (type, session, text) = parsePush(raw)
        if (type == "turn") {
            // Finished-turn nudge (fires for every goose turn, Desktop too -- the server can't tell
            // clients apart). Only show it for a turn THIS device actually sent and is still waiting
            // on, and not while you're already watching (foreground). Tap deep-links to that session.
            if (shouldShowTurnNudge(session, cm.store.pendingPushSessionId, cm.isForeground)) {
                cm.store.pendingPushSessionId = null
                Notifier(this).postReply(text, session)
            }
        } else {
            // Briefing/proactive: ALWAYS record for the Assistant status/dialog — even when
            // foreground, or a briefing that lands while you're in the app is lost and the dialog
            // wrongly reads "none yet". Only raise a notification when backgrounded. Tap lands in
            // the persistent Assistant thread.
            SecureStore(this).apply { lastBriefingAt = System.currentTimeMillis(); lastBriefingText = text }
            if (!cm.isForeground) Notifier(this).postProactive(text, session)
        }
    }

    override fun onNewEndpoint(endpoint: PushEndpoint, instance: String) {
        SecureStore(this).pushEndpoint = endpoint.url
        PushRegistry.publish(this, endpoint.url)
        // Self-heal for rotation (the exact bug that made "test pushes not arrive": a reinstall
        // minted a fresh uppush registration while the server kept POSTing the dead token).
        // Publish the endpoint into goose's server-side config over the ACP socket, where senders
        // read it. Best-effort — if the socket is down now, the next app start re-registers
        // (Push.refresh) and lands here again.
        ConnectionManager.get(this).publishPushEndpoint(endpoint.url)
    }

    override fun onRegistrationFailed(reason: FailedReason, instance: String) {}

    override fun onUnregistered(instance: String) { SecureStore(this).pushEndpoint = "" }
}

/** Best-effort: POST our endpoint URL to an external registry so its senders know where to reach
 *  us. No-op until a registry URL is configured (SecureStore.pushRegistryUrl). */
object PushRegistry {
    private val io = Executors.newSingleThreadExecutor()
    fun publish(context: Context, endpoint: String) {
        val url = SecureStore(context).pushRegistryUrl.ifBlank { return }
        io.execute {
            runCatching {
                val body = endpoint.toRequestBody("text/plain".toMediaTypeOrNull())
                Net.builder().build().newCall(Request.Builder().url(url).post(body).build())
                    .execute().close()
            }
        }
    }
}
