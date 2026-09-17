// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import id.gauvin.grouse.ConnectionManager.Companion.turnOwnerMatches
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Which wire may release a wedged in-flight turn.
 *
 * 2026-09-16: a roam host was killed mid-turn. Nothing the client can receive ends that
 * turn — the peer's RunEnded died with the connection, and a killed host emits neither
 * that nor an empty run id — so `busy` stayed true, every later send was parked behind a
 * turn that could never finish, and the composer kept saying "Queue message…" with no
 * error. The user's stop messages sat in the client for an hour.
 *
 * The release rule has to be the OWNER's wire, though: clearing the turn on any drop
 * would let an unrelated chat's disconnect unblock a turn that is genuinely still running
 * elsewhere (and the queue would then send into a live turn, interleaving transcripts).
 */
class TurnLivenessTest {

    @Test
    fun `the owning wire releases the turn`() {
        assertTrue(
            turnOwnerMatches(
                turnInFlightSession = "roam:Phaethon:20260822_1",
                currentSession = "20260917_8",
                lostSessionId = "roam:Phaethon:20260822_1",
            )
        )
    }

    @Test
    fun `a drop in another chat must not release the turn`() {
        assertFalse(
            turnOwnerMatches(
                turnInFlightSession = "roam:Phaethon:20260822_1",
                currentSession = "roam:Phaethon:20260822_1",
                lostSessionId = "20260917_8",
            )
        )
    }

    @Test
    fun `an unrecorded owner falls back to the chat on screen`() {
        // busy was armed (send() sets it) but the turn's routing was never recorded —
        // the on-screen chat is the one whose composer is stuck, so its wire may release.
        assertTrue(
            turnOwnerMatches(
                turnInFlightSession = null,
                currentSession = "roam:Phaethon:20260822_1",
                lostSessionId = "roam:Phaethon:20260822_1",
            )
        )
        assertFalse(
            turnOwnerMatches(
                turnInFlightSession = null,
                currentSession = "20260917_8",
                lostSessionId = "roam:Phaethon:20260822_1",
            )
        )
    }

    @Test
    fun `nothing to release with no session at all`() {
        assertFalse(turnOwnerMatches(null, null, "roam:Phaethon:20260822_1"))
    }
}
