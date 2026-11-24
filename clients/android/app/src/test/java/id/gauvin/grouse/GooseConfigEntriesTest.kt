// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Pure rules behind Settings › Server › Goose config. No Compose/Robolectric: the key space,
 *  the filter and the secret attribution are the parts that can silently corrupt a write —
 *  e.g. by sending a secret to the ordinary config section, or masking a plain value. */
class GooseConfigEntriesTest {

    @Test
    fun `secret keys live in their own namespace`() {
        assertEquals("MODEL", gooseConfigEntryKey(" MODEL ", false))
        assertEquals("secret:MODEL", gooseConfigEntryKey("MODEL", true))
        // A secret and an ordinary key of the same name must stay two distinct rows.
        assertFalse(gooseConfigEntryKey("MODEL", false) == gooseConfigEntryKey("MODEL", true))
    }

    @Test
    fun `parse round-trips both kinds of key`() {
        assertEquals("MODEL" to false, parseGooseConfigEntryKey(gooseConfigEntryKey("MODEL", false)))
        assertEquals("MODEL" to true, parseGooseConfigEntryKey(gooseConfigEntryKey("MODEL", true)))
        assertEquals("A:B" to false, parseGooseConfigEntryKey("A:B"))
        assertEquals("" to true, parseGooseConfigEntryKey("secret:"))
    }

    @Test
    fun `explicit read replies are attributed by their namespace`() {
        val knownSecrets = setOf("API_KEY")
        // Bare replies come from read-all and are ordinary, even if a secret of
        // the same name is known: the two goose stores can hold the same key.
        assertEquals("API_KEY", gooseConfigIncomingEntryKey("API_KEY", knownSecrets))
        assertEquals("secret:API_KEY", gooseConfigIncomingEntryKey("secret:API_KEY", emptySet()))
        assertEquals("API_KEY", gooseConfigIncomingEntryKey("config:API_KEY", knownSecrets))
        // Different keys in the two stores remain two rows.
        assertEquals("secret:API_KEY", gooseConfigIncomingEntryKey("secret:API_KEY", knownSecrets))
    }

    @Test
    fun `search matches keys case-insensitively and never values`() {
        val entries = mapOf(
            "MODEL" to "gpt-5",
            "secret:MODEL" to "sk-topsecret",
            "GOOSE_CONTEXT_LIMIT" to "262144",
        )
        assertEquals(listOf("MODEL", "secret:MODEL"),
            filterGooseConfigEntries(entries, "model").map { it.first })
        assertEquals(listOf("MODEL", "secret:MODEL"),
            filterGooseConfigEntries(entries, " MoDel ").map { it.first })
        assertTrue("values are not searched", filterGooseConfigEntries(entries, "topsecret").isEmpty())
        assertTrue("values are not searched", filterGooseConfigEntries(entries, "gpt").isEmpty())
    }

    @Test
    fun `list is sorted by key and a blank query keeps everything`() {
        val entries = mapOf(
            "secret:ZEBRA" to "z",
            "alpha" to "a",
            "Beta" to "b",
        )
        assertEquals(listOf("alpha", "Beta", "secret:ZEBRA"),
            filterGooseConfigEntries(entries, "").map { it.first })
        assertEquals(listOf("alpha", "Beta", "secret:ZEBRA"),
            filterGooseConfigEntries(entries, "   ").map { it.first })
        assertEquals(3, filterGooseConfigEntries(entries, "").size)
    }

    @Test
    fun `secret values are masked everywhere they are displayed`() {
        val mask = gooseConfigDisplayValue("secret:API_KEY", "sk-live-key")
        assertEquals(GOOSE_MASKED_VALUE, mask)
        assertFalse("a real secret must never reach the display", mask.contains("sk-live"))
        assertEquals("secret:API_KEY", gooseConfigEntryKey("API_KEY", true))
        // Even an unset secret shows only the mask — no "set/unset" signal leaks out.
        assertEquals(GOOSE_MASKED_VALUE, gooseConfigDisplayValue("secret:API_KEY", ""))
    }

    @Test
    fun `detail route carries the secret flag and encodes the key`() {
        assertEquals("goose_config_detail/GOOSE_CONTEXT_LIMIT/0",
            gooseConfigDetailRoute("GOOSE_CONTEXT_LIMIT"))
        assertEquals("goose_config_detail/API%2FKEY/1",
            gooseConfigDetailRoute("secret:API/KEY"))
    }
}
