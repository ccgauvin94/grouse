// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The core rejects ':' in a roam label, and the label is also the `roam:<label>:` session-id
 *  prefix. This pins the PURE predicate (and the card-label helpers it drives) without an
 *  Activity, Compose, a core or Android framework classes — matching the lock-gate test. */
class RoamLabelValidationTest {

    @Test
    fun `ordinary labels are valid`() {
        listOf("laptop", "desk-1", "work desktop", "host_2", "a".repeat(ROAM_LABEL_MAX_CHARS))
            .forEach {
                assertTrue("'$it' should be accepted", validRoamLabel(it))
                assertNull(roamLabelError(it))
            }
    }

    @Test
    fun `blank labels are invalid`() {
        listOf("", "   ", "\t", "\n", "  \u00A0  ").forEach {
            assertFalse("'$it' must be rejected", validRoamLabel(it))
            assertTrue("expected a reason for '$it'", roamLabelError(it)!!.isNotBlank())
        }
    }

    @Test
    fun `a colon anywhere is rejected`() {
        listOf("desk:1", ":desk", "desk:", "a:b:c", "roam:desk:1").forEach {
            assertFalse("'$it' must not reach core.roamConnect", validRoamLabel(it))
            assertTrue("message should name the colon for '$it'",
                roamLabelError(it)!!.contains("':'"))
        }
    }

    @Test
    fun `whitespace only and control characters are rejected`() {
        assertFalse(validRoamLabel("  \t  "))
        assertEquals("Give the host a name.", roamLabelError("  \t  "))
        assertFalse(validRoamLabel("desk\n1"))
        assertTrue(roamLabelError("desk\n1")!!.contains("line breaks"))
    }

    @Test
    fun `length ceiling is exact`() {
        assertTrue(validRoamLabel("x".repeat(ROAM_LABEL_MAX_CHARS)))
        assertFalse(validRoamLabel("x".repeat(ROAM_LABEL_MAX_CHARS + 1)))
        assertTrue(roamLabelError("x".repeat(ROAM_LABEL_MAX_CHARS + 1))!!
            .contains(ROAM_LABEL_MAX_CHARS.toString()))
        val max = "x".repeat(ROAM_LABEL_MAX_CHARS)
        // Padding is rejected, not trimmed at the core boundary.
        assertFalse(validRoamLabel("  $max  "))
        assertTrue(roamLabelError("  $max  ")!!.contains("spaces"))
    }

    @Test
    fun `padded labels are rejected, matching the core boundary`() {
        listOf(" laptop", "laptop ", "\tlaptop\t").forEach {
            assertFalse("'$it' must be rejected", validRoamLabel(it))
            assertTrue(roamLabelError(it)!!.contains("spaces"))
        }
    }

    @Test
    fun `unicode labels are allowed up to the same character budget`() {
        assertTrue(validRoamLabel("主机"))
        assertTrue(validRoamLabel("💻 laptop"))
        assertTrue(validRoamLabel("主机".repeat(ROAM_LABEL_MAX_CHARS / 2)))
        assertFalse(validRoamLabel("主机".repeat(ROAM_LABEL_MAX_CHARS / 2 + 1)))
        assertFalse(validRoamLabel("主:机"))
    }

    @Test
    fun `a bare card has no embedded label`() {
        val card = "goose+roam://eyJ2ZXJzaW9uIjoxfQ"
        assertNull(roamCardEmbeddedLabel(card))
        assertEquals(card, roamCardWithoutLabel(card))
        assertNull(roamCardLabelError(card, "laptop"))
        assertEquals("laptop", roamPeerName(card, " laptop "))
    }

    @Test
    fun `a valid card label is stripped from the card and used only when no name was typed`() {
        val card = "desk-one: goose+roam://eyJ2ZXJzaW9uIjoxfQ"
        assertEquals("desk-one", roamCardEmbeddedLabel(card))
        assertEquals("goose+roam://eyJ2ZXJzaW9uIjoxfQ", roamCardWithoutLabel(card))
        assertNull(roamCardLabelError(card, ""))
        assertEquals("desk-one", roamPeerName(card, ""))
        assertEquals("typed", roamPeerName(card, "typed"))
    }

    @Test
    fun `a card label with a colon is rejected with a clear message`() {
        val card = "desk:1: goose+roam://eyJ2ZXJzaW9uIjoxfQ"
        assertEquals("desk:1", roamCardEmbeddedLabel(card))
        // Even with a perfectly good typed name, the card's own label is not silently
        // accepted or stripped: the user gets a reason and nothing is saved/dialled.
        val message = roamCardLabelError(card, "laptop")
        assertTrue(message!!.contains("card's label"))
        assertTrue(message.contains("':'"))
        assertFalse(validRoamLabel(roamCardEmbeddedLabel(card)!!))
    }

    @Test
    fun `an invalid typed label is rejected on the card path too`() {
        val card = "goose+roam://eyJ2ZXJzaW9uIjoxfQ"
        assertTrue(roamCardLabelError(card, "desk:1")!!.contains("':'"))
        assertTrue(roamCardLabelError(card, "")!!.contains("name"))
    }
}
