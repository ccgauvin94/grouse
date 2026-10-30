// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Thread groups remember which of them are open, in a separator-joined string (a `Set`
 * is not Bundle-friendly, and this state has to survive a rotation — collapsing a group
 * someone just opened is exactly the kind of thing that reads as a broken button).
 *
 * Labels are normalised slugs (`pnc-low-balance`), so they cannot contain the unit
 * separator; that is what makes a split safe rather than a guess.
 */
class ThreadGroupTest {

    @Test
    fun toggling_adds_then_removes() {
        val one = toggleLabel("", "dentist-reschedule")
        assertEquals("dentist-reschedule", one)
        val two = toggleLabel(one, "ramp-approvals")
        assertEquals(setOf("dentist-reschedule", "ramp-approvals"), split(two))
        val back = toggleLabel(two, "dentist-reschedule")
        assertEquals(setOf("ramp-approvals"), split(back))
        assertEquals("", toggleLabel(back, "ramp-approvals"))
    }

    @Test
    fun an_empty_state_toggles_cleanly() {
        // The first tap after a rotation back to nothing must not produce a leading
        // separator, which would parse as an empty label.
        assertEquals("global-entry", toggleLabel("", "global-entry"))
    }

    @Test
    fun a_hyphenated_label_survives_a_round_trip() {
        // Hyphens are IN labels; the separator is U+0001, never a character a
        // normalised label can hold.
        val csv = toggleLabel(toggleLabel("", "pnc-low-balance"), "msu-cryoarm-contract")
        assertEquals(setOf("pnc-low-balance", "msu-cryoarm-contract"), split(csv))
    }

    private fun split(csv: String) = csv.split('\u0001').filter { it.isNotBlank() }.toSet()
}
