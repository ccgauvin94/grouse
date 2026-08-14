package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Test

/** ACP byte-stream framing: newline-delimited JSON-RPC messages, matching the
 *  agent-client-protocol ByteStreams component (BufReader::lines + trailing \n). */
class RoamFrameCodecTest {

    @Test
    fun `whole frame decodes`() {
        val c = RoamFrameCodec()
        val text = """{"jsonrpc":"2.0","id":1,"method":"x"}"""
        val bytes = (text + "\n").toByteArray()
        val frames = c.feed(bytes, bytes.size)
        assertEquals(1, frames.size)
        assertEquals(text, frames[0])
    }

    @Test
    fun `frame split across feeds stays buffered until the newline`() {
        val c = RoamFrameCodec()
        val bytes = """{"jsonrpc":"2.0","id":2}""".toByteArray()
        assertEquals(0, c.feed(bytes, 5).size)          // partial, no newline yet
        assertEquals(0, c.feed(bytes.copyOfRange(5, bytes.size), bytes.size - 5).size)
        assertEquals(listOf("""{"jsonrpc":"2.0","id":2}"""), c.feed("\n".toByteArray(), 1))
    }

    @Test
    fun `several frames in one chunk`() {
        val c = RoamFrameCodec()
        val bytes = """{"a":1}""" + "\n" + """{"a":2}""" + "\n"
        val frames = c.feed(bytes.toByteArray(), bytes.length)
        assertEquals(listOf("""{"a":1}""", """{"a":2}"""), frames)
    }

    @Test
    fun `utf8 multibyte content survives the round trip`() {
        val c = RoamFrameCodec()
        val text = """{"text":"héllo — 世界"}"""
        val bytes = c.encode(text)
        val frames = c.feed(bytes, bytes.size)
        assertEquals(listOf(text), frames)
    }

    @Test
    fun `crlf tolerated`() {
        val c = RoamFrameCodec()
        val bytes = """{"a":1}""" + "\r\n"
        val frames = c.feed(bytes.toByteArray(), bytes.length)
        assertEquals(listOf("""{"a":1}"""), frames)
    }

    @Test
    fun `encode appends exactly one newline`() {
        val c = RoamFrameCodec()
        val bytes = c.encode("""{"x":null}""")
        assertEquals("""{"x":null}""" + "\n", String(bytes))
    }

    @Test
    fun `empty feed yields no frames`() {
        assertEquals(0, RoamFrameCodec().feed(ByteArray(0), 0).size)
    }
}
