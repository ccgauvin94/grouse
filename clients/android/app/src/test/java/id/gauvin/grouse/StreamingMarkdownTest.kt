// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Test

/** Where a streaming reply stops being "written" and becomes "settled".
 *
 *  A wrong answer here is not cosmetic: the settled prefix is the part that gets parsed as
 *  Markdown once and then skipped, so settling too late re-parses the whole reply per token
 *  (the stutter this replaces) and settling too early cuts a code block in half and shows the
 *  rest as plain text. Framework-free, like the other parser tests.
 */
class StreamingMarkdownTest {

    private fun settled(text: String) = text.substring(0, settledParagraphEnd(text))
    private fun tail(text: String) = text.substring(settledParagraphEnd(text))

    @Test
    fun `nothing is settled until the reply closes a block`() {
        assertEquals(0, settledParagraphEnd(""))
        assertEquals(0, settledParagraphEnd("A single line still being writ"))
        assertEquals(0, settledParagraphEnd("line one\nline two"))
        // A single newline is not a paragraph break in Markdown, so it settles nothing.
        assertEquals(0, settledParagraphEnd("line one\n"))
    }

    @Test
    fun `a blank line settles everything before it and leaves the rest in flight`() {
        val text = "First paragraph.\n\nSecond para"
        assertEquals("First paragraph.\n\n", settled(text))
        assertEquals("Second para", tail(text))
    }

    @Test
    fun `the newest paragraph is always the one in flight`() {
        val text = "one\n\ntwo\n\nthree"
        assertEquals("one\n\ntwo\n\n", settled(text))
        assertEquals("three", tail(text))
    }

    @Test
    fun `a trailing blank line settles the lot`() {
        val text = "one\n\ntwo\n\n"
        assertEquals(text, settled(text))
        assertEquals("", tail(text))
    }

    @Test
    fun `a blank line inside an open fence is code, not a break`() {
        val text = "```\nfun main() {\n\n    println(\"hi\")\n}\n```\n"
        // Sliced at the blank line this would re-parse the block on every following token.
        assertEquals(0, settledParagraphEnd(text))
    }

    @Test
    fun `a closed fence hands the next paragraph the settled region`() {
        val text = "```\ncode\n```\n\nafter the block"
        assertEquals("```\ncode\n```\n\n", settled(text))
        assertEquals("after the block", tail(text))
    }

    @Test
    fun `an indented fence counts, the way the renderer sees it`() {
        // splitFencedCode trims leading whitespace before matching, so this scan must too.
        val text = "  ```\n  code\n\n  more\n  ```\n\ntail"
        assertEquals("  ```\n  code\n\n  more\n  ```\n\n", settled(text))
        assertEquals("tail", tail(text))
    }

    @Test
    fun `two fences pair up and do not swallow the paragraph after them`() {
        val text = "```\na\n```\n\nmid\n\n```\nb\n```\n\nend"
        assertEquals("```\na\n```\n\nmid\n\n```\nb\n```\n\n", settled(text))
        assertEquals("end", tail(text))
    }

    @Test
    fun `a roaming partial fence does not fake a close`() {
        // The opening fence alone: everything after it is still inside the block.
        assertEquals(0, settledParagraphEnd("```\n\nstill opening"))
    }
}
