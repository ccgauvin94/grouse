// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Which server-shared files paint INLINE in the transcript instead of arriving as
 * a download chip.
 *
 * The rule is a raster allowlist with a size cap, and every branch here is one
 * that would otherwise be decided by a guess: SVG has no decoder on this platform
 * (an empty frame reads as a broken picture, not as a file), a size the server
 * never reported is not a reason to hide an image, and a byte count we KNOW is too
 * large is the one case where the row must stay a card and wait for a tap.
 */
class InlineImageTest {

    @Test
    fun raster_images_paint_inline() {
        for (mime in listOf("image/png", "image/jpeg", "image/gif", "image/webp", "image/heic")) {
            assertTrue(mime, isInlineImage(mime, "x.bin", 1024))
        }
    }

    @Test
    fun mime_parameters_and_case_do_not_hide_an_image() {
        assertTrue(isInlineImage("IMAGE/PNG; charset=binary", "x.png", 1024))
    }

    @Test
    fun a_generic_mime_falls_back_to_the_suffix() {
        // A relay that lost the type reports application/octet-stream. Treating that
        // as "not an image" is how a picture silently became a download chip.
        assertTrue(isInlineImage("application/octet-stream", "chart.png", 1024))
        assertFalse(isInlineImage("application/octet-stream", "report.pdf", 1024))
    }

    @Test
    fun an_unlisted_image_type_is_still_an_image() {
        assertTrue(isInlineImage("image/avif", "a.bin", 1024))
        assertFalse("no raster loader here", isInlineImage("image/tiff", "a.tiff", 1024))
    }

    @Test
    fun svg_stays_a_card() {
        // No SVG decoder on either client: painting nothing is worse than a chip.
        assertFalse(isInlineImage("image/svg+xml", "logo.svg", 900))
    }

    @Test
    fun a_known_too_large_image_stays_a_card() {
        assertFalse(isInlineImage("image/png", "huge.png", INLINE_IMAGE_MAX_BYTES + 1))
        assertTrue("the cap is inclusive", isInlineImage("image/png", "edge.png", INLINE_IMAGE_MAX_BYTES))
    }

    @Test
    fun an_unreported_size_is_not_a_reason_to_hide_it() {
        // The share's REQUEST carries no size and the result may never report one,
        // so 0/absent has to mean "unknown", not "empty".
        assertTrue(isInlineImage("image/png", "chart.png", 0))
        assertTrue(isInlineImage("image/png", "chart.png", -1))
    }

    @Test
    fun mime_beats_a_misleading_suffix() {
        assertTrue(isInlineImage("image/png", "chart.txt", 1024))
        assertFalse("a PNG name does not make a text file an image",
            isInlineImage("text/plain", "chart.png", 1024))
    }

    @Test
    fun a_reference_with_no_mime_falls_back_to_the_suffix() {
        assertTrue(isInlineImage("", "screenshot.JPG", 1024))
        assertFalse(isInlineImage("", "report.pdf", 1024))
    }
}
