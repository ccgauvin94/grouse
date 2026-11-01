// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** The header dot's three states, and the retry schedule behind the amber one.
 *
 *  This is the whole of the app's connection UI now: the title no longer changes, so a wrong
 *  mapping here is the user losing the only signal that the wire is down. Framework-free by
 *  design, like the other parser tests.
 */
class LinkStatusTest {

    // --- main connection -----------------------------------------------------

    @Test
    fun `a live wire is green whatever the flags say`() {
        assertEquals(LinkState.Connected, mainLinkState(online = true, retrying = true, failed = true))
        assertEquals(LinkState.Connected, mainLinkState(online = true, retrying = false, failed = false))
    }

    @Test
    fun `retrying blinks, and outranks an earlier give-up`() {
        assertEquals(LinkState.Retrying, mainLinkState(online = false, retrying = true, failed = true))
        assertEquals(LinkState.Retrying, mainLinkState(online = false, retrying = true, failed = false))
    }

    @Test
    fun `the retries running out is the only red`() {
        assertEquals(LinkState.Down, mainLinkState(online = false, retrying = false, failed = true))
    }

    @Test
    fun `nothing said yet is a retry in progress, not a failure`() {
        // Cold start: no status, no Ready, no error. Claiming red here would accuse the
        // server before anything was even attempted.
        assertEquals(LinkState.Retrying, mainLinkState(online = false, retrying = false, failed = false))
    }

    // --- roam peers ----------------------------------------------------------

    @Test
    fun `peer status maps straight onto the dot`() {
        assertEquals(LinkState.Connected, roamLinkState("ready"))
        assertEquals(LinkState.Retrying, roamLinkState("connecting"))
        assertEquals(LinkState.Retrying, roamLinkState("connecting: dialing"))
        assertEquals(LinkState.Retrying, roamLinkState("connecting: handshake"))
        // The core's own supervised re-dial, which is the peer analogue of the app's loop.
        assertEquals(LinkState.Retrying, roamLinkState("connecting: reconnect 3"))
        assertEquals(LinkState.Down, roamLinkState("error: invalid card: bad base64"))
        assertEquals(
            LinkState.Down,
            roamLinkState("error: no reply from host — check the host accepted this device"),
        )
    }

    @Test
    fun `a peer with no wire of its own is red, deliberately disconnected included`() {
        assertEquals(LinkState.Down, roamLinkState("disconnected"))
        assertEquals(LinkState.Down, roamLinkState(null))
        assertEquals(LinkState.Down, roamLinkState(""))
    }

    // --- retry schedule ------------------------------------------------------

    @Test
    fun `the schedule retries quickly at first and gives up inside five minutes`() {
        // The common case is a WiFi interface that reassociates in a second or two, so the
        // first wait must be short; the tail rides out a lift. A schedule that grew into
        // the tens of minutes would leave a red dot as the only outcome.
        assertTrue("first wait should be about a second", LINK_RETRY_DELAYS_MS.first() <= 1_500)
        assertTrue("must be more than one attempt", LINK_RETRY_DELAYS_MS.size >= 4)
        assertTrue(
            "total budget was ${LINK_RETRY_DELAYS_MS.sum()}ms",
            LINK_RETRY_DELAYS_MS.sum() in 30_000..300_000,
        )
        assertTrue("no wait may exceed 60s", LINK_RETRY_DELAYS_MS.all { it <= 60_000 })
    }

    // --- which failures are worth retrying -----------------------------------

    @Test
    fun `a host that answered and refused us is not retried`() {
        // Retrying these hammers the server and moves the red dot further away than needed.
        assertTrue(linkFailureIsTerminal("internal error at connect: WebSocket connect failed: HTTP 401"))
        assertTrue(linkFailureIsTerminal("HTTP 403 forbidden"))
        assertTrue(linkFailureIsTerminal("unauthorized"))
        assertTrue(linkFailureIsTerminal("invalid secret key"))
        assertTrue(linkFailureIsTerminal("webpki: certificate has expired"))
        assertTrue(linkFailureIsTerminal("x509: unknown issuer"))
    }

    @Test
    fun `a socket that dropped or timed out is exactly what the schedule is for`() {
        assertFalse(linkFailureIsTerminal("WebSocket connect failed: timed out"))
        assertFalse(linkFailureIsTerminal("connect: connection refused"))
        assertFalse(linkFailureIsTerminal("connection reset by peer"))
        assertFalse(linkFailureIsTerminal(""))
    }
}
