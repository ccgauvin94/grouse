// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * S-A-4: chart specs are interpolated into a <script> block (`const spec = __SPEC__;`).
 * chartHtml must JSON-encode the spec and neutralize `<` so a spec embedding `</script>`
 * cannot terminate the surrounding script element (script-breakout / HTML injection).
 */
class ChartHtmlTest {

    @Test
    fun chartHtml_escapes_script_breakout() {
        val out = chartHtml("</script><b>x")
        // org.json.JSONObject.quote escapes `/` after `<` too, so the injected `</script>` becomes
        // `\u003c\/script>` (or with `<` neutralised): the point is no `<` from the injected string
        // may survive, because a surviving `<` could terminate the template's own <script> block.
        assertTrue("expected a neutralized < (\\\\u003c) but was: $out", out.contains("\\u003c"))
        // No raw `</script>` from the injected string may survive.
        assertFalse("raw </script> leaked into: $out", out.contains("</script><b>x"))
        // The template's own trailing </script> must still exist (i.e. we didn't break the tag).
        assertTrue(out.endsWith("</script></body></html>"))
    }

    @Test
    fun chartHtml_embeds_plain_spec() {
        val out = chartHtml("plain")
        assertTrue("spec not embedded: $out", out.contains("const spec = \"plain\";"))
    }
}
