// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Test
import java.time.LocalDate

/**
 * The speed of the due line is not the point; its SIGN is. `ChronoUnit.DAYS.between(a, b)` is
 * `b - a`, and the original call had the arguments the wrong way round, so every branch
 * inverted: an item due in a week rendered "overdue 7d" and one fifteen days past rendered
 * "due in 15d". Nothing failed loudly — a due date is only ever read by a human, and the
 * briefing titles carry their own prose ("eight days past its Oct 15 due date"), which is
 * exactly how the contradiction stayed invisible on screen: title says overdue, meta says
 * due soon.
 *
 * These cases are derived from the live store's shapes (a bare date, and a naive datetime),
 * and every expected string is computed off `LocalDate.now()` so the test does not rot.
 */
class DueLabelTest {

    private fun inDays(n: Long): String = LocalDate.now().plusDays(n).toString()

    @Test
    fun a_future_due_date_reads_as_due() {
        // The reported bug: due 2026-11-06, read on 2026-10-30, printed "overdue 7d".
        assertEquals("due in 7d", dueLabel(inDays(7)))
        assertEquals("due in 30d", dueLabel(inDays(30)))
    }

    @Test
    fun today_and_tomorrow_have_their_own_words() {
        assertEquals("due today", dueLabel(inDays(0)))
        assertEquals("due tomorrow", dueLabel(inDays(1)))
    }

    @Test
    fun a_past_due_date_reads_as_overdue() {
        assertEquals("overdue 3d", dueLabel(inDays(-3)))
        assertEquals("overdue 13d", dueLabel(inDays(-13)))
    }

    @Test
    fun past_two_weeks_switch_to_weeks() {
        // The flu shot (due_at 2026-10-15, read on 2026-10-30) is 15 days past, so it lands
        // one day into the week form: "overdue 2w". Two weeks is the switch-over, not 21 days.
        assertEquals("overdue 2w", dueLabel(inDays(-14)))
        assertEquals("overdue 2w", dueLabel(inDays(-15)))
        assertEquals("overdue 3w", dueLabel(inDays(-21)))
    }

    @Test
    fun far_future_names_the_date() {
        // Past a month a day count stops helping; the month/day is what a reader can check.
        val date = LocalDate.now().plusDays(120)
        assertEquals(
            "due ${date.month.getDisplayName(java.time.format.TextStyle.SHORT, java.util.Locale.getDefault())} ${date.dayOfMonth}",
            dueLabel(date.toString()),
        )
    }

    @Test
    fun a_naive_datetime_is_read_by_its_date() {
        // Obligations carry a time ("2026-11-06T15:30"); the line only needs the day.
        assertEquals("due in 5d", dueLabel("${inDays(5)}T15:30"))
        assertEquals("overdue 2d", dueLabel("${inDays(-2)}T09:00"))
    }

    @Test
    fun a_blank_or_junk_value_never_invents_a_date() {
        assertEquals("", dueLabel(""))
        assertEquals("due not-a-date", dueLabel("not-a-date"))
    }
}
