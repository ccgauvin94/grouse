// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import id.gauvin.grouse.ConnectionManager.Companion.chatMatches
import id.gauvin.grouse.ConnectionManager.Companion.peerMatches
import id.gauvin.grouse.ConnectionManager.Companion.queryMatches
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Drawer search (tier 1). The filter runs over whatever `session/list` already gave us, so
 *  these rules ARE the feature: what a term has to match, and what a miss looks like. Both are
 *  easy to break silently — a change to the AND/OR of terms, or dropping the snippet from the
 *  haystack, still renders a plausible-looking list. */
class DrawerSearchTest {

    private fun chat(title: String = "", snippet: String = "", projectId: String? = null) =
        SessionInfo(sessionId = "s:" + title + snippet, title = title, updatedAt = "",
            messageCount = 1, model = "", snippet = snippet, projectId = projectId)

    @Test
    fun `blank query matches everything - the unfiltered drawer runs through here too`() {
        val c = chat(title = "Bird feeder cam")
        assertTrue(chatMatches("", c))
        assertTrue(chatMatches("   ", c))
        assertTrue(queryMatches("  ", listOf(null, null)))
    }

    @Test
    fun `title is searched case-insensitively and on substrings`() {
        val c = chat(title = "Bird Feeder Cam")
        assertTrue(chatMatches("feeder", c))
        assertTrue(chatMatches("FEED", c))
        assertFalse(chatMatches("feeder cam2", c))
    }

    @Test
    fun `terms AND together, so a two-word query narrows instead of widening`() {
        val c = chat(title = "bird-feeder cam")
        assertTrue(chatMatches("feeder cam", c))
        assertFalse("a term nothing matches must exclude the chat", chatMatches("feeder door", c))
    }

    /** The last-message snippet is often the ONLY scent that identifies a chat — most titles
     *  here are auto-summarized and several read alike. */
    @Test
    fun `the last-message snippet counts as a match`() {
        assertTrue(chatMatches("mqtt", chat(title = "Chat 3", snippet = "broker keeps dropping mqtt")))
    }

    /** Searching a project's NAME should find the chats filed under it: "grouse" is expected to
     *  bring up the grouse project's chats, none of which contain the word in their title. */
    @Test
    fun `the group name counts, which is how searching a project finds its chats`() {
        val c = chat(title = "Fix the flicker", projectId = "p1")
        assertTrue(chatMatches("grouse", c, group = "grouse"))
        assertFalse(chatMatches("grouse", c, group = "home-server"))
        // No group supplied (a free chat) must not match on nothing.
        assertFalse(chatMatches("grouse", c))
    }

    /** A null haystack (no snippet, no project) must not match a term, and must not throw —
     *  SessionInfo.snippet defaults to "" and projectId to null. */
    @Test
    fun `null haystacks are skipped not dereferenced`() {
        assertFalse(queryMatches("anything", listOf(null, "")))
        assertTrue(queryMatches("", listOf<String?>(null)))
    }

    @Test
    fun `an endpoint card stays if its name matches or any of its chats do`() {
        val laptopChat = chat(title = "Deploy the quadlet")
        assertTrue("peer name alone is a hit, even with no matching chat",
            peerMatches("laptop", "laptop", emptyList()))
        assertTrue(peerMatches("quadlet", "laptop", listOf(laptopChat)))
        assertFalse(peerMatches("kitchen", "laptop", listOf(laptopChat)))
    }

    /** Stray spaces are typed constantly. A term list that keeps empty terms would widen the
     *  AND to "matches everything", and a query that is only spaces must neither match-all nor
     *  match-none — it must be the same as no query at all. */
    @Test
    fun `stray whitespace is normalized, not treated as a term`() {
        val c = chat(title = "Bird feeder cam")
        assertTrue(chatMatches("  ", c))
        assertTrue(chatMatches(" feeder", c))
        assertTrue(chatMatches("feeder  cam", c))
        assertFalse(chatMatches("  zzz  ", c))
    }
}
