// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** The streaming settle scanner and the code-block renderer must use ONE fence grammar.
 *
 *  They used to disagree: the renderer split `~~~` blocks, the scanner only toggled on
 *  backticks, so a blank line inside a tilde block "settled" the paragraph and the block
 *  re-parsed on every subsequent token. Framework-free, like the other parser tests. */
class FenceMatcherTest {

    @Test
    fun `tilde fence with a blank line is code to both consumers`() {
        val text = "~~~\nlet x = 1\n\nstill code\n~~~"
        assertEquals(0, settledParagraphEnd(text))
        val segments = splitFencedCode(text)
        assertEquals(1, segments.size)
        assertTrue(segments[0] is Segment.Code)
        assertEquals("let x = 1\n\nstill code", (segments[0] as Segment.Code).code)
    }

    @Test
    fun `backtick fence still pairs and releases the settled suffix`() {
        val text = "```kotlin\nval x = 1\n```\n\nafter"
        assertEquals("```kotlin\nval x = 1\n```\n\n", text.substring(0, settledParagraphEnd(text)))
        val segments = splitFencedCode(text)
        assertEquals(2, segments.size)
        assertEquals("val x = 1", (segments[0] as Segment.Code).code)
        assertEquals("kotlin", (segments[0] as Segment.Code).tag)
        assertEquals("after", (segments[1] as Segment.Text).text)
    }

    @Test
    fun `indentation up to three columns is a fence for both consumers`() {
        val text = "   ~~~\n   code\n\n   more\n   ~~~\n\ntail"
        assertEquals("   ~~~\n   code\n\n   more\n   ~~~\n\n",
            text.substring(0, settledParagraphEnd(text)))
        assertTrue(splitFencedCode(text).any { it is Segment.Code })
        assertTrue(isFenceOpen("  ```"))
        assertTrue(isFenceOpen("\t~~~"))
    }

    @Test
    fun `four columns of indentation is text for both consumers`() {
        val text = "    ```\n\nmore"
        assertEquals("    ```\n\n", text.substring(0, settledParagraphEnd(text)))
        assertFalse(splitFencedCode(text).any { it is Segment.Code })
        assertFalse(isFenceOpen("    ```"))
    }

    @Test
    fun `a different marker inside a fence is code, not a close`() {
        val text = "```\ncode\n~~~\n\nstill code\n```\n\nafter"
        assertEquals("```\ncode\n~~~\n\nstill code\n```\n\n",
            text.substring(0, settledParagraphEnd(text)))
        assertFalse(isFenceClose("~~~", "```"))
        assertTrue(isFenceClose("   ```   ", "```"))
        assertTrue(isFenceClose("~~~~", "~~~"))
        assertFalse(isFenceClose("~~~", "````"))
    }

    @Test
    fun `the predicates and the splitter agree on every marker run`() {
        for (line in listOf("```", "````", "~~~", "~~~~~", "```kotlin", "~~~js extra", "``", "~~")) {
            assertEquals(line, isFenceOpen(line), splitFencedCode("$line\nbody\n$line")
                .any { it is Segment.Code })
        }
    }
}
