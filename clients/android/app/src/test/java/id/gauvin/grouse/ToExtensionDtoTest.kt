package id.gauvin.grouse

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * toExtensionDto: the shape goose LISTS extensions in (config.yaml spelling) is NOT the shape
 * the add methods accept. Feeding a listing straight back fails with -32602 — and since
 * tool-allowlist editing is remove-then-add, the extension gets DELETED with the error
 * surfacing nowhere. This conversion is the guard against that.
 */
class ToExtensionDtoTest {

    private val json = Json { ignoreUnknownKeys = true }

    private fun dto(raw: String) = toExtensionDto(json.parseToJsonElement(raw).jsonObject)

    @Test
    fun `streamable_http becomes an mcp http server with headers as array`() {
        val out = dto("""{"type":"streamable_http","name":"kagi","uri":"https://kagi.local/mcp",
            "headers":{"Authorization":"Bearer x","X-Tenant":"t1"},"timeout":30,"description":"Kagi"}""")

        assertEquals("mcp", out["type"]?.jsonPrimitive?.content)
        val server = out["server"]?.jsonObject
        assertEquals("http", server?.get("type")?.jsonPrimitive?.content)
        assertEquals("kagi", server?.get("name")?.jsonPrimitive?.content)
        assertEquals("https://kagi.local/mcp", server?.get("url")?.jsonPrimitive?.content)
        val headers = server?.get("headers")?.jsonArray
        assertEquals(2, headers?.size)
        assertEquals("Authorization", headers?.get(0)?.jsonObject?.get("name")?.jsonPrimitive?.content)
        assertEquals("Bearer x", headers?.get(0)?.jsonObject?.get("value")?.jsonPrimitive?.content)
        assertEquals(30, out["timeout"]?.jsonPrimitive?.content?.toInt())
        assertEquals("Kagi", out["description"]?.jsonPrimitive?.content)
    }

    @Test
    fun `sse keeps its transport type`() {
        val out = dto("""{"type":"sse","name":"s","uri":"https://x/sse"}""")
        val server = out["server"]?.jsonObject
        assertEquals("sse", server?.get("type")?.jsonPrimitive?.content)
        assertEquals("https://x/sse", server?.get("url")?.jsonPrimitive?.content)
    }

    @Test
    fun `stdio becomes an mcp stdio server`() {
        val out = dto("""{"type":"stdio","name":"node","cmd":"node","args":["mcp.js"],"env_keys":["HOME"]}""")
        val server = out["server"]?.jsonObject
        assertEquals("stdio", server?.get("type")?.jsonPrimitive?.content)
        assertEquals("node", server?.get("command")?.jsonPrimitive?.content)
        assertEquals("mcp.js", server?.get("args")?.jsonArray?.get(0)?.jsonPrimitive?.content)
        // env_keys is copied as camelCase envKeys into the accept shape.
        assertEquals("HOME", out["envKeys"]?.jsonArray?.get(0)?.jsonPrimitive?.content)
    }

    @Test
    fun `builtin platform and mcp pass through untouched`() {
        for (type in listOf("builtin", "platform", "mcp")) {
            val raw = """{"type":"$type","name":"x"}"""
            val out = dto(raw)
            assertEquals("passthrough for $type", raw.replace(" ", ""), out.toString())
        }
    }

    @Test
    fun `unknown type with no name is returned as-is rather than mangled`() {
        val raw = """{"type":"weird"}"""
        assertEquals(raw, dto(raw).toString())
    }
}
