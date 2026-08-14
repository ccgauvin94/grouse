package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/** Cold-start transcript cache cap: keep at most `keep` newest files (by mtime), delete the
 *  rest, best-effort. Top-level internal so it is JVM-testable. */
class TranscriptCacheTest {

    private fun tempDir(): File {
        val dir = File.createTempFile("transcript-cache-", "").let {
            it.delete(); File(it.parentFile, "transcript-cache-${it.name}")
        }
        dir.mkdirs()
        return dir
    }

    private fun File.touchFiles(n: Int, base: Long = 1_000_000L) {
        repeat(n) { i ->
            File(this, "s$i.json").apply {
                writeText("{}")
                setLastModified(base + i)
            }
        }
    }

    @Test
    fun `over the cap keeps the 20 newest and deletes the 5 oldest`() {
        val dir = tempDir()
        try {
            dir.touchFiles(25)
            pruneTranscriptCache(dir, keep = 20)
            val remaining = dir.listFiles()!!.map { it.name }.toSet()
            assertEquals(20, remaining.size)
            (0 until 5).forEach { i -> assertTrue("s$i.json should be pruned", "s$i.json" !in remaining) }
            (5 until 25).forEach { i -> assertTrue("s$i.json should survive", "s$i.json" in remaining) }
        } finally {
            dir.deleteRecursively()
        }
    }

    @Test
    fun `at or under the cap leaves every file alone`() {
        val dir = tempDir()
        try {
            dir.touchFiles(20)
            pruneTranscriptCache(dir, keep = 20)
            assertEquals(20, dir.listFiles()!!.size)
        } finally {
            dir.deleteRecursively()
        }
    }

    @Test
    fun `missing directory is a no-op`() {
        val dir = File(tempDir(), "does-not-exist")
        pruneTranscriptCache(dir, keep = 20)   // must not throw
        assertTrue(!dir.exists())
    }
}
