// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import id.gauvin.grouse.ConnectionManager.Companion.roamStatusDetail
import id.gauvin.grouse.ConnectionManager.Companion.roamStatusShort
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Endpoint status formatting. The drawer row puts this text on ONE line beside the
 *  endpoint name, and the name is the weight(1f) child — so anything long here starves
 *  the name to zero width and the row renders nameless. That is a real regression that
 *  shipped, hence the length ceiling below. */
class RoamStatusTest {

    /** The longest thing that may sit beside a name. "listing sessions" is the widest
     *  legitimate value; a raw transport error is ~70 chars and is what broke the row. */
    private val maxRowChars = ConnectionManager.ROAM_STATUS_MAX

    @Test
    fun `raw transport error collapses to a short label`() {
        val raw = "error: roam connect: connect: transport error: connect failed: timed out"
        assertEquals("no reply", roamStatusShort(raw))
        assertTrue(roamStatusShort(raw).length <= maxRowChars)
    }

    @Test
    fun `every status stays short enough to share a row with the name`() {
        listOf(
            null,
            "",
            "ready",
            "disconnected",
            "connecting: dialing",
            "connecting: handshake",
            "connecting: listing sessions",
            "error: roam connect: connect: transport error: connect failed: timed out",
            "error: no reply from host — check the host accepted this device and its relay is reachable",
            "error: roam dial task panicked: attempt to subtract with overflow",
            "error: invalid card: bad base64",
        ).forEach { st ->
            val short = roamStatusShort(st)
            assertTrue("too long for the row: '$short'", short.length <= maxRowChars)
            assertTrue("must not be blank for '$st'", short.isNotBlank())
        }
    }

    @Test
    fun `connecting phases surface the phase, not the prefix`() {
        assertEquals("dialing", roamStatusShort("connecting: dialing"))
        assertEquals("listing sessions", roamStatusShort("connecting: listing sessions"))
        // A bare "connecting" has no phase to show and must not render empty.
        assertEquals("connecting", roamStatusShort("connecting"))
    }

    @Test
    fun `steady states pass through`() {
        assertEquals("ready", roamStatusShort("ready"))
        assertEquals("disconnected", roamStatusShort("disconnected"))
        assertEquals("offline", roamStatusShort(null))
        assertEquals("offline", roamStatusShort(""))
    }

    @Test
    fun `detail explains errors in words, and only for errors`() {
        val timedOut = roamStatusDetail(
            "error: roam connect: connect: transport error: connect failed: timed out")
        assertTrue(timedOut!!.contains("No reply from the host"))
        // The point of the fix: no raw transport prose leaks into the message.
        assertTrue(!timedOut.contains("transport error"))

        assertTrue(roamStatusDetail("error: invalid card: bad base64")!!.contains("could not be decoded"))

        // Non-error states have nothing to explain.
        assertNull(roamStatusDetail("ready"))
        assertNull(roamStatusDetail("connecting: dialing"))
        assertNull(roamStatusDetail("disconnected"))
        assertNull(roamStatusDetail(null))
    }

    @Test
    fun `an unrecognised status is clamped, never passed through at length`() {
        val long = "some future status the core has not emitted before, at length"
        assertTrue(roamStatusShort(long).length <= maxRowChars)
        assertTrue(roamStatusShort(long).endsWith("…"))
    }

    @Test
    fun `an unrecognised error still says something`() {
        val d = roamStatusDetail("error: something nobody has seen before")
        assertEquals("something nobody has seen before", d)
        assertEquals("error", roamStatusShort("error: something nobody has seen before"))
    }
}
