// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import id.gauvin.grouse.ui.theme.statusColors

/** What the connection dot says about the wire carrying the chat on screen.
 *
 *  Three states, because a colour carries three — and because the words that used to say
 *  this (`Offline · tap to reconnect`) REPLACED the chat title, so a dropped socket cost
 *  the user the name of the conversation they were in. The dot says it instead, and the
 *  title never moves. */
enum class LinkState { Connected, Retrying, Down }

/** The main connection's dot state.
 *
 *  `failed` is the app's own auto-reconnect running out of attempts — not a single failed
 *  try, since the core retries on its own before the app's loop even starts. A wire that is
 *  retrying again outranks it, so a foreground return that re-arms the loop goes back to
 *  blinking rather than staying red until someone taps. */
fun mainLinkState(online: Boolean, retrying: Boolean, failed: Boolean): LinkState = when {
    online -> LinkState.Connected
    retrying -> LinkState.Retrying
    failed -> LinkState.Down
    // Nothing has said anything yet (cold start, or a first connect): an attempt is
    // implied, so blink rather than claiming a failure nobody has observed.
    else -> LinkState.Retrying
}

/** A roam peer's dot state, straight from the core's status string for that peer:
 *  `ready` / `connecting: …` / `disconnected` / `error: …` (CONTRACT §6). A peer the user
 *  disconnected on purpose reads as `disconnected` and shows Down, which is honest — the
 *  chat on screen has no wire either way. */
fun roamLinkState(status: String?): LinkState = when {
    status == "ready" -> LinkState.Connected
    status != null && status.startsWith("connecting") -> LinkState.Retrying
    else -> LinkState.Down
}

/** The app's own auto-reconnect schedule, in milliseconds between attempts.
 *
 *  Only used once the core's supervisor has given up: Android freezes the process before
 *  the core can spend its budget, so after a pocket-length sleep the socket is dead and
 *  every core retry is already used. The first waits are short because the common case is
 *  a WiFi interface that reassociates in a second or two; the tail is long enough to ride
 *  out a lift/underground gap. Bounded — when it runs out the dot goes red and the user
 *  decides. */
val LINK_RETRY_DELAYS_MS = longArrayOf(1_000, 2_000, 5_000, 10_000, 20_000, 30_000, 30_000)

/** Is this failure one that retrying cannot fix?
 *
 *  The host ANSWERED and refused us (a rotated secret key, a cert the app will not trust).
 *  Retrying those just hammers the server and moves a red dot further away than it needs
 *  to be. A timeout, a refused connection or a dropped socket all get the full schedule:
 *  those are exactly what a sleeping phone produces. */
fun linkFailureIsTerminal(message: String): Boolean {
    val m = message.lowercase()
    return listOf(
        "401", "403", "unauthorized", "forbidden", "invalid secret", "secret key",
        "certificate", "x509", "unknown issuer", "self-signed", "tls handshake",
    ).any { m.contains(it) }
}

/** The connection dot: green when the wire is up, slowly blinking amber while a reconnect
 *  is being attempted, red once the automatic attempts have run out.
 *
 *  `onClick` makes it the whole connection UI: tapping reports on the wire it describes
 *  (main connection settings, or that roam endpoint's page) rather than acting on it — the
 *  old tap-to-reconnect on the TITLE both hid the chat name and reconnected into a fresh
 *  session. The touch target is padded out from the dot, which is far too small to hit. */
@Composable
fun LinkDot(
    state: LinkState,
    onClick: (() -> Unit)? = null,
    dotSize: Dp = 10.dp,
    touchSize: Dp = 24.dp,
    modifier: Modifier = Modifier,
) {
    val color = when (state) {
        LinkState.Connected -> MaterialTheme.statusColors.online
        LinkState.Retrying -> MaterialTheme.statusColors.connecting
        LinkState.Down -> MaterialTheme.statusColors.offline
    }
    // A slow blink, not a pulse: "something is being tried" should be visible out of the
    // corner of the eye without pulling the eye off the conversation. Only Retrying
    // animates — a red dot that kept blinking would read as still-working.
    val alpha = if (state == LinkState.Retrying) {
        val transition = rememberInfiniteTransition(label = "linkDot")
        val a by transition.animateFloat(
            initialValue = 1f,
            targetValue = 0.3f,
            animationSpec = infiniteRepeatable(tween(durationMillis = 900), RepeatMode.Reverse),
            label = "linkDotAlpha",
        )
        a
    } else 1f
    Box(
        modifier = modifier
            .size(touchSize)
            .let { if (onClick != null) it.clickable(onClick = onClick) else it },
        contentAlignment = Alignment.Center,
    ) {
        Box(Modifier.size(dotSize).alpha(alpha).background(color, CircleShape))
    }
}
