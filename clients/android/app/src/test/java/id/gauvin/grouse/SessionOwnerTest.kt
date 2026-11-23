// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import id.gauvin.grouse.ConnectionManager.Companion.classifyOwner
import id.gauvin.grouse.ConnectionManager.Companion.ownerFromId
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** Routing is a pure function of the CORE's `SessionOwner(kind, label)`. The
 *  generated `kind` may be an enum (whose `toString()` we classify) or a plain
 *  string on an older binding; none of that needs a native library to test. */
class SessionOwnerTest {

    @Test
    fun `a labelled roam owner is a peer`() {
        assertEquals(OwnerView(OwnerKind.PEER, "Phaethon"), classifyOwner("ROAM", "Phaethon"))
        assertEquals(OwnerView(OwnerKind.PEER, "Phaethon"), classifyOwner("Roam", " Phaethon "))
        // A generated sealed/enum toString can carry the type prefix.
        assertEquals(OwnerView(OwnerKind.PEER, "Phaethon"),
            classifyOwner("SessionOwnerKind.ROAM", "Phaethon"))
    }

    @Test
    fun `a main owner is never a routing target even when it has a label`() {
        assertEquals(OwnerView(OwnerKind.MAIN, null), classifyOwner("MAIN", "host"))
        assertEquals(OwnerView(OwnerKind.MAIN, null), classifyOwner("local", null))
        assertEquals(OwnerView(OwnerKind.MAIN, null), classifyOwner(null, null))
    }

    @Test
    fun `a labelled unknown kind is treated as a peer`() {
        assertEquals(OwnerView(OwnerKind.PEER, "new-transport"),
            classifyOwner("something-new", "new-transport"))
    }

    @Test
    fun `the id namespace is the fallback for older cores`() {
        assertEquals(OwnerView(OwnerKind.PEER, "Phaethon"),
            ownerFromId("roam:Phaethon:20260822_1"))
        assertEquals(OwnerView(OwnerKind.MAIN, null), ownerFromId("20260822_1"))
        assertEquals(OwnerView(OwnerKind.MAIN, null), ownerFromId(null))
        assertNull(ownerFromId("roam::20260822_1").peer)
    }
}
