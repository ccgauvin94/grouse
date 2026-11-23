// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.grouse_core.PushKind

/**
 * A push body is unauthenticated: the distributor endpoint is a bearer capability, so any
 * sender can name an arbitrary `session`. That id must never become the target of an inline
 * reply or a chat deep link unless it is a TURN for a session this device actually knows
 * and, when a turn was armed here, exactly the armed one.
 */
class PushSessionValidationTest {
    private val known = setOf("20260917_8", "roam:home:20260822_1")

    @Test
    fun `a known turn session is accepted`() {
        assertEquals("20260917_8", validatedPushSession(PushKind.TURN, "20260917_8", known, null))
    }

    @Test
    fun `a known peer-owned turn session is accepted`() {
        assertEquals("roam:home:20260822_1",
            validatedPushSession(PushKind.TURN, "roam:home:20260822_1", known, null))
    }

    @Test
    fun `surrounding whitespace around an id is harmless`() {
        assertEquals("20260917_8", validatedPushSession(PushKind.TURN, " 20260917_8 ", known, null))
    }

    @Test
    fun `the armed session narrows the target`() {
        assertEquals("20260917_8",
            validatedPushSession(PushKind.TURN, "20260917_8", known, "20260917_8"))
        assertNull(validatedPushSession(PushKind.TURN, "roam:home:20260822_1", known, "20260917_8"))
    }

    @Test
    fun `a blank armed session is no constraint`() {
        assertEquals("20260917_8", validatedPushSession(PushKind.TURN, "20260917_8", known, "  "))
    }

    @Test
    fun `an unknown session is rejected even when it matches the armed session`() {
        assertNull(validatedPushSession(PushKind.TURN, "attacker-chat", known, "attacker-chat"))
        assertNull(validatedPushSession(PushKind.TURN, "attacker-chat", known, null))
    }

    @Test
    fun `a session-less turn stays session-less`() {
        assertNull(validatedPushSession(PushKind.TURN, null, known, null))
        assertNull(validatedPushSession(PushKind.TURN, "  ", known, null))
    }

    @Test
    fun `briefings never carry a session`() {
        assertNull(validatedPushSession(PushKind.BRIEFING, "20260917_8", known, null))
        assertNull(validatedPushSession(PushKind.BRIEFING, "20260917_8", known, "20260917_8"))
    }

    @Test
    fun `task pushes never carry a session`() {
        assertNull(validatedPushSession(PushKind.TASKS, "20260917_8", known, null))
        assertNull(validatedPushSession(PushKind.TASKS, "roam:home:20260822_1", known, null))
    }

    @Test
    fun `an empty known set rejects every turn`() {
        assertNull(validatedPushSession(PushKind.TURN, "20260917_8", emptySet(), null))
    }
}
