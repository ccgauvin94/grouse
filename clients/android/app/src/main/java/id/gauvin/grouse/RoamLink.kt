package id.gauvin.grouse

import uniffi.grouse_roam_core.RoamStream

/** Frame I/O over a roam byte stream. AcpClient owns the reader thread and the
 *  newline framing; this is just the blocking byte surface + one frame encode. */
interface RoamLink {
    /** Blocking write of one newline-terminated ACP frame. False = transport dead. */
    fun send(text: String): Boolean
    /** Blocking read of up to `max` bytes; empty array = EOF/closed. */
    fun read(max: Int): ByteArray
    /** Tear down the stream (FIN + release the native handle). */
    fun close()
}

/** [RoamLink] over the uniffi [RoamStream] (iroh QUIC duplex). The native read
 *  is blocking; a transport error surfaces as EOF so the reader thread stops. */
class RoamStreamLink(private val stream: RoamStream) : RoamLink {
    private val codec = RoamFrameCodec()

    override fun send(text: String): Boolean = try {
        stream.write(codec.encode(text)); true
    } catch (_: Throwable) { false }

    override fun read(max: Int): ByteArray = try {
        stream.read(max.toLong())
    } catch (_: Throwable) { ByteArray(0) }

    override fun close() {
        // shutdown() drops the write half (FIN); close() releases the native
        // handle, which unblocks any in-flight read with a channel error.
        try { stream.shutdown() } catch (_: Throwable) {}
        try { stream.close() } catch (_: Throwable) {}
    }
}
