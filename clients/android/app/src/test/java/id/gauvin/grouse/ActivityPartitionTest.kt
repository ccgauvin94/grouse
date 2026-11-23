// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.grouse_core.ActiveWork

/** The background-tasks sheet splits the core's transcript-ordered lanes into live work and
 *  history. Both groups must read newest-first: the pane's whole purpose is to answer "what
 *  is running now" before showing anything that already finished (2026-11-22). */
class ActivityPartitionTest {

    private fun lane(id: String, running: Boolean) = ActiveWork(
        id = id,
        title = id,
        detail = "",
        status = if (running) "in_progress" else "completed",
        running = running,
        outcome = if (running) "running" else "completed",
        subagent = false,
        steps = emptyList(),
    )

    @Test
    fun `no lanes means no groups - the empty state still owns the sheet`() {
        val (active, finished) = partitionActivity(emptyList())
        assertEquals(emptyList<ActiveWork>(), active)
        assertEquals(emptyList<ActiveWork>(), finished)
    }

    @Test
    fun `each group is newest-first, independently of the other`() {
        // Core order is oldest first; the newest live lane and the newest finished lane
        // both have to land at the top of their own group.
        val lanes = listOf(
            lane("done-1", running = false),
            lane("live-1", running = true),
            lane("done-2", running = false),
            lane("live-2", running = true),
            lane("live-3", running = true),
        )
        val (active, finished) = partitionActivity(lanes)
        assertEquals(listOf("live-3", "live-2", "live-1"), active.map { it.id })
        assertEquals(listOf("done-2", "done-1"), finished.map { it.id })
    }

    @Test
    fun `a one-sided list stays one-sided`() {
        val allLive = listOf(lane("a", true), lane("b", true))
        assertEquals(listOf("b", "a"), partitionActivity(allLive).first.map { it.id })
        assertEquals(emptyList<ActiveWork>(), partitionActivity(allLive).second)

        val allDone = listOf(lane("a", false), lane("b", false))
        assertEquals(emptyList<ActiveWork>(), partitionActivity(allDone).first)
        assertEquals(listOf("b", "a"), partitionActivity(allDone).second.map { it.id })
    }

    @Test
    fun `the snapshot handed over by the core is not reordered in place`() {
        val lanes = listOf(lane("old", false), lane("new", true))
        partitionActivity(lanes)
        assertEquals(listOf("old", "new"), lanes.map { it.id })
    }

    @Test
    fun `the badge caps at 9 plus`() {
        assertEquals("1", activityCountLabel(1))
        assertEquals("9", activityCountLabel(9))
        assertEquals("9+", activityCountLabel(10))
        assertEquals("9+", activityCountLabel(27))
    }
}
