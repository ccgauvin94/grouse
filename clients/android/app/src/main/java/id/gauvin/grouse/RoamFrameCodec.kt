package id.gauvin.grouse

import java.io.ByteArrayOutputStream

/** ACP byte-stream framing (the `ByteStreams` component of agent-client-protocol):
 *  one JSON-RPC message per line, newline-terminated — the same framing goose uses
 *  on stdio and on the roam transport (verified against the crate source: the Rust
 *  side does `BufReader.lines()` and writes `bytes + b'\n'`). JSON is serialized
 *  compactly, so a message can never contain a raw newline.
 *
 *  Pure + stateless-safe (feed any chunking): the JVM tests drive it directly. */
class RoamFrameCodec {
    private val buf = ByteArrayOutputStream()

    /** Feed raw stream bytes; returns every complete frame (newline removed).
     *  Partial frames stay buffered until their newline arrives. CRLF tolerated. */
    fun feed(chunk: ByteArray, len: Int): List<String> {
        val out = mutableListOf<String>()
        for (i in 0 until len) {
            when (val b = chunk[i].toInt() and 0xff) {
                '\n'.code -> {
                    out += buf.toString(Charsets.UTF_8.name())
                    buf.reset()
                }
                '\r'.code -> {}   // CRLF tolerance (BufReader::lines strips \r)
                else -> buf.write(b)
            }
        }
        return out
    }

    /** One outbound frame: the JSON message plus its terminating newline. */
    fun encode(text: String): ByteArray = (text + "\n").toByteArray(Charsets.UTF_8)
}
