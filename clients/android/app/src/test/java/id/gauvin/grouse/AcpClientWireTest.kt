package id.gauvin.grouse

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit

/**
 * Wire tests: real AcpClient against a MockWebServer WebSocket. These defend what the client
 * actually SENDS — the secret-key header, _meta.client on session/new (without it a chat is an
 * 'acp' session invisible in Desktop), and the snake_case cron_schedule that recipes/schedule
 * demands (a camelCase spelling silently leaves the recipe unscheduled).
 */
class AcpClientWireTest {

    private val json = Json { ignoreUnknownKeys = true }
    private val received = CopyOnWriteArrayList<String>()

    /** A server that completes the initialize handshake so session/new and later calls flow. */
    private fun handshakeServer(): MockWebServer {
        val server = MockWebServer()
        server.enqueue(MockResponse().withWebSocketUpgrade(object : WebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: Response) {}
            override fun onMessage(webSocket: WebSocket, text: String) {
                received.add(text)
                val method = json.parseToJsonElement(text).jsonObject
                    .get("method")?.jsonPrimitive?.contentOrNull
                if (method == "initialize") {
                    val id = json.parseToJsonElement(text).jsonObject
                        .get("id")!!.jsonPrimitive.content
                    webSocket.send("""{"jsonrpc":"2.0","id":$id,"result":{}}""")
                }
            }
        }))
        return server
    }

    private fun wsUrl(server: MockWebServer) =
        server.url("/acp").toString().replaceFirst("http", "ws")

    private fun waitForFrame(substring: String, timeoutMs: Long = 5000): String {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (true) {
            received.find { it.contains(substring) }?.let { return it }
            if (System.currentTimeMillis() > deadline) {
                throw AssertionError("no frame containing '$substring' within ${timeoutMs}ms; got: $received")
            }
            Thread.sleep(20)
        }
    }

    @Test
    fun `connect sends the secret key header and initialize frame`() {
        val server = MockWebServer()
        server.enqueue(MockResponse().withWebSocketUpgrade(object : WebSocketListener() {
            override fun onMessage(webSocket: WebSocket, text: String) { received.add(text) }
        }))
        val client = AcpClient(wsUrl(server), "sekret-123") {}
        client.connect()

        val upgrade = server.takeRequest(5, TimeUnit.SECONDS)
            ?: throw AssertionError("no WebSocket upgrade request arrived")
        assertEquals("sekret-123", upgrade.getHeader("X-Secret-Key"))

        val init = waitForFrame("\"initialize\"")
        assertTrue(init.contains("\"protocolVersion\""))
        server.shutdown()
    }

    @Test
    fun `session new carries _meta client and the configured cwd`() {
        val server = handshakeServer()
        val client = AcpClient(wsUrl(server), "k") {}
        client.desiredCwd = "/home/user/Projects/Inbox"
        client.connect()

        val frame = waitForFrame("\"session/new\"")
        assertTrue("_meta.client=grouse required for Desktop visibility", frame.contains("\"client\""))
        assertTrue(frame.contains("grouse"))
        assertTrue(frame.contains("/home/user/Projects/Inbox"))
        server.shutdown()
    }

    @Test
    fun `scheduleRecipe sends cron_schedule, never a camelCase cron`() {
        val server = handshakeServer()
        val client = AcpClient(wsUrl(server), "k") {}
        client.connect()
        waitForFrame("\"session/new\"")   // handshake complete before issuing the call

        client.scheduleRecipe("r1", "0 9 * * *")
        val frame = waitForFrame("recipes/schedule")
        assertTrue(frame.contains("\"cron_schedule\":\"0 9 * * *\""))
        // The exact gotcha: a wrong spelling does not error — the recipe is silently
        // unscheduled. cron_schedule is the only key that works.
        assertFalse(frame.contains("\"cron\":"))
        server.shutdown()
    }

    @Test
    fun `stream events for another session are dropped, the bound session renders`() {
        // The wrong-chat bug: goose broadcasts session/update for OTHER sessions onto the same
        // socket. A finished-turn response for a chat the user left must not render in the chat
        // that is on screen. This binds the client to "sess-B" (full handshake), then feeds one
        // chunk for a foreign session and one for the bound session.
        val server = MockWebServer()
        server.enqueue(MockResponse().withWebSocketUpgrade(object : WebSocketListener() {
            override fun onMessage(webSocket: WebSocket, text: String) {
                received.add(text)
                val obj = json.parseToJsonElement(text).jsonObject
                val method = obj.get("method")?.jsonPrimitive?.contentOrNull
                val id = obj.get("id")?.jsonPrimitive?.contentOrNull ?: return
                when (method) {
                    "initialize" -> webSocket.send("""{"jsonrpc":"2.0","id":$id,"result":{}}""")
                    "session/new" -> webSocket.send(
                        """{"jsonrpc":"2.0","id":$id,"result":{"sessionId":"sess-B"}}""")
                }
            }
        }))
        val events = java.util.concurrent.CopyOnWriteArrayList<AcpEvent>()
        val client = AcpClient(wsUrl(server), "k") { events.add(it) }
        client.desiredCwd = "/home/user/Projects/Inbox"
        client.connect()
        waitForEvents(events) { it.any { e -> e is AcpEvent.Ready && e.sessionId == "sess-B" } }
        events.clear()

        // A response for ANOTHER session must not land in the on-screen transcript.
        client.handle("""{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-A","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"reply for A"}}}}""")
        assertEquals(0, events.size)

        // The bound session's own stream renders as usual.
        client.handle("""{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-B","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"reply for B"}}}}""")
        assertEquals(listOf(AcpEvent.AgentChunk("reply for B", null)), events)
        server.shutdown()
    }

    private fun waitForEvents(
        events: java.util.concurrent.CopyOnWriteArrayList<AcpEvent>,
        timeoutMs: Long = 5000,
        cond: (List<AcpEvent>) -> Boolean,
    ) {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (!cond(events)) {
            if (System.currentTimeMillis() > deadline) throw AssertionError("timeout waiting for events: $events")
            Thread.sleep(20)
        }
    }
}
