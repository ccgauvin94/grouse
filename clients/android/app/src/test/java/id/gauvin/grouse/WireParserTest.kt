// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * JVM tests for the pure wire parsers / translators the app historically shipped without any
 * coverage — the ones its own docs call "the bug-prone" edge (`toExtensionDto` mutates the
 * listed-extension shape into the add-method shape and silently returns the input on anything
 * it cannot map). These lock in the documented contract so a regression can't vanish a tool.
 */
class WireParserTest {

    private fun json(s: String): JsonElement = Json.parseToJsonElement(s)

    // ---------------------------------------------------------------------
    // toExtensionDto — listed extension -> add-method shape
    // ---------------------------------------------------------------------

    @Test
    fun `streamable_http becomes an mcp http server with a header array`() {
        val inExt = json(
            """{"type":"streamable_http","name":"kagi",
               "uri":"https://kagi.example/mcp",
               "headers":{"Authorization":"Bearer x","X-Trace":"abc"},"timeout":30}"""
        ).jsonObject

        val got = toExtensionDto(inExt)

        val server = got["server"]!!.jsonObject
        assertEquals("mcp", got["type"]!!.jsonPrimitive.content)
        assertEquals("http", server["type"]!!.jsonPrimitive.content)
        assertEquals("kagi", server["name"]!!.jsonPrimitive.content)
        assertEquals("https://kagi.example/mcp", server["url"]!!.jsonPrimitive.content)
        val headers = server["headers"]!!.jsonArray
        // The header MAP is rendered as a list of {name, value} pairs, in order.
        assertEquals(2, headers.size)
        assertEquals("Authorization", headers[0].jsonObject["name"]!!.jsonPrimitive.content)
        assertEquals("Bearer x", headers[0].jsonObject["value"]!!.jsonPrimitive.content)
        assertEquals("X-Trace", headers[1].jsonObject["name"]!!.jsonPrimitive.content)
        assertEquals("abc", headers[1].jsonObject["value"]!!.jsonPrimitive.content)
        assertEquals(30, got["timeout"]!!.jsonPrimitive.int)
    }

    @Test
    fun `sse becomes an mcp sse server`() {
        val inExt = json("""{"type":"sse","name":"s","uri":"https://s.example/sse"}""").jsonObject
        val got = toExtensionDto(inExt)
        assertEquals("mcp", got["type"]!!.jsonPrimitive.content)
        val server = got["server"]!!.jsonObject
        assertEquals("sse", server["type"]!!.jsonPrimitive.content)
        assertEquals("https://s.example/sse", server["url"]!!.jsonPrimitive.content)
    }

    @Test
    fun `stdio becomes an mcp stdio server`() {
        val inExt = json(
            """{"type":"stdio","name":"t","cmd":"python","args":["run.py"],"description":"d"}"""
        ).jsonObject
        val got = toExtensionDto(inExt)
        assertEquals("mcp", got["type"]!!.jsonPrimitive.content)
        val server = got["server"]!!.jsonObject
        assertEquals("stdio", server["type"]!!.jsonPrimitive.content)
        assertEquals("python", server["command"]!!.jsonPrimitive.content)
        assertEquals(listOf("run.py"), server["args"]!!.jsonArray.map { it.jsonPrimitive.content })
        assertTrue(server["env"]!!.jsonArray.isEmpty())
        assertEquals("d", got["description"]!!.jsonPrimitive.content)
    }

    @Test
    fun `builtin, platform and mcp pass through unchanged`() {
        for (t in listOf("builtin", "platform", "mcp")) {
            val ext = json("""{"type":"$t","name":"x"}""").jsonObject
            assertEquals("already-accepted shape must round-trip untouched", ext, toExtensionDto(ext))
        }
    }

    @Test
    fun `missing type or name returns the input unchanged`() {
        val noName = json("""{"type":"streamable_http"}""").jsonObject
        assertEquals("silently returns the input rather than invent a broken DTO", noName, toExtensionDto(noName))
        val noType = json("""{"name":"x"}""").jsonObject
        assertEquals(noType, toExtensionDto(noType))
    }

    // ---------------------------------------------------------------------
    // cron parse/build round-trips
    // ---------------------------------------------------------------------

    @Test
    fun `hourly cron parses and round-trips`() {
        val s = parseCron("0 30 * * * *")
        assertEquals(CronKind.HOURLY, s.kind)
        assertEquals(30, s.minute)
        assertEquals("0 30 * * * *", buildCron(s))
    }

    @Test
    fun `daily cron parses and round-trips`() {
        val s = parseCron("0 5 9 * * *")
        assertEquals(CronKind.DAILY, s.kind)
        assertEquals(9, s.hour)
        assertEquals(5, s.minute)
        assertEquals("0 5 9 * * *", buildCron(s))
    }

    @Test
    fun `weekly cron parses and round-trips with canonical day`() {
        val s = parseCron("0 10 7 * * mon")
        assertEquals(CronKind.WEEKLY, s.kind)
        assertEquals("Mon", s.dow)
        assertEquals("0 10 7 * * Mon", buildCron(s))
    }

    @Test
    fun `hour window parses and round-trips`() {
        val s = parseCron("0 20 6-9 * * *")
        assertEquals(CronKind.HOURLY, s.kind)
        assertEquals(6, s.fromHour)
        assertEquals(9, s.toHour)
        assertEquals("0 20 6-9 * * *", buildCron(s))
    }

    @Test
    fun `five-field cron is tolerated with a seconds prefix`() {
        val s = parseCron("0 9 * * *")
        assertEquals(CronKind.DAILY, s.kind)
        assertEquals(9, s.hour)
        assertEquals("0 0 9 * * *", buildCron(s))
    }

    @Test
    fun `unrecognisable cron is kept verbatim as custom`() {
        val s = parseCron("a b c")
        assertEquals(CronKind.CUSTOM, s.kind)
        assertEquals("a b c", s.raw)
        assertEquals("a b c", buildCron(s))
    }

    // ---------------------------------------------------------------------
    // recipe / schedule / extension parsers
    // ---------------------------------------------------------------------

    @Test
    fun `recipes parse off the real wire shape`() {
        val wire = """
            [ {"id":"r1","schedule_cron":"0 6 * * *",
               "recipe":{"title":"Daily",
                          "settings":{"goose_provider":"openai-etc","goose_model":"gpt-x"},
                          "prompt":"p","instructions":"i",
                          "parameters":[{"key":"k","requirement":"required","description":"d","default":"v"}],
                          "sub_recipes":[{"name":"sub"}],"extensions":[{"name":"ext"}]},
               "file_path":"/tmp/r1.yaml"} ]
        """
        val got = parseRecipes(wire)
        assertEquals(1, got.size)
        val r = got[0]
        assertEquals("r1", r.id)
        assertEquals("Daily", r.title)
        assertEquals("0 6 * * *", r.cron)
        assertEquals("openai-etc", r.provider)
        assertEquals("gpt-x", r.model)
        assertEquals(1, r.parameters.size)
        assertEquals("required", r.parameters[0].requirement)
        assertEquals(listOf("sub"), r.subRecipes)
        assertEquals(listOf("ext"), r.extensions)
        // The parser sorts by title; the raw recipe object rides along for the editor.
        assertTrue(r.raw.isNotEmpty())
    }

    @Test
    fun `malformed recipe payloads yield an empty list, never a throw`() {
        assertEquals(emptyList<RecipeInfo>(), parseRecipes(""))
        assertEquals(emptyList<RecipeInfo>(), parseRecipes("not json"))
        assertEquals(emptyList<RecipeInfo>(), parseRecipes("[]"))
    }

    @Test
    fun `schedules parse off the real wire shape`() {
        val wire = """
            [ {"id":"job1","cron":"0 6 * * *","source":"recipe",
               "paused":false,"currentlyRunning":true,"lastRun":"t","currentSessionId":"s1"} ]
        """
        val got = parseSchedules(wire)
        assertEquals(1, got.size)
        assertEquals("job1", got[0].id)
        assertEquals("0 6 * * *", got[0].cron)
        assertFalse(got[0].paused)
        assertTrue(got[0].running)
        assertEquals("s1", got[0].currentSessionId)
    }

    @Test
    fun `global extensions parse nested server name for mcp extensions`() {
        val wire = """
            [ {"enabled":true,"configKey":"kagi",
               "extension":{"type":"streamable_http","description":"search",
                             "server":{"name":"kagi","uri":"https://k.example/mcp"}}} ]
        """
        val got = parseGlobalExtensions(wire)
        assertEquals(1, got.size)
        // type=mcp extensions carry their name NESTED under server.name.
        assertEquals("kagi", got[0].name)
        assertEquals(true, got[0].enabled)
        assertEquals("kagi", got[0].configKey)
    }

    @Test
    fun `session extensions parse and carry the fromPeer flag`() {
        val wire = """[{"type":"mcp","name":"kagi","description":"d"}]"""
        val local = parseSessionExtensions(wire, fromPeer = false)
        val peer = parseSessionExtensions(wire, fromPeer = true)
        assertEquals(1, local.size)
        assertEquals("kagi", local[0].name)
        // Same payload, different provenance must be preserved.
        assertTrue(local[0].enabled)
        assertFalse(local[0].fromPeer)
        assertTrue(peer[0].fromPeer)
    }

    @Test
    fun `malformed extension payloads yield an empty list, never a throw`() {
        assertEquals(emptyList<ExtInfo>(), parseGlobalExtensions("::"))
        assertEquals(emptyList<ExtInfo>(), parseGlobalExtensions("[]"))
        assertEquals(emptyList<ExtInfo>(), parseSessionExtensions("not json", fromPeer = false))
    }
}
