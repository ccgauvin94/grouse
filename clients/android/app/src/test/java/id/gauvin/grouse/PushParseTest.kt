package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Test

/** Push envelope parsing: {type, session, text}, with plain or malformed text falling through
 *  to a briefing. Pure function, extracted from GoosePushService so it is JVM-testable. */
class PushParseTest {

    @Test
    fun `turn envelope carries session and text`() {
        val (type, session, text) = parsePush("""{"type":"turn","session":"s-42","text":"Done!"}""")
        assertEquals("turn", type)
        assertEquals("s-42", session)
        assertEquals("Done!", text)
    }

    @Test
    fun `briefing envelope has no session`() {
        val (type, session, text) = parsePush("""{"type":"briefing","text":"Server update"}""")
        assertEquals("briefing", type)
        assertEquals(null, session)
        assertEquals("Server update", text)
    }

    @Test
    fun `plain text is treated as a briefing`() {
        val (type, session, text) = parsePush("hello from the server")
        assertEquals(null, type)
        assertEquals(null, session)
        assertEquals("hello from the server", text)
    }

    @Test
    fun `malformed json falls through to raw text`() {
        val (type, session, text) = parsePush("{oops")
        assertEquals(null, type)
        assertEquals(null, session)
        assertEquals("{oops", text)
    }

    @Test
    fun `missing text field uses the raw payload`() {
        val (type, session, text) = parsePush("""{"type":"turn","session":"s1"}""")
        assertEquals("turn", type)
        assertEquals("s1", session)
        assertEquals("""{"type":"turn","session":"s1"}""", text)
    }
}
