// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Test
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.format.TextStyle
import java.util.Locale

/**
 * My Day's timeline anchors: when the assistant ADDED a row, and when a closed one left.
 *
 * The stamps were on the wire from the start (`state_items` answers `SELECT *`), and the
 * core simply dropped them, so every row read as undated — which is why the page could
 * not be read as a timeline however it was ordered. The format has three ages because one
 * does not fit all: the clock for today, the weekday for this week, the date for anything
 * older. Every case is computed off a FIXED clock and zone, so the test does not rot the
 * way a label function reading `Instant.now()` inside would.
 */
class AddedStampTest {

    private val zone = ZoneId.of("America/Denver")
    // 2026-11-03 17:00 local.
    private val now = LocalDate.of(2026, 11, 3).atTime(17, 0).atZone(zone).toInstant()

    private fun stamp(daysAgo: Long, hour: Int = 9, minute: Int = 5): String =
        LocalDate.of(2026, 11, 3).minusDays(daysAgo).atTime(hour, minute).atZone(zone)
            .toInstant().toString()

    @Test
    fun something_added_today_reads_as_a_clock_time() {
        // 09:05 local on the same day, stored as UTC ("Z") the way the server writes it.
        assertEquals("added 09:05", stampLabel(stamp(0), now = now, zone = zone))
    }

    @Test
    fun yesterday_and_this_week_have_their_own_words() {
        assertEquals("added yesterday", stampLabel(stamp(1), now = now, zone = zone))
        // Two days back is a weekday, which is what a person scans for in a week's list.
        val weekday = LocalDate.of(2026, 11, 1).dayOfWeek.getDisplayName(TextStyle.SHORT, Locale.getDefault())
        assertEquals("added $weekday", stampLabel(stamp(2), now = now, zone = zone))
    }

    @Test
    fun anything_older_reads_as_a_date() {
        val expected = LocalDate.of(2026, 10, 24).month.getDisplayName(TextStyle.SHORT, Locale.getDefault())
        assertEquals("added $expected 24", stampLabel("2026-10-24T15:00:00Z", now = now, zone = zone))
    }

    @Test
    fun an_older_year_is_named() {
        // Built from the locale's own short month so the test is about WHICH branch ran,
        // not about the machine's language.
        val day = LocalDate.of(2025, 12, 30)
        val month = day.month.getDisplayName(TextStyle.SHORT, Locale.getDefault())
        assertEquals("added $month 30, 2025", stampLabel("2025-12-30T15:00:00Z", now = now, zone = zone))
    }

    @Test
    fun the_prefix_is_the_caller_s() {
        // A closed row says "closed …": the same formatter, a different question.
        assertEquals("closed yesterday", stampLabel(stamp(1), "closed", now, zone))
        assertEquals("started 09:05", stampLabel(stamp(0), "started", now, zone))
    }

    @Test
    fun no_stamp_prints_nothing() {
        // An older server, or a row that has never been seen: the meta line must not
        // gain "added " with nothing after it.
        assertEquals("", stampLabel("", now = now, zone = zone))
        assertEquals("", stampLabel("not a date", now = now, zone = zone))
    }

    @Test
    fun a_clock_skewed_stamp_still_reads_as_now() {
        // A server a few minutes ahead must not render a negative age or a blank.
        val future = LocalDate.of(2026, 11, 3).atTime(17, 30).atZone(zone).toInstant().toString()
        assertEquals("added 17:30", stampLabel(future, now = now, zone = zone))
    }

    @Test
    fun fractional_seconds_and_offsets_parse() {
        // Both shapes the store has produced: bare UTC and an explicit offset with micros.
        // 16:05Z is 09:05 in Denver (MST, UTC-7) — the label is local, the stamp is UTC.
        assertEquals("added 09:05", stampLabel("2026-11-03T16:05:30.123456Z", now = now, zone = zone))
        assertEquals("added 09:05", stampLabel("2026-11-03T16:05:30.123456+00:00", now = now, zone = zone))
    }

    @Test
    fun ordering_keys_put_the_undated_row_last() {
        // Sorting is what makes the list a timeline, so a row with no stamp must not
        // sort to the top as epoch zero would.
        val older = stampMillis("2026-10-01T00:00:00Z")
        val newer = stampMillis("2026-11-03T00:00:00Z")
        assertEquals(true, newer > older)
        assertEquals(Long.MIN_VALUE, stampMillis(""))
        assertEquals(Long.MIN_VALUE, stampMillis("nope"))
        // Newest first is the page's rule; the undated row lands at the bottom.
        val sorted = listOf("", "2026-10-01T00:00:00Z", "2026-11-03T00:00:00Z")
            .sortedByDescending { stampMillis(it) }
        assertEquals("2026-11-03T00:00:00Z", sorted.first())
        assertEquals("", sorted.last())
    }

    @Test
    fun a_closed_row_dates_itself_by_when_it_closed() {
        val closed = TaskItem(
            id = "itm_1", kind = "waiting", state = "done", title = "x",
            firstSeen = "2026-10-01T00:00:00Z", closedAt = "2026-11-02T00:00:00Z",
        )
        assertEquals(stampMillis("2026-11-02T00:00:00Z"), closed.doneStampMs())
        // Without a closed_at (a server that predates the column) the added stamp is the
        // only honest answer, rather than a row that sorts to the bottom forever.
        val bare = closed.copy(closedAt = "")
        assertEquals(stampMillis("2026-10-01T00:00:00Z"), bare.doneStampMs())
    }
}
