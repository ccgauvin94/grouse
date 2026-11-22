// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** The chart spec is server-controlled and the WebView has JavaScript enabled. Errors must
 *  be written as TEXT (not markup), under a CSP, with the bundled chart.min.js still loadable.
 *  The navigation block itself lives in the WebViewClient (not testable on the JVM). */
class ChartSecurityTest {

    @Test
    fun `errors are text content, never innerHTML`() {
        val out = chartHtml("{}")
        assertFalse("innerHTML is an XSS sink here: $out", out.contains("innerHTML"))
        assertTrue(out.contains("textContent"))
        assertTrue(out.contains("document.createElement('pre')"))
    }

    @Test
    fun `a restrictive CSP is present and Chart js still loads`() {
        val out = chartHtml("{}")
        assertTrue(out.contains("Content-Security-Policy"))
        assertTrue(out.contains("default-src 'none'"))
        assertTrue(out.contains("connect-src 'none'"))
        assertTrue(out.contains("frame-src 'none'"))
        assertTrue("chart.min.js must still load", out.contains("<script src=\"chart.min.js\"></script>"))
    }

    @Test
    fun `script breakout is still neutralized`() {
        val out = chartHtml("</script><img src=x onerror=alert(1)>")
        assertTrue(out.contains("\\u003c"))
        assertFalse(out.contains("</script><img"))
    }
}
