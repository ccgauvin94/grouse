// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The plan handshake, and the mode a session row reports.
 *
 * These are the two pure bits of dev mode: the sentinel the model writes and the tag the
 * core derives. Both decide UI behaviour (does the plan popup open? does the row carry the
 * `</>` mark?), so they are the parts worth pinning without a device.
 */
class PlanModeTest {
    @Test
    fun sentinelIsRecognisedOnItsOwnLine() {
        assertEquals("1. Do it\n2. Verify it",
            stripPlanSentinel("1. Do it\n2. Verify it\n[plan-ready]"))
        // Case and surrounding whitespace follow the core's own rule; the plan's own
        // text is otherwise passed through untouched.
        assertEquals("the plan", stripPlanSentinel("the plan\n  [PLAN-READY]  "))
    }

    @Test
    fun sentinelInsideProseIsContentNotASignal() {
        // The core matches a WHOLE line. A model explaining the marker must not trip the
        // popup, and its sentence must survive stripping.
        val prose = "I will write [plan-ready] when the plan is finished."
        assertEquals(prose, stripPlanSentinel(prose))
    }

    @Test
    fun plainTextIsUntouched() {
        val reply = "Two options:\n1. Keep the cache\n2. Drop it"
        assertEquals(reply, stripPlanSentinel(reply))
    }

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
