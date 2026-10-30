// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The `tasks` section of the My Day envelope — handed-over work, which the core
 * projects from assistant-state's `task_list` (a different object from an item:
 * a task is an act with a result, an item is a fact that needs attention).
 *
 * What is worth pinning is the parsing the UI depends on: the state drives which
 * card style and which line is shown, `session_id` is what makes the card
 * tappable at all, and `artifacts` arrive as `[{path, …}]` objects that must
 * become plain paths (a map taking `it as? String` would silently yield none).
 */
class AgentTaskTest {

    private fun row(json: String): AgentTask? =
        agentTaskFrom(Json.parseToJsonElement(json) as JsonObject)

    @Test
    fun a_review_row_carries_its_result_and_the_session_that_did_it() {
        val t = row(
            """{"id":"tsk_20261030T2030_acfd55","title":"Board deck","state":"review",
                "thread":"series-a","session_id":"sess-9","request":"build it",
                "result":"12 slides","created_at":"2026-10-30T20:30:21Z",
                "updated_at":"2026-10-30T21:02:00Z"}"""
        )!!
        assertEquals("review", t.state)
        assertEquals("12 slides", t.result)
        assertEquals("sess-9", t.sessionId)
        assertEquals("series-a", t.thread)
    }

    @Test
    fun artifacts_are_objects_with_a_path_not_bare_strings() {
        // The server stores `[{"path": "/workspace/deck.pdf", "bytes": 2401}]`.
        val t = row(
            """{"id":"tsk_1","title":"T","state":"review",
                "artifacts":[{"path":"/workspace/deck.pdf","bytes":2401},
                             {"path":"/workspace/model.xlsx"}]}"""
        )!!
        assertEquals(listOf("/workspace/deck.pdf", "/workspace/model.xlsx"), t.artifacts)
    }

    @Test
    fun a_running_row_has_no_result_and_may_have_no_session() {
        val t = row("""{"id":"tsk_2","title":"Watch payroll","state":"running","request":"watch"}""")!!
        assertEquals("running", t.state)
        assertTrue(t.result.isEmpty())
        assertTrue(t.sessionId.isEmpty())
        assertTrue(t.artifacts.isEmpty())
    }

    @Test
    fun a_row_without_an_id_is_dropped() {
        // Nothing to key it on and nothing to open: better absent than painted
        // under an invented key.
        assertNull(row("""{"title":"orphan","state":"running"}"""))
    }

    @Test
    fun a_null_field_does_not_throw() {
        val t = row("""{"id":"tsk_3","title":"T","state":"running","thread":null,"artifacts":null}""")!!
        assertEquals("", t.thread)
        assertTrue(t.artifacts.isEmpty())
    }
}
