// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse.ui.theme

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.unit.dp

/**
 * Component shape tokens — mapped from design/tokens.json `component.*` (radius values and
 * the chatBubble corner list). Consumed instead of raw dp/radius literals at the component
 * sites the audit flagged (A-6): the composer, tool chips, and the user bubble. A full
 * dp-literal sweep of every surface is a separate pass; this scopes to those three.
 */
object GrouseShapes {
    /** component.composer.radius — the chat composer's single rounded field. */
    val composer = RoundedCornerShape(28.dp)

    /** component.chatBubble.toolChip.radius — collapsed tool-call chips. */
    val toolChip = RoundedCornerShape(14.dp)

    /** component.chatBubble.user.corners [16,16,4,16] — right-aligned user bubble with a tail corner. */
    val userBubble = RoundedCornerShape(
        topStart = 16.dp, topEnd = 16.dp, bottomStart = 4.dp, bottomEnd = 16.dp,
    )
}
