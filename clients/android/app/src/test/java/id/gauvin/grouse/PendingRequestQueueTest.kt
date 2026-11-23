// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Server requests are parked by ID, not in one `firstOrNull()` slot. These are the pure
 *  queue/key helpers behind both the permission and the elicitation sheets: a second
 *  request must never drop the first, and answering one must not disturb the others. */
class PendingRequestQueueTest {

    @Test
    fun `two requests coexist in server order`() {
        val q = LinkedHashMap<String, String>()
        assertTrue(q.enqueueRequest("req-1", "first"))
        assertTrue(q.enqueueRequest("req-2", "second"))
        assertEquals(listOf("req-1" to "first", "req-2" to "second"),
            q.entries.map { it.key to it.value })
        assertEquals("req-1", q.oldestRequest()?.first)
    }

    @Test
    fun `a redelivered id does not replace the first request`() {
        val q = LinkedHashMap<String, String>()
        assertTrue(q.enqueueRequest("same", "first"))
        assertFalse(q.enqueueRequest("same", "replacement"))
        assertEquals("first", q["same"])
        assertEquals(1, q.size)
    }

    @Test
    fun `blank ids are rejected instead of collapsing every form into one`() {
        val q = LinkedHashMap<String, String>()
        assertFalse(q.enqueueRequest("  ", "nope"))
        assertTrue(q.isEmpty())
    }

    @Test
    fun `answering by id removes only that row`() {
        val q = LinkedHashMap<String, Int>()
        q.enqueueRequest("a", 1)
        q.enqueueRequest("b", 2)
        q.enqueueRequest("c", 3)
        assertEquals(2, q.takeRequest("b"))
        assertNull(q.takeRequest("b"))
        assertEquals(listOf("a", "c"), q.keys.toList())
        assertEquals("a", q.oldestRequest()?.first)
        assertEquals(3, q.takeRequest("c"))
        assertEquals("a", q.oldestRequest()?.first)
    }

    @Test
    fun `the queue is bounded and evicts the oldest request first`() {
        val q = LinkedHashMap<String, Int>()
        repeat(10) { q.enqueueRequest("r$it", it) }
        assertEquals(8, q.size)
        assertEquals("r2", q.oldestRequest()?.first)
        assertFalse(q.containsKey("r0"))
        assertFalse(q.containsKey("r1"))
        assertTrue(q.containsKey("r9"))
    }

    @Test
    fun `server id wins and the local counter is only a fallback`() {
        assertEquals("srv-42", serverRequestId("srv-42", "elicit", 7))
        assertEquals("elicit-7", serverRequestId("  ", "elicit", 7)) // blank is not an id
        assertEquals("elicit-7", serverRequestId(null, "elicit", 7))
        assertEquals("recipeparams-8", serverRequestId("", "recipeparams", 8))
    }

    @Test
    fun `owner line names the wire and the session`() {
        assertEquals("this device · 20260912_61", requestOwnerLabel("main", "20260912_61"))
        assertEquals("this device · 20260912_61", requestOwnerLabel("MAIN", " 20260912_61 "))
        assertEquals("peer Phaethon", requestOwnerLabel("roam:Phaethon", null))
        assertEquals("peer Phaethon · roam:Phaethon:s1",
            requestOwnerLabel("roam", "roam:Phaethon:s1"))
        assertEquals("roam peer · roam:Phaethon:s1", requestOwnerLabel(null, "roam:Phaethon:s1"))
        assertEquals("Phaethon", requestOwnerLabel("Phaethon", null))
        assertEquals("", requestOwnerLabel("", null))
    }

    @Test
    fun `pending rows carry the owner line the sheets display`() {
        val permission = PendingPermission(
            "tool-1",
            AcpEvent.Permission("tool-1", "Shell", "rm -rf /tmp/x", emptyList()),
            "roam:Phaethon",
            "roam:Phaethon:s1",
        )
        assertEquals("request id is the tool call id", "tool-1", permission.requestId)
        assertEquals("peer Phaethon · roam:Phaethon:s1", permission.ownerLabel)

        val form = PendingElicitation(
            "req-9",
            AcpEvent.Elicitation("req-9", "Input requested", "", emptyList()),
            "main",
        )
        assertEquals("req-9", form.requestId)
        assertEquals("this device", form.ownerLabel)
    }
}
