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
