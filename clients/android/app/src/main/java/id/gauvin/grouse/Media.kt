// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import android.content.Context
import android.net.Uri
import android.util.Base64

/** Read a content Uri into a base64 image ContentBlock for a prompt. */
fun readImage(context: Context, uri: Uri): ImageBlock? = runCatching {
    val mime = context.contentResolver.getType(uri) ?: "image/jpeg"
    val bytes = context.contentResolver.openInputStream(uri)?.use { it.readBytes() } ?: return null
    ImageBlock(mime, Base64.encodeToString(bytes, Base64.NO_WRAP))
}.getOrNull()

/**
 * Metadata for a non-image content Uri. Deliberately does NOT read the bytes:
 * the core streams them to the server during the upload, so reading the whole
 * file here would be a second copy of something we never send inline.
 */
fun readFile(context: Context, uri: Uri, name: String): FileBlock? = runCatching {
    val mime = context.contentResolver.getType(uri) ?: "application/octet-stream"
    // Ask the provider for the size so the chip can say "25.3 MB" before the
    // upload starts. -1 = it will not say, which the chip renders as no size
    // rather than a wrong one. The core re-reads the size from the same source
    // when it streams, so this is display only.
    val bytes = runCatching {
        context.contentResolver.query(uri, arrayOf(android.provider.OpenableColumns.SIZE), null, null, null)
            ?.use { c -> if (c.moveToFirst() && !c.isNull(0)) c.getLong(0) else -1L } ?: -1L
    }.getOrDefault(-1L)
    FileBlock(name, mime, uri, bytes)
}.getOrNull()
