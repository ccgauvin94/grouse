// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The two pure decisions behind the chat's scroll behaviour: where a returning reader was,
 *  and when a live reply outgrowing the screen unpins the follower. No Compose here. */
class ChatStateTest {

    private fun frame(size: Int, viewport: Int = 100, streamId: Long? = 7L) =
        EndFrame(count = 10, viewport = viewport, streamId = streamId, size = size)

    @Test
    fun `reading position round trips through its saveable form`() {
        for (position in listOf(ReadingPosition(0, 0), ReadingPosition(42, 317))) {
            assertEquals(position, ReadingPosition.decode(position.encode()))
        }
    }

    @Test
    fun `a restored history position is not the bottom`() {
        assertTrue(ReadingPosition(0, 0).atBottom)
        assertFalse(ReadingPosition(3, 0).atBottom)
        assertFalse(ReadingPosition(0, 12).atBottom)
    }

    @Test
    fun `garbage saved state is ignored rather than scrolled to`() {
        assertNull(ReadingPosition.decode(null))
        assertNull(ReadingPosition.decode(""))
        assertNull(ReadingPosition.decode("1"))
        assertNull(ReadingPosition.decode("a:2"))
        assertNull(ReadingPosition.decode("-1:2"))
    }

    @Test
    fun `the follower latches on the crossing, not on being over a screen`() {
        assertFalse(crossedStreamingViewport(frame(90), false))
        assertTrue(crossedStreamingViewport(frame(100), false))
        assertFalse(crossedStreamingViewport(frame(140), true))
        // Re-pinned by a tap: the next token must not immediately unpin again.
        assertTrue(crossedStreamingViewport(frame(140), false))
    }

    @Test
    fun `no stream, no measured row and no viewport cannot latch`() {
        assertFalse(crossedStreamingViewport(frame(200, streamId = null), false))
        assertFalse(crossedStreamingViewport(frame(-1), false))
        assertFalse(crossedStreamingViewport(frame(200, viewport = 0), false))
    }
}
