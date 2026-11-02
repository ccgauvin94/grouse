// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import android.content.ContentResolver
import android.net.Uri
import android.provider.OpenableColumns
import uniffi.grouse_core.FileSource

/**
 * Streams a picked file into the core, one chunk at a time.
 *
 * The core drives the upload: it asks for 8 MiB at a time and base64-encodes
 * each slice onto the wire as a tool-call argument. Nothing here parses the
 * file — extraction happens server-side — so this class is only responsible for
 * what an Android app can uniquely do: resolve a SAF `content://` URI into
 * bytes.
 *
 * A `ContentResolver` stream is not seekable, and the core reads strictly
 * forward, so the stream is held open across calls and advanced by the delta.
 * Re-opening per chunk would make an n-chunk upload O(n²) in skipped bytes.
 */
class UriFileSource(
    private val resolver: ContentResolver,
    private val uri: Uri,
    private val displayName: String,
    private val mimeType: String,
) : FileSource {

    /** `-1` when the provider will not report a size; the core treats 0 specially. */
    private val total: Long = runCatching {
        resolver.query(uri, arrayOf(OpenableColumns.SIZE), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst() && !cursor.isNull(0)) cursor.getLong(0) else -1L
        } ?: -1L
    }.getOrDefault(-1L)

    private var stream: java.io.InputStream? = null
    private var position: Long = 0

    override fun name(): String = displayName

    override fun mime(): String = mimeType

    override fun size(): ULong = if (total > 0) total.toULong() else 0uL

    @Synchronized
    override fun read(offset: ULong, len: UInt): ByteArray {
        val want = len.toInt()
        if (want <= 0) return ByteArray(0)
        val start = offset.toLong()
        // A backwards or repeated offset means the core restarted (a retry after
        // a dropped socket). Re-open rather than serve stale bytes.
        if (stream == null || start < position) {
            stream?.let { runCatching { it.close() } }
            stream = resolver.openInputStream(uri)
            position = 0
        }
        val input = stream ?: return ByteArray(0)

        var toSkip = start - position
        while (toSkip > 0) {
            val skipped = input.skip(toSkip)
            if (skipped <= 0) break
            toSkip -= skipped
            position += skipped
        }

        val buffer = ByteArray(want)
        var read = 0
        while (read < want) {
            val n = input.read(buffer, read, want - read)
            if (n <= 0) break
            read += n
        }
        position += read
        // A short read is the core's signal for "end of file"; it refuses to
        // continue on one mid-file, because a truncated upload must never
        // commit as if it were complete.
        return if (read == want) buffer else buffer.copyOf(read)
    }

    fun close() {
        runCatching { stream?.close() }
        stream = null
        position = 0
    }
}
