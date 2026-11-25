// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import android.content.Context
import android.os.Handler
import android.os.PowerManager

/** Delimiter that cannot appear in a session id or a run id, so roam ids
 *  (`roam:label:id`) can be owned without confusing the owner-key parser. */
private const val WAKE_FIELD = '\u001f'

/** Owner key for one reason a turn is holding the CPU awake. The id makes
 *  overlapping owners distinct; the session keeps release scoped to one chat. */
internal fun wakeOwnerKey(kind: String, sessionId: String, id: String): String =
    listOf(kind, sessionId, id).joinToString(WAKE_FIELD.toString())

/** True when [key] belongs to [sessionId]. Session ids are compared whole,
 *  including the colons in `roam:<label>:<id>`, rather than by splitting them. */
internal fun wakeOwnerMatchesSession(key: String, sessionId: String): Boolean {
    val fields = key.split(WAKE_FIELD, limit = 3)
    return fields.size == 3 && fields[1] == sessionId
}

/** True when [key] belongs to the given owner kind (turn send, voice, ...). */
internal fun wakeOwnerHasKind(key: String, kind: String): Boolean {
    val fields = key.split(WAKE_FIELD, limit = 3)
    return fields.size == 3 && fields[0] == kind
}

/**
 * Keeps a partial wake lock held only while a turn has an owner.
 *
 * A foreground service keeps the PROCESS from being reclaimed, but it does not
 * keep the CPU awake: with the screen off Android can still freeze the app hard
 * enough to miss a WebSocket pong or a streamed tool/thinking frame. Goose then
 * sees the ACP connection die and cancels the run. This lock is deliberately
 * narrow — acquired on send / live-run adoption, released on run end or wire
 * loss — so we do not hold a CPU lock merely because "persistent connection" is
 * enabled. A short release grace covers the reconnect that follows a lost wire.
 */
internal class TurnWakeLock(
    context: Context,
    private val handler: Handler,
    private val log: (String) -> Unit,
) {
    private val power = context.applicationContext.getSystemService(Context.POWER_SERVICE) as PowerManager
    private val lock = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "grouse:turn").apply {
        // Explicit release only; repeated acquire() calls would otherwise stack refcounts.
        setReferenceCounted(false)
    }

    private val owners = LinkedHashSet<String>()
    private var held = false
    private var releasePending = false

    private fun arm(timeoutMs: Long) {
        if (held) runCatching { lock.release() }
        lock.acquire(timeoutMs)
        held = true
    }

    private fun scheduleRenew() {
        handler.removeCallbacks(renew)
        handler.postDelayed(renew, RENEW_MS)
    }

    private val renew = object : Runnable {
        override fun run() {
            synchronized(this@TurnWakeLock) {
                if (!held || owners.isEmpty()) {
                    releasePending = false
                    return@synchronized
                }
                arm(HOLD_MS)
                scheduleRenew()
                log("wake: renew owners=${owners.size}")
            }
        }
    }

    private val delayedRelease = Runnable {
        synchronized(this@TurnWakeLock) {
            releasePending = false
            if (owners.isEmpty() && held) {
                handler.removeCallbacks(renew)
                runCatching { lock.release() }
                held = false
                log("wake: released (grace expired)")
            }
        }
    }

    /** Schedule (or cancel) the bounded tail after the last owner goes away. */
    private fun settle(reason: String, immediate: Boolean) {
        if (owners.isNotEmpty() || !held) return
        handler.removeCallbacks(renew)
        handler.removeCallbacks(delayedRelease)
        if (immediate) {
            runCatching { lock.release() }
            held = false
            log("wake: released reason=$reason")
            return
        }
        releasePending = true
        handler.postDelayed(delayedRelease, RELEASE_GRACE_MS)
        log("wake: release scheduled in ${RELEASE_GRACE_MS / 1000}s reason=$reason")
    }

    fun acquire(sessionId: String, kind: String, id: String) {
        val owner = wakeOwnerKey(kind, sessionId, id)
        synchronized(this) {
            handler.removeCallbacks(delayedRelease)
            releasePending = false
            owners.add(owner)
            if (!held) arm(HOLD_MS)
            scheduleRenew()
            log("wake: acquire ${display(owner)} owners=${owners.size}")
        }
    }

    fun release(owner: String, reason: String, immediate: Boolean = false) {
        synchronized(this) {
            if (owners.remove(owner)) log("wake: release ${display(owner)} reason=$reason")
            settle(reason, immediate)
        }
    }

    fun releaseSession(sessionId: String, reason: String, immediate: Boolean = false) {
        synchronized(this) {
            val before = owners.size
            owners.removeAll { wakeOwnerMatchesSession(it, sessionId) }
            if (owners.size != before) log("wake: release session=$sessionId reason=$reason")
            settle(reason, immediate)
        }
    }

    /** Release every owner except a protected kind. Barge-in cancels the ACP
     *  turn but leaves the still-open microphone holding its bounded lock. */
    fun releaseExceptKind(kind: String, reason: String, immediate: Boolean = false) {
        synchronized(this) {
            val before = owners.size
            owners.removeAll { !wakeOwnerHasKind(it, kind) }
            if (owners.size != before) log("wake: release except kind=$kind reason=$reason")
            settle(reason, immediate)
        }
    }

    fun releaseAll(reason: String, immediate: Boolean = false) {
        synchronized(this) {
            if (owners.isNotEmpty()) log("wake: release all owners=${owners.size} reason=$reason")
            owners.clear()
            settle(reason, immediate)
        }
    }

    fun snapshot(): String = synchronized(this) {
        if (owners.isEmpty() && !held) return@synchronized "off"
        buildString {
            append(if (held) "held" else "idle")
            append(" owners=").append(owners.size)
            if (releasePending) append(" releasing")
            if (owners.isNotEmpty()) {
                append('[')
                append(owners.joinToString(", ") { display(it) })
                append(']')
            }
        }
    }

    private fun display(owner: String): String {
        val fields = owner.split(WAKE_FIELD, limit = 3)
        if (fields.size != 3) return owner.take(60)
        val sid = fields[1]
        val shortSid = if (sid.length > 42) sid.take(20) + "…" + sid.takeLast(18) else sid
        return "${fields[0]}:$shortSid:${fields[2].take(8)}"
    }

    companion object {
        private const val HOLD_MS = 10 * 60 * 1000L
        private const val RENEW_MS = 2 * 60 * 1000L
        private const val RELEASE_GRACE_MS = 45_000L
    }
}
