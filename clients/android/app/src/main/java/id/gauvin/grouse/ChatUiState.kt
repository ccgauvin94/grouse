// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

/** Assistant briefing health, read by both the header icon and the schedule dialog.
 *  Kept as one value so the two cannot disagree about what "healthy" means. */
internal data class ChatBriefing(
    val ago: String,
    val healthy: Boolean,
    val late: Boolean,
    val lastAt: Long,
)

/** The screen-level facts shared by the header, transcript and overlays. Mutable UI flags
 *  stay in [ChatScreen] and reach the children as explicit lambdas; this only carries the
 *  values more than one child reads. */
internal data class ChatUiState(
    val link: LinkState,
    val roamPeer: String?,
    val debugTools: Boolean,
    val briefing: ChatBriefing,
    val currentModel: String,
)
