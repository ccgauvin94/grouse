// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import id.gauvin.grouse.ConnectionManager.Companion.fileKeyFor
import org.junit.Assert.assertEquals
import org.junit.Test

/** A file row lives on ONE session of ONE owner. The download card's key must
 *  therefore carry both, so two peers sharing a server path cannot overwrite
 *  each other's progress. Pure helper: no core, no Android framework. */
class FileKeyTest {

    @Test
    fun `peer namespace keeps identical paths apart`() {
        val main = fileKeyFor(null, "20260822_1", "/data/report.pdf")
        val peer = fileKeyFor("Phaethon", "20260822_1", "/data/report.pdf")
        assertEquals("main|20260822_1|/data/report.pdf", main)
        assertEquals("Phaethon|20260822_1|/data/report.pdf", peer)
        assert(main != peer)
    }

    @Test
    fun `display id and path are both part of the key`() {
        assertEquals(
            "Phaethon|roam:Phaethon:20260822_1|/data/report.pdf",
            fileKeyFor("Phaethon", "roam:Phaethon:20260822_1", "/data/report.pdf"),
        )
        assert(fileKeyFor(null, "a", "/x") != fileKeyFor(null, "b", "/x"))
        assert(fileKeyFor(null, "a", "/x") != fileKeyFor(null, "a", "/y"))
    }

    @Test
    fun `a missing session still keys stably`() {
        assertEquals("main||/data/report.pdf", fileKeyFor(null, null, "/data/report.pdf"))
    }
}
