// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Article
import androidx.compose.material.icons.automirrored.filled.InsertDriveFile
import androidx.compose.material.icons.filled.AudioFile
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Code
import androidx.compose.material.icons.filled.Description
import androidx.compose.material.icons.filled.FolderZip
import androidx.compose.material.icons.filled.Image
import androidx.compose.material.icons.filled.PictureAsPdf
import androidx.compose.material.icons.filled.Slideshow
import androidx.compose.material.icons.filled.TableChart
import androidx.compose.material.icons.filled.VideoFile
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import java.util.Locale

/**
 * What a chat bubble or a composer chip shows for one attached file.
 *
 * Deliberately NOT [FileBlock]: that one is a transport record (a `content://`
 * URI the core streams from) and it only exists before the send. After the send
 * the row carries the core's own record — name, MIME and byte count — which is
 * what survives a replayed transcript, so the display type is built from that.
 */
data class AttachedFile(
    val name: String,
    val mime: String,
    /** -1 when the provider would not report a size (the label then omits it). */
    val bytes: Long,
    /**
     * SERVER-side path, for a file the server shared ([ItemKind.FILE]); empty for
     * a file this device attached. It is the fetch key: `ConnectionManager.fetchFile`
     * sends it back and the core streams the bytes.
     */
    val path: String = "",
    /** Content digest, when the server reported one. Makes a re-fetch free. */
    val sha256: String = "",
)

/**
 * One server-shared file's download: idle (absent from the map) → downloading →
 * saved, or failed with the server's own message. Keyed by the SERVER path, which
 * is what the core reports on.
 */
data class FileDownload(
    /** downloading | saved | failed */
    val state: String,
    val received: Long,
    val total: Long,
    /** Where it landed locally (saved only). */
    val localPath: String,
    val message: String,
)

/** A send-time pick, as the card the composer and the bubble both draw. */
fun FileBlock.toAttachedFile(): AttachedFile = AttachedFile(name, mimeType, bytes)

/** The icon for a file, chosen by MIME and falling back to the name's suffix. */
fun attachmentIcon(mime: String, name: String): ImageVector {
    val m = mime.lowercase(Locale.US)
    val ext = name.substringAfterLast('.', "").lowercase(Locale.US)
    return when {
        m == "application/pdf" || ext == "pdf" -> Icons.Filled.PictureAsPdf
        m.contains("wordprocessingml") || m == "application/msword" ||
            m.contains("opendocument.text") || ext in setOf("doc", "docx", "odt", "rtf", "pages") ->
            Icons.Filled.Description
        m.contains("spreadsheetml") || m == "application/vnd.ms-excel" ||
            m.contains("opendocument.spreadsheet") || m == "text/csv" ||
            ext in setOf("xls", "xlsx", "ods", "csv", "numbers") -> Icons.Filled.TableChart
        m.contains("presentationml") || m == "application/vnd.ms-powerpoint" ||
            m.contains("opendocument.presentation") || ext in setOf("ppt", "pptx", "odp", "key") ->
            Icons.Filled.Slideshow
        m.startsWith("image/") -> Icons.Filled.Image
        m.startsWith("audio/") -> Icons.Filled.AudioFile
        m.startsWith("video/") -> Icons.Filled.VideoFile
        m.contains("zip") || m.contains("gzip") || m.contains("tar") || m.contains("7z") ||
            ext in setOf("zip", "gz", "tar", "7z", "rar", "bz2", "xz") -> Icons.Filled.FolderZip
        m.contains("json") || m.contains("javascript") || m.contains("xml") || m.contains("html") ||
            ext in setOf("json", "js", "ts", "xml", "html", "css", "py", "rs", "kt", "sh", "yaml", "yml") ->
            Icons.Filled.Code
        m.startsWith("text/") || ext in setOf("txt", "md", "log") -> Icons.AutoMirrored.Filled.Article
        else -> Icons.AutoMirrored.Filled.InsertDriveFile
    }
}

/** `PDF`, `Word`, `Excel`, `Text`… — the kind, for the second line of a chip. */
fun attachmentKind(mime: String, name: String): String = when (attachmentIcon(mime, name)) {
    Icons.Filled.PictureAsPdf -> "PDF"
    Icons.Filled.Description -> "Document"
    Icons.Filled.TableChart -> "Spreadsheet"
    Icons.Filled.Slideshow -> "Presentation"
    Icons.Filled.Image -> "Image"
    Icons.Filled.AudioFile -> "Audio"
    Icons.Filled.VideoFile -> "Video"
    Icons.Filled.FolderZip -> "Archive"
    Icons.Filled.Code -> "Code"
    Icons.AutoMirrored.Filled.Article -> "Text"
    else -> "File"
}

/**
 * 812 B / 108 KB / 25.3 MB / 1.2 GB.
 *
 * One decimal only where it carries information (10 KB, not 10.0 KB), because a
 * chip is small and the number is a hint, not a measurement. `-1` means the
 * provider would not say, and renders as nothing at all rather than "0 B".
 */
fun formatBytes(bytes: Long): String {
    if (bytes < 0) return ""
    if (bytes < 1024) return "$bytes B"
    val units = listOf("KB", "MB", "GB", "TB")
    var value = bytes / 1024.0
    var unit = 0
    while (value >= 1024 && unit < units.lastIndex) {
        value /= 1024
        unit++
    }
    val pattern = if (value >= 100 || value == value.toLong().toDouble()) "%.0f" else "%.1f"
    return String.format(Locale.getDefault(), "$pattern %s", value, units[unit])
}

/** The second line of a chip: `PDF · 25.3 MB`, or just the kind when unsized. */
fun attachmentSubtitle(file: AttachedFile): String {
    val kind = attachmentKind(file.mime, file.name)
    val size = formatBytes(file.bytes)
    // "Image 1" already says what it is; do not repeat it in the subtitle.
    val showKind = !file.name.startsWith(kind, ignoreCase = true)
    return when {
        !showKind -> size
        size.isEmpty() -> kind
        else -> "$kind · $size"
    }
}

/**
 * One attached file, as a card: icon, name, kind and size.
 *
 * The same composable is the composer's chip (`onRemove` given, so it can be
 * taken back before sending) and the bubble's record of what went with a
 * message (`onRemove` null — nothing to undo once it is on the server). One
 * shape for both is the point: the thing you picked is the thing you see after
 * you send it.
 */
@Composable
fun AttachmentCard(
    file: AttachedFile,
    modifier: Modifier = Modifier,
    container: Color = MaterialTheme.colorScheme.surfaceVariant,
    content: Color = MaterialTheme.colorScheme.onSurfaceVariant,
    onRemove: (() -> Unit)? = null,
) {
    Row(
        modifier = modifier
            .background(container, RoundedCornerShape(10.dp))
            .padding(start = 10.dp, end = if (onRemove != null) 2.dp else 10.dp, top = 7.dp, bottom = 7.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(attachmentIcon(file.mime, file.name), contentDescription = null,
            tint = content, modifier = Modifier.size(20.dp))
        Spacer(Modifier.width(9.dp))
        Column(Modifier.weight(1f, fill = false)) {
            Text(file.name, style = MaterialTheme.typography.labelLarge, color = content,
                maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(attachmentSubtitle(file), style = MaterialTheme.typography.labelSmall,
                color = content.copy(alpha = 0.68f), maxLines = 1)
        }
        if (onRemove != null) {
            IconButton(onClick = onRemove, modifier = Modifier.size(32.dp)) {
                Icon(Icons.Filled.Close, contentDescription = "remove ${file.name}",
                    tint = content, modifier = Modifier.size(16.dp))
            }
        }
    }
}
