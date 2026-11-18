// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The mode a session row reports.
 *
 * Plan mode used to live here too; it is gone (2026-11-17). A mode is the only durable
 * dev-state a client has, and it comes from the server's tag walk — the rest was a local
 * mirror that could be silently wrong, which is why it was removed rather than patched.
 */
class DevModeTest {
    @Test
    fun aDevSessionRowIsFlaggedByItsMode() {
        val row = SessionInfo(
            sessionId = "s1", title = "Dev chat", updatedAt = "", messageCount = 0,
            model = "", mode = "dev",
        )
        assertTrue(row.isDev)
        // The effective mode is never blank, but an older core could send nothing: that is
        // a plain chat, not a dev one.
        assertFalse(row.copy(mode = "chat").isDev)
        assertFalse(row.copy(mode = "").isDev)
    }
}
