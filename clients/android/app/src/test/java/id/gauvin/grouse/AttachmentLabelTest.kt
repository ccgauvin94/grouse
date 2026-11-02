// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import java.util.Locale

/**
 * The size on an attachment card is the only thing that distinguishes a
 * screenshot from a 26 MB scanned deck at a glance, so the formatting is worth
 * pinning: it must never claim a size it does not have (`-1` = the provider
 * would not say), and it must not round a real file down to "0 B".
 */
class AttachmentLabelTest {

    // The card shows a decimal separator, which is locale-dependent; pin it so
    // the expectations below are about the rounding rule and not the test host.
    private lateinit var was: Locale

    @Before
    fun pinLocale() {
        was = Locale.getDefault()
        Locale.setDefault(Locale.US)
    }

    @After
    fun restoreLocale() {
        Locale.setDefault(was)
    }

    private fun size(bytes: Long): String = formatBytes(bytes)

    @Test
    fun an_unknown_size_renders_as_nothing() {
        // -1 is "the provider would not tell us", not an empty file.
        assertEquals("", size(-1))
    }

    @Test
    fun a_tiny_file_keeps_its_bytes() {
        assertEquals("0 B", size(0))
        assertEquals("812 B", size(812))
        assertEquals("1023 B", size(1023))
    }

    @Test
    fun kilobytes_drop_a_pointless_decimal() {
        assertEquals("1 KB", size(1024))
        assertEquals("106 KB", size(108_227))
        assertEquals("1023 KB", size(1023 * 1024))
    }

    @Test
    fun megabytes_keep_one_decimal_only_while_it_informs() {
        assertEquals("1 MB", size(1024L * 1024))
        assertEquals("25.3 MB", size(26_520_788))
        assertEquals("100 MB", size(104_857_600))
        assertEquals("1.5 MB", size(1_572_864))
    }

    @Test
    fun a_petabyte_is_still_a_number_not_a_crash() {
        val s = size(1024L * 1024 * 1024 * 1024 * 1024)
        assertTrue(s, s.endsWith("TB"))
    }

    @Test
    fun an_image_is_identified_by_its_mime_and_a_pdf_by_either() {
        // A picker that reports octet-stream still names the file .pdf, and the
        // card should say PDF rather than "File".
        assertEquals("PDF", attachmentKind("application/pdf", "scan"))
        assertEquals("PDF", attachmentKind("application/octet-stream", "scan.pdf"))
        assertEquals("Spreadsheet", attachmentKind("text/csv", "rows.csv"))
        assertEquals("File", attachmentKind("application/octet-stream", "blob"))
    }

    @Test
    fun the_subtitle_says_the_kind_and_the_size_but_never_twice() {
        assertEquals("PDF · 106 KB", attachmentSubtitle(AttachedFile("document-513", "application/pdf", 108_227)))
        // "Image 1" already says what it is; only the size is left to add.
        assertEquals("1.2 MB", attachmentSubtitle(AttachedFile("Image 1", "image/jpeg", 1_258_291)))
        // No size known: the kind alone, not a dangling separator.
        assertEquals("PDF", attachmentSubtitle(AttachedFile("a.pdf", "application/pdf", -1)))
    }

    @Test
    fun an_extensionless_name_is_still_classified() {
        // The case the server now fixes on the way in; a card drawn from an old
        // upload (or from the pre-send FileBlock) must not regress to "File".
        val f = AttachedFile("document-34", "application/pdf", 26_520_788)
        assertEquals("PDF", attachmentKind(f.mime, f.name))
        assertEquals("PDF · 25.3 MB", attachmentSubtitle(f))
    }
}
