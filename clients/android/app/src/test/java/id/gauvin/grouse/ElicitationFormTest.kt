// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** A required number must PARSE before Submit is enabled: the old blank check let "abc"
 *  through and then dropped the key from the answer entirely. Pure helpers from
 *  ElicitationSheet so the rule is testable without a Compose hierarchy. */
class ElicitationFormTest {

    private fun field(name: String, type: String, required: Boolean) =
        AcpEvent.ElicitField(name, type, name, "", emptyList(), required)

    @Test
    fun `required numeric fields must parse`() {
        val fields = listOf(field("count", "number", true), field("age", "integer", true))
        assertFalse(elicitationComplete(fields, mapOf("count" to "abc", "age" to "7")))
        assertFalse(elicitationComplete(fields, mapOf("count" to "12.5")))
        assertTrue(elicitationComplete(fields, mapOf("count" to "12.5", "age" to "7")))
    }

    @Test
    fun `required text and choices must be present, booleans are always answerable`() {
        val fields = listOf(
            field("name", "string", true),
            field("choice", "string", true),
            field("flag", "boolean", true),
        )
        assertFalse(elicitationComplete(fields, mapOf("name" to "Ada")))
        assertFalse(elicitationComplete(fields, mapOf("name" to " ", "choice" to "x")))
        assertTrue(elicitationComplete(fields, mapOf("name" to "Ada", "choice" to "x")))
    }

    @Test
    fun `optional fields do not block submission`() {
        assertTrue(elicitationComplete(listOf(field("note", "string", false)), emptyMap()))
    }

    @Test
    fun `validated values keep their declared wire types`() {
        val fields = listOf(
            field("ratio", "number", true),
            field("count", "integer", true),
            field("flag", "boolean", false),
            field("note", "string", false),
        )
        val values = elicitationValues(
            fields,
            mapOf("ratio" to "1.5", "count" to "3", "note" to "  "),
            mapOf("flag" to true),
        )
        assertEquals("1.5", values["ratio"]?.content)
        assertEquals("3", values["count"]?.content)
        assertEquals("true", values["flag"]?.content)
        assertFalse(values.containsKey("note"))
    }
}
