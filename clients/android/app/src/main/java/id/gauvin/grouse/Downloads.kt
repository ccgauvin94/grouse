package id.gauvin.grouse

import android.content.ContentValues
import android.content.Context
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import java.io.File

/**
 * Copy a file the server shared out of app-private storage into the user's
 * Downloads folder, so it is visible in Files and survives clearing app data.
 *
 * The core fetches into `filesDir/files/<digest16>-<name>` because that is the
 * only place it can write without permissions and the only place the
 * FileProvider can serve from. That is right for a preview, wrong for a
 * document: a 15 MB installer sitting in `/data/data/…` is invisible to every
 * file manager and is deleted with the app.
 *
 * Returns the `content://` URI to open, or null when the copy could not be made
 * (the caller then keeps the private file and opens that instead). MediaStore's
 * Downloads collection needs no permission from API 29 on; below that the public
 * directory needs WRITE_EXTERNAL_STORAGE, so those devices keep the private
 * copy rather than growing a permission prompt for a file preview.
 */
internal fun exportToDownloads(
    ctx: Context,
    name: String,
    mime: String,
    localPath: String,
): String? {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return null
    val src = File(localPath)
    if (!src.isFile) return null
    val resolver = ctx.contentResolver
    val displayName = name.ifBlank { src.name }
    val values = ContentValues().apply {
        put(MediaStore.Downloads.DISPLAY_NAME, displayName)
        put(MediaStore.Downloads.MIME_TYPE, mime.ifBlank { "application/octet-stream" })
        // A subfolder keeps Grouse's output out of the way of everything else
        // downloaded on the device, which is what makes it findable again.
        put(
            MediaStore.Downloads.RELATIVE_PATH,
            "${Environment.DIRECTORY_DOWNLOADS}/Grouse",
        )
        // Invisible until the bytes are complete: without this a half-copied
        // file can be opened (or installed) from Files while it is still being
        // written.
        put(MediaStore.Downloads.IS_PENDING, 1)
    }
    val uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values) ?: return null
    return runCatching {
        resolver.openOutputStream(uri).use { out ->
            requireNotNull(out) { "no output stream for $uri" }
            src.inputStream().use { input -> input.copyTo(out) }
        }
        resolver.update(
            uri,
            ContentValues().apply { put(MediaStore.Downloads.IS_PENDING, 0) },
            null,
            null,
        )
        uri.toString()
    }.getOrElse {
        // A pending row with no bytes would sit in Downloads forever.
        runCatching { resolver.delete(uri, null, null) }
        null
    }
}
