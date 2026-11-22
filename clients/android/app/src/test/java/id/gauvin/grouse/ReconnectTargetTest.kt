// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import id.gauvin.grouse.ConnectionManager.Companion.resumeTargetFor
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * What a main-wire reconnect is allowed to resume.
 *
 * 2026-11-22: "when I leave the app and reopen it, it always opens the last non-roam
 * chat". Coming back to the foreground reconnected the main wire with the last MAIN chat
 * as its resume target, even while a roam chat was on screen — and resuming binds, which
 * the core treats as displaying, so the peer's transcript was replaced by that chat.
 *
 * The rule is therefore: the chat on screen, and only when the main wire owns it. Null is
 * the ATTACH case (`connect_attach`): bring the wire up bound to nothing, because a
 * session `list`, projects and My Day all work without one and nothing gets displayed.
 */
class ReconnectTargetTest {

    @Test
    fun `a main chat on screen is resumed`() {
        assertEquals("20260917_8", resumeTargetFor("20260917_8"))
    }

    @Test
    fun `a peer's chat is never resumed on the main wire`() {
        assertNull(resumeTargetFor("roam:Phaethon:20260822_1"))
    }

    @Test
    fun `nothing on screen attaches`() {
        assertNull(resumeTargetFor(null))
    }
}
