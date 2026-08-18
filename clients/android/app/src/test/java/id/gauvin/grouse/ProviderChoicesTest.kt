package id.gauvin.grouse

import id.gauvin.grouse.ConnectionManager.Companion.parseProviders
import id.gauvin.grouse.ConnectionManager.Companion.providerChoices
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** Which providers a picker offers. The dropdowns used to carry their own hardcoded
 *  three-item list as a Composable default parameter that no call site ever overrode, so
 *  "Show all providers" changed nothing at all — the assertion below that the toggle must
 *  change the list is the one that would have caught it. */
class ProviderChoicesTest {

    private fun p(id: String, configured: Boolean, models: List<String> = emptyList()) =
        ProviderInfo(id = id, name = id, configured = configured, models = models)

    private val inventory = listOf(
        p("openai", true), p("openrouter", true),
        p("openrouter_custom", false), p("anthropic", false), p("ollama", false),
    )
    private val catalog = inventory.map { it.id }

    @Test
    fun `off shows only what this goose has configured`() {
        assertEquals(
            listOf("openai", "openrouter"),
            providerChoices(inventory, showAll = false, current = "openai"),
        )
    }

    @Test
    fun `on shows the whole catalog`() {
        assertEquals(catalog, providerChoices(inventory, showAll = true, current = "openai"))
    }

    @Test
    fun `the toggle actually changes the list`() {
        val off = providerChoices(inventory, showAll = false, current = "")
        val on = providerChoices(inventory, showAll = true, current = "")
        assertTrue("the toggle must change something", off != on)
        assertTrue(on.size > off.size)
    }

    @Test
    fun `a selected provider outside the list stays visible`() {
        // Otherwise the current value silently vanishes from its own picker and the row
        // reads as if nothing is selected.
        val choices = providerChoices(inventory, showAll = false, current = "anthropic")
        assertTrue("current selection must remain selectable: $choices", "anthropic" in choices)
        assertEquals("anthropic", choices.last())
    }

    @Test
    fun `a blank selection adds nothing`() {
        assertEquals(
            listOf("openai", "openrouter"),
            providerChoices(inventory, showAll = false, current = ""),
        )
    }

    @Test
    fun `an already-listed selection is not duplicated`() {
        val choices = providerChoices(inventory, showAll = true, current = "ollama")
        assertEquals(1, choices.count { it == "ollama" })
    }

    // -- the inventory itself, straight off the wire ------------------------------------

    // The real shape, copied from a live `_goose/unstable/providers/list`: models carry
    // BOTH an id and a display name, and they differ.
    private val wire = """
      [ {"providerId":"openai","providerName":"OpenAI","configured":true,
          "models":[{"id":"gpt-4o","name":"GPT-4o","contextLimit":128000},
                    {"id":"gpt-4o-mini","name":"GPT-4o mini"}]},
        {"providerId":"anthropic","providerName":"Anthropic","configured":false,
          "models":[{"id":"global.anthropic.claude-sonnet-5",
                     "name":"Claude Sonnet 5 (Global)","reasoning":true}]} ]
    """

    @Test
    fun `parses goose's provider inventory`() {
        val got = parseProviders(wire)
        assertEquals(2, got.size)
        assertEquals("openai", got[0].id)
        assertEquals("OpenAI", got[0].name)
        assertTrue(got[0].configured)
        assertEquals(listOf("gpt-4o", "gpt-4o-mini"), got[0].models)
        // `configured` is the SERVER's judgement; the app must not assume.
        assertTrue(!got[1].configured)
    }

    @Test
    fun `configured drives the default picker, not a hardcoded set`() {
        val inv = parseProviders(wire)
        assertEquals(listOf("openai"), providerChoices(inv, showAll = false, current = ""))
        assertEquals(listOf("openai", "anthropic"), providerChoices(inv, showAll = true, current = ""))
    }

    @Test
    fun `each provider carries its own models`() {
        val inv = parseProviders(wire)
        // The IDENTIFIER, not the display name: this is what goes into GOOSE_MODEL, and
        // "Claude Sonnet 5 (Global)" is not a model the server can resolve.
        assertEquals(
            listOf("global.anthropic.claude-sonnet-5"),
            inv.first { it.id == "anthropic" }.models,
        )
    }

    @Test
    fun `malformed or empty payloads yield an empty list, never a throw`() {
        assertEquals(emptyList<ProviderInfo>(), parseProviders(""))
        assertEquals(emptyList<ProviderInfo>(), parseProviders("not json"))
        assertEquals(emptyList<ProviderInfo>(), parseProviders("{}"))
        assertEquals(emptyList<ProviderInfo>(), parseProviders("[]"))
        // An entry with no providerId is skipped rather than poisoning the list.
        assertEquals(1, parseProviders("""[{"x":1},{"providerId":"ollama"}]""").size)
    }

    @Test
    fun `a plain string model list is tolerated`() {
        val got = parseProviders("""[{"providerId":"o","models":["a","b"]}]""")
        assertEquals(listOf("a", "b"), got[0].models)
    }
}
