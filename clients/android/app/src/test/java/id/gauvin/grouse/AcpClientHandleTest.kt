package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * End-to-end dispatch through AcpClient.handle(): feeds raw JSON-RPC frames exactly as they
 * arrive on the WebSocket and asserts the AcpEvents surfaced. handle() is internal so tests
 * drive the real dispatcher without a socket; with ws == null the outbound side is a no-op.
 *
 * The expensive contracts live here: session_info_update is THREE notifications in one tag
 * (distinguished only by _meta.goose key presence), MCP-App tool_calls declare themselves via
 * _meta.goose.mcpApp, and the chart tool's data arrives as a JSON OBJECT.
 */
class AcpClientHandleTest {

    private val events = mutableListOf<AcpEvent>()
    private val client = AcpClient("ws://localhost:1/acp", "test-key") { events.add(it) }

    private fun handle(frame: String) {
        events.clear()
        client.handle(frame)
    }

    private fun last(): AcpEvent = events.last()

    // --- streaming chunks --------------------------------------------------------------

    @Test
    fun `agent chunk carries messageId from _meta for replay boundaries`() {
        handle("""{"jsonrpc":"2.0","method":"session/update","params":{"update":{
            "sessionUpdate":"agent_message_chunk",
            "content":{"type":"text","text":"Hello"},
            "_meta":{"goose":{"messageId":"m-1"}}}}}
        """)
        assertEquals(listOf(AcpEvent.AgentChunk("Hello", "m-1")), events)
    }

    @Test
    fun `user chunk and thought chunk dispatch`() {
        handle("""{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"ask"}}}}""")
        assertEquals(listOf(AcpEvent.UserChunk("ask", null)), events)

        handle("""{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"hm"}}}}""")
        assertEquals(listOf(AcpEvent.ThoughtChunk("hm")), events)
    }

    @Test
    fun `non-text content blocks render as placeholders instead of vanishing`() {
        handle("""{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"resource_link","uri":"u","name":"file"}}}}""")
        assertEquals(listOf(AcpEvent.AgentChunk("[resource: file]", null)), events)
    }

    // --- tool_call: MCP-App, chart object-data, and command detail ---------------------

    @Test
    fun `mcpApp tool call builds app fields and key`() {
        handle("""{"jsonrpc":"2.0","method":"session/update","params":{"update":{
            "sessionUpdate":"tool_call","title":"Show chart","toolCallId":"tc1",
            "rawInput":{"data":{"labels":["a"]}},
            "_meta":{"goose":{
                "toolCall":{"toolName":"autovisualiser__show_chart"},
                "mcpApp":{"resourceUri":"ui://chart","extensionName":"chart-ext"}}}}}}
        """)
        val tc = last() as AcpEvent.ToolCall
        assertEquals("Show chart", tc.title)
        assertEquals("tc1", tc.toolCallId)
        assertEquals("ui://chart", tc.appUri)
        assertEquals("chart-ext", tc.appExt)
        assertEquals("chart-ext|ui://chart", tc.appKey)
        assertTrue(tc.appInput.contains("\"labels\""))
    }

    @Test
    fun `chart data arriving as an OBJECT still renders a chart`() {
        // The regression that disabled every chart once: data is a JSON object, and reading
        // only the string form silently dropped it.
        handle("""{"jsonrpc":"2.0","method":"session/update","params":{"update":{
            "sessionUpdate":"tool_call","title":"c","toolCallId":"tc1",
            "rawInput":{"data":{"type":"bar","values":[1,2]}},
            "_meta":{"goose":{"toolCall":{"toolName":"autovisualiser__show_chart"}}}}}}
        """)
        val chart = last() as AcpEvent.Chart
        assertTrue(chart.spec.contains("\"bar\""))
        assertTrue(chart.spec.contains("\"values\""))
    }

    @Test
    fun `chart data as a plain string still renders`() {
        handle("""{"jsonrpc":"2.0","method":"session/update","params":{"update":{
            "sessionUpdate":"tool_call","title":"c","toolCallId":"tc1",
            "rawInput":{"data":"{\"type\":\"line\"}"},
            "_meta":{"goose":{"toolCall":{"toolName":"autovisualiser__show_chart"}}}}}}
        """)
        assertEquals(AcpEvent.Chart("{\"type\":\"line\"}"), last())
    }

    @Test
    fun `plain tool call keeps command as detail`() {
        handle("""{"jsonrpc":"2.0","method":"session/update","params":{"update":{
            "sessionUpdate":"tool_call","title":"Run build","toolCallId":"tc1",
            "rawInput":{"command":"cargo build --release","cwd":"/p"},
            "_meta":{"goose":{"toolCall":{"toolName":"builtin__shell"}}}}}}
        """)
        assertEquals(AcpEvent.ToolCall("Run build", "cargo build --release", "tc1"), last())
    }

    // --- tool_call_update ---------------------------------------------------------------

    @Test
    fun `live_output chunks append as live updates`() {
        handle("""{"jsonrpc":"2.0","method":"session/update","params":{"update":{
            "sessionUpdate":"tool_call_update","toolCallId":"tc1","status":"in_progress",
            "_meta":{"toolNotification":{"type":"live_output","params":{"chunks":[
                {"stream":"stdout","output":"line1\n"},{"stream":"stderr","output":"line2\n"}]}}}}}}
        """)
        assertEquals(AcpEvent.ToolCallUpdate("tc1", "in_progress", "line1\nline2\n", live = true), last())
    }

    @Test
    fun `content array becomes output text on completion`() {
        handle("""{"jsonrpc":"2.0","method":"session/update","params":{"update":{
            "sessionUpdate":"tool_call_update","toolCallId":"tc1","status":"completed",
            "content":[{"type":"content","content":{"type":"text","text":"done"}}]}}}
        """)
        assertEquals(AcpEvent.ToolCallUpdate("tc1", "completed", "done"), last())
    }

    // --- session_info_update: three notifications, one tag -----------------------------

    @Test
    fun `activeRunId presence emits ActiveRun alongside the info change`() {
        handle("""{"jsonrpc":"2.0","method":"session/update","params":{
            "sessionId":"s1","update":{
                "sessionUpdate":"session_info_update","title":"New title","updatedAt":"t1",
                "_meta":{"goose":{"activeRunId":"run-9"}}}}}
        """)
        assertEquals(
            listOf(
                AcpEvent.ActiveRun("s1", "run-9"),
                AcpEvent.SessionInfoChanged("s1", "New title", "t1"),
            ),
            events)
    }

    @Test
    fun `no activeRunId emits only the info change`() {
        handle("""{"jsonrpc":"2.0","method":"session/update","params":{
            "sessionId":"s1","update":{
                "sessionUpdate":"session_info_update","title":"Renamed","updatedAt":"t2"}}}
        """)
        assertEquals(listOf(AcpEvent.SessionInfoChanged("s1", "Renamed", "t2")), events)
    }

    // --- usage / mode / commands / config ----------------------------------------------

    @Test
    fun `usage_update maps cost object`() {
        handle("""{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"usage_update","used":100,"size":200,"cost":{"amount":0.0042,"currency":"USD"}}}}""")
        assertEquals(AcpEvent.Usage(100, 200, 0.0042, "USD"), last())
    }

    @Test
    fun `config_option_update surfaces config`() {
        handle("""{"jsonrpc":"2.0","method":"session/update","params":{"update":{
            "sessionUpdate":"config_option_update",
            "configOptions":[{"id":"mode","name":"Mode","currentValue":"auto","options":[]}]}}}
        """)
        val cfg = last() as AcpEvent.Config
        assertEquals("mode", cfg.options[0].id)
        assertEquals("auto", cfg.options[0].currentValue)
    }

    @Test
    fun `current_mode_update and available_commands_update dispatch`() {
        handle("""{"jsonrpc":"2.0","method":"session/update","params":{"update":{
            "sessionUpdate":"current_mode_update","currentModeId":"plan"}}}
        """)
        assertEquals(AcpEvent.ModeChanged("plan"), last())

        handle("""{"jsonrpc":"2.0","method":"session/update","params":{"update":{
            "sessionUpdate":"available_commands_update",
            "availableCommands":[{"name":"/compact"},{"name":"/share"}]}}}
        """)
        assertEquals(AcpEvent.Commands(listOf("/compact", "/share")), last())
    }

    // --- goose-custom session/update ----------------------------------------------------

    @Test
    fun `goose status_message surfaces compaction status`() {
        handle("""{"jsonrpc":"2.0","method":"_goose/unstable/session/update","params":{"update":{"sessionUpdate":"status_message","status":{"message":"compacting…"}}}}""")
        assertEquals(AcpEvent.CompactionStatus("compacting…"), last())
    }

    @Test
    fun `goose message_usage derives per-message stats`() {
        handle("""{"jsonrpc":"2.0","method":"_goose/unstable/session/update","params":{"update":{
            "sessionUpdate":"message_usage",
            "usage":{"outputTokens":120,"elapsedMs":2000,"timeToFirstTokenMs":150,"cost":0.001}}}}
        """)
        assertEquals(AcpEvent.MessageUsage(120, 2000, 150, 0.001), last())
    }

    // --- server requests ----------------------------------------------------------------

    @Test
    fun `request_permission surfaces options and command detail`() {
        handle("""{"jsonrpc":"2.0","method":"session/request_permission","id":7,"params":{"toolCall":{"toolCallId":"t1","title":"Run","rawInput":{"command":"ls -la"}},"options":[{"optionId":"once","name":"Once","kind":"allow_once"},{"optionId":"always","name":"Always","kind":"allow_always"}]}}""")
        val perm = last() as AcpEvent.Permission
        assertEquals("t1", perm.toolCallId)
        assertEquals("Run", perm.title)
        assertEquals("ls -la", perm.detail)
        assertEquals(2, perm.options.size)
        assertEquals("once", perm.options[0].optionId)
        assertEquals("allow_once", perm.options[0].kind)
    }

    @Test
    fun `elicitation form parses oneOf and enum choices`() {
        handle("""{"jsonrpc":"2.0","method":"elicitation/create","id":8,"params":{"mode":"form","message":"Fill in","requestedSchema":{"title":"Details","required":["name"],"properties":{"name":{"type":"string","title":"Name"},"color":{"type":"string","oneOf":[{"const":"red","title":"Red"}]},"size":{"type":"string","enum":["s","m"]}}}}}""")
        val elicit = last() as AcpEvent.Elicitation
        assertTrue(elicit.requestKey.startsWith("elicit-"))
        assertEquals("Fill in", elicit.message)
        assertEquals("Details", elicit.title)
        val byName = elicit.fields.associateBy { it.name }
        assertTrue(byName.getValue("name").required)
        assertEquals(listOf(Choice("red", "Red")), byName.getValue("color").options)
        assertEquals(listOf(Choice("s", "s"), Choice("m", "m")), byName.getValue("size").options)
    }

    @Test
    fun `recipe request-params reads snake_case input_type and surfaces defaults`() {
        handle("""{"jsonrpc":"2.0","method":"_goose/unstable/session/recipe/request-params","id":9,"params":{"parameters":[{"key":"topic","input_type":"string","description":"What about","default":"tech","requirement":"required","options":["tech","food"]}]}}""")
        val elicit = last() as AcpEvent.Elicitation
        assertTrue(elicit.requestKey.startsWith("recipeparams-"))
        assertEquals("Recipe parameters", elicit.title)
        val f = elicit.fields[0]
        assertEquals("topic", f.name)
        assertEquals("string", f.type)
        assertEquals("What about (default: tech)", f.description)
        assertTrue(f.required)
        assertEquals(listOf(Choice("tech", "tech"), Choice("food", "food")), f.options)
    }

    // --- malformed input ----------------------------------------------------------------

    @Test
    fun `bad json surfaces an error instead of throwing`() {
        client.handle("this is not json")
        assertEquals(1, events.size)
        assertTrue((events[0] as AcpEvent.Error).text.startsWith("bad json:"))
    }

    @Test
    fun `unknown notification method is silently ignored`() {
        handle("""{"jsonrpc":"2.0","method":"totally/unknown","params":{}}""")
        assertEquals(0, events.size)
    }

    @Test
    fun `a structurally wrong update is dropped, not fatal`() {
        handle("""{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":42}}}""")
        assertEquals(0, events.size)
    }
}
