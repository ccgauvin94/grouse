// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** ChatMessage identity: the chat LazyColumn keys on id, and streaming updates copy() the
 *  message — so the id must survive copy() or every streaming delta rebuilds the bubble. */
class ChatMessageTest {

    @Test
    fun `copy preserves the id so streaming keeps the same composition`() {
        val original = ChatMessage(role = "assistant", text = "Hel")
        val updated = original.copy(text = "Hello")
        assertEquals(original.id, updated.id)
    }

    @Test
    fun `ids are unique and increasing`() {
        val a = ChatMessage(role = "user", text = "a")
        val b = ChatMessage(role = "assistant", text = "b")
        val c = ChatMessage(role = "tool", text = "c")
        assertTrue(a.id < b.id)
        assertTrue(b.id < c.id)
        assertNotEquals(a.id, b.id)
    }

    @Test
    fun `defaults leave tool fields empty`() {
        val m = ChatMessage(role = "tool", text = "")
        assertEquals("", m.detail)
        assertEquals("", m.toolCallId)
        assertEquals("", m.status)
        assertEquals("", m.output)
        assertEquals("", m.appKey)
        assertEquals("", m.appHtml)
    }
}
