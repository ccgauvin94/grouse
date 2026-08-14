package id.gauvin.grouse

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Parser contracts for the list-style ACP replies. Every one of these defends a field that
 * came back null (or a session that vanished) when read with the wrong casing or the wrong
 * nesting — the recipes family is snake_case while almost everything else is camelCase, and
 * MCP extensions carry their name nested under server.name. See AGENTS.md "ACP protocol
 * gotchas" for the incident history.
 */
class AcpClientParsersTest {

    private val client = AcpClient("ws://localhost:1/acp", "test-key") {}

    private val json = Json { ignoreUnknownKeys = true }

    // --- recipes: the snake_case family ------------------------------------------------

    @Test
    fun `parseRecipes reads snake_case fields and preserves raw`() {
        val result = json.parseToJsonElement("""
            {"recipes": [{
              "id": "r1",
              "schedule_cron": "0 9 * * *",
              "file_path": "/goose/recipes/daily.md",
              "recipe": {
                "title": "Daily briefing",
                "description": "Morning summary",
                "prompt": "Summarize",
                "instructions": "Be brief",
                "settings": {"goose_provider": "openai", "goose_model": "gpt-4o"},
                "parameters": [{"key": "topic", "requirement": "required", "description": "What", "default": "tech"}],
                "sub_recipes": [{"name": "weather"}],
                "extensions": [{"name": "fetch"}],
                "extra_key": {"nested": true}
              }
            }]}
        """).jsonObject

        val recipes = client.parseRecipes(result)

        assertEquals(1, recipes.size)
        val r = recipes[0]
        assertEquals("r1", r.id)
        // snake_case, NOT camelCase — the gotcha that made every recipe look unscheduled.
        assertEquals("0 9 * * *", r.cron)
        assertEquals("/goose/recipes/daily.md", r.filePath)
        assertEquals("openai", r.provider)
        assertEquals("gpt-4o", r.model)
        assertEquals("Daily briefing", r.title)
        assertEquals("Summarize", r.prompt)
        assertEquals(1, r.parameters.size)
        assertEquals("topic", r.parameters[0].key)
        assertEquals("required", r.parameters[0].requirement)
        assertEquals("tech", r.parameters[0].default)
        assertEquals(listOf("weather"), r.subRecipes)
        assertEquals(listOf("fetch"), r.extensions)
        // raw must round-trip the WHOLE recipe: recipes/save replaces the entire object, so a
        // modelled-only rebuild silently drops sub_recipes/allowlists/schemas.
        assertEquals("Daily briefing", r.raw["title"]?.jsonPrimitive?.content)
        assertEquals("weather", (r.raw["sub_recipes"] as? kotlinx.serialization.json.JsonArray)?.first()
            ?.jsonObject?.get("name")?.jsonPrimitive?.content)
    }

    @Test
    fun `parseRecipes ignores camelCase spellings that goose does not send`() {
        // If someone "fixes" the parser to read scheduleCron, it still works — but if goose
        // starts sending ONLY camelCase this test documents that the parser reads snake_case.
        val result = json.parseToJsonElement("""
            {"recipes": [{"id": "r1", "scheduleCron": "0 9 * * *", "filePath": "/x.md",
              "recipe": {"title": "T", "settings": {"gooseProvider": "openai"}}}]}
        """).jsonObject

        val recipes = client.parseRecipes(result)
        assertNull("cron must be read from schedule_cron, not scheduleCron", recipes[0].cron)
        assertEquals("", recipes[0].filePath)
        assertNull(recipes[0].provider)
    }

    @Test
    fun `parseRecipes drops entries without an id and handles empty results`() {
        assertEquals(0, client.parseRecipes(null).size)
        assertEquals(0, client.parseRecipes(json.parseToJsonElement("""{"recipes": []}""").jsonObject).size)
        // entry missing "recipe" object -> skipped
        val result = json.parseToJsonElement("""{"recipes": [{"id": "orphan"}]}""").jsonObject
        assertEquals(0, client.parseRecipes(result).size)
    }

    // --- sessions: _meta extraction and client-side filters -----------------------------

    @Test
    fun `parseSessions reads _meta fields and project id`() {
        val result = json.parseToJsonElement("""
            {"sessions": [{
              "sessionId": "s1",
              "title": "Chat one",
              "updatedAt": "2026-08-01T10:00:00Z",
              "cwd": "/home/user/Projects/one",
              "_meta": {
                "messageCount": 7,
                "modelId": "gpt-4o",
                "lastMessageSnippet": "…",
                "hasRecipe": true,
                "projectId": "one"
              }
            }]}
        """).jsonObject

        val sessions = client.parseSessions(result)
        assertEquals(1, sessions.size)
        val s = sessions[0]
        assertEquals("s1", s.sessionId)
        assertEquals("Chat one", s.title)
        assertEquals(7, s.messageCount)
        assertEquals("gpt-4o", s.model)
        assertEquals("…", s.snippet)
        assertTrue(s.hasRecipe)
        assertEquals("one", s.projectId)
        assertEquals("/home/user/Projects/one", s.cwd)
    }

    @Test
    fun `parseSessions drops scheduler and archived sessions client-side`() {
        val result = json.parseToJsonElement("""
            {"sessions": [
              {"sessionId": "s1", "title": "Scheduled job: abc", "_meta": {}},
              {"sessionId": "s2", "title": "Gone", "_meta": {"archivedAt": "2026-07-01"}},
              {"sessionId": "s3", "title": "Real"}
            ]}
        """).jsonObject

        val sessions = client.parseSessions(result)
        assertEquals(1, sessions.size)
        assertEquals("s3", sessions[0].sessionId)
    }

    // --- extensions: the nested server.name trap ---------------------------------------

    @Test
    fun `parseExtensions finds mcp extension names nested under server`() {
        val result = json.parseToJsonElement("""
            {"extensions": [
              {"extension": {"name": "builtin-http", "type": "builtin", "description": "d"},
               "enabled": true, "configKey": "ext.builtin"},
              {"extension": {"type": "mcp", "description": "Nextcloud",
                             "server": {"name": "nextcloud"}},
               "enabled": false, "configKey": "ext.nextcloud"}
            ]}
        """).jsonObject

        val exts = client.parseExtensions(result)
        assertEquals(2, exts.size)
        assertEquals("builtin-http", exts[0].name)
        assertTrue(exts[0].enabled)
        assertEquals("mcp", exts[1].type)
        assertEquals("nextcloud", exts[1].name)
        assertEquals("Nextcloud", exts[1].description)
        assertEquals("ext.nextcloud", exts[1].configKey)
        // raw preserves the server-sent object for toExtensionDto round-trips.
        assertEquals("mcp", exts[1].raw["type"]?.jsonPrimitive?.content)
    }

    @Test
    fun `parseExtensions skips extensions with no name anywhere`() {
        val result = json.parseToJsonElement(
            """{"extensions": [{"extension": {"type": "mcp"}, "enabled": true}]}""").jsonObject
        assertEquals(0, client.parseExtensions(result).size)
    }

    // --- schedules ---------------------------------------------------------------------

    @Test
    fun `parseSchedules maps jobs with cron and running flags`() {
        val result = json.parseToJsonElement("""
            {"jobs": [
              {"id": "j1", "cron": "0 9 * * *", "source": "/recipes/rd.md", "paused": true,
               "currentlyRunning": false, "lastRun": "2026-08-01T09:00:00Z", "currentSessionId": "s9"}
            ]}
        """).jsonObject

        val jobs = client.parseSchedules(result)
        assertEquals(1, jobs.size)
        val j = jobs[0]
        assertEquals("j1", j.id)
        assertEquals("0 9 * * *", j.cron)
        assertEquals("/recipes/rd.md", j.source)
        assertTrue(j.paused)
        assertEquals("s9", j.currentSessionId)
        // currentlyRunning is the wire key; the DTO calls it running.
        assertEquals(false, j.running)
    }

    // --- projects ----------------------------------------------------------------------

    @Test
    fun `parseProjects derives slug from path and root from content`() {
        val result = json.parseToJsonElement("""
            {"sources": [
              {"path": "/goose/projects/my-project.md", "name": "My Project",
               "description": "d", "content": "---\nroot: /srv/work\n---\nNotes"}
            ]}
        """).jsonObject

        val projects = client.parseProjects(result)
        assertEquals(1, projects.size)
        val p = projects[0]
        assertEquals("my-project", p.id)
        assertEquals("My Project", p.name)
        assertEquals("/srv/work", p.root)
    }

    // --- config options ----------------------------------------------------------------

    @Test
    fun `parseConfig builds options with choices`() {
        val result = json.parseToJsonElement("""
            {"configOptions": [
              {"id": "provider", "name": "Provider", "currentValue": "openai",
               "options": [{"value": "openai", "name": "OpenAI"}, {"value": "localai", "name": "LocalAI"}]}
            ]}
        """).jsonObject

        val config = client.parseConfig(result)
        assertEquals(1, config.size)
        val c = config[0]
        assertEquals("provider", c.id)
        assertEquals("openai", c.currentValue)
        assertEquals(2, c.choices.size)
        assertEquals("OpenAI", c.choices[0].label)
    }

    // --- skills ------------------------------------------------------------------------

    @Test
    fun `parseSkills reads writable and global flags`() {
        val result = json.parseToJsonElement("""
            {"sources": [
              {"name": "my-skill", "description": "d", "content": "# Skill", "path": "/s.md",
               "global": false, "writable": true}
            ]}
        """).jsonObject

        val skills = client.parseSkills(result)
        assertEquals(1, skills.size)
        assertTrue(skills[0].writable)
        assertEquals(false, skills[0].global)
        assertEquals("# Skill", skills[0].content)
    }
}
