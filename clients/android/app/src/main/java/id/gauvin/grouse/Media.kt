package id.gauvin.grouse

import android.content.Context
import android.graphics.Bitmap
import android.net.Uri
import android.util.Base64
import java.io.ByteArrayOutputStream

/** Read a content Uri into a base64 image ContentBlock for a prompt. */
fun readImage(context: Context, uri: Uri): ImageBlock? = runCatching {
    val mime = context.contentResolver.getType(uri) ?: "image/jpeg"
    val bytes = context.contentResolver.openInputStream(uri)?.use { it.readBytes() } ?: return null
    ImageBlock(mime, Base64.encodeToString(bytes, Base64.NO_WRAP))
}.getOrNull()

/** Read a non-image content Uri: text files become a text block, everything else a base64 blob. */
fun readFile(context: Context, uri: Uri, name: String): FileBlock? = runCatching {
    val mime = context.contentResolver.getType(uri) ?: "application/octet-stream"
    val bytes = context.contentResolver.openInputStream(uri)?.use { it.readBytes() } ?: return null
    val text = if (mime.startsWith("text/") ||
        mime in setOf("application/json", "application/xml", "application/javascript",
            "application/x-yaml", "application/x-sh", "application/toml", "application/sql",
            "application/csv", "application/x-httpd-php") && isProbablyText(bytes)) {
        String(bytes, Charsets.UTF_8)
    } else null
    if (text != null) FileBlock(name, mime, text = text)
    else FileBlock(name, mime, blobB64 = Base64.encodeToString(bytes, Base64.NO_WRAP))
}.getOrNull()

private fun isProbablyText(bytes: ByteArray): Boolean {
    val sample = bytes.take(2048)
    val printable = sample.count { b ->
        b == 9.toByte() || b == 10.toByte() || b == 13.toByte() || b in 32..126 || b.toInt() >= 0x80
    }
    return sample.isEmpty() || printable * 10 >= sample.size * 9
}

/** Encode a camera capture (bitmap) as a JPEG image block. */
fun bitmapToImage(bitmap: Bitmap): ImageBlock = runCatching {
    val out = ByteArrayOutputStream()
    bitmap.compress(Bitmap.CompressFormat.JPEG, 88, out)
    ImageBlock("image/jpeg", Base64.encodeToString(out.toByteArray(), Base64.NO_WRAP))
}.getOrElse { ImageBlock("image/jpeg", "") }
