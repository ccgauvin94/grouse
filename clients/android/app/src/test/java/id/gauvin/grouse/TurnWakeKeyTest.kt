// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Owner keys must survive roam ids, whose colons are part of the session id. */
class TurnWakeKeyTest {
    @Test
    fun `roam session id stays one field`() {
        val key = wakeOwnerKey("run", "roam:office:20261123_1", "abc123")
        assertTrue(wakeOwnerMatchesSession(key, "roam:office:20261123_1"))
    }

    @Test
    fun `similar session ids do not cross-release`() {
        val key = wakeOwnerKey("send", "roam:office:20261123_1", "1")
        assertFalse(wakeOwnerMatchesSession(key, "roam:office:20261123_10"))
        assertFalse(wakeOwnerMatchesSession(key, "roam:office:20261123"))
        assertFalse(wakeOwnerMatchesSession(key, "20261123_1"))
    }

    @Test
    fun `malformed key never matches`() {
        assertFalse(wakeOwnerMatchesSession("run\u001f20261123_1", "20261123_1"))
        assertFalse(wakeOwnerMatchesSession("run:20261123_1:abc", "20261123_1"))
    }
}
