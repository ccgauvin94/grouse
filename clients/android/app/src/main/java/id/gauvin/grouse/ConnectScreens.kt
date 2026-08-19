// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import android.content.Context
import android.graphics.Bitmap
import android.net.Uri
import android.util.Base64
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.expandVertically
import androidx.compose.animation.shrinkVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.clickable
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material.icons.filled.ArrowUpward
import androidx.compose.material.icons.filled.Delete
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.runtime.snapshotFlow
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material.icons.filled.Public
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Chat
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.ArrowDropDown
import androidx.compose.material.icons.filled.Build
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.Image
import androidx.compose.material.icons.filled.InsertDriveFile
import androidx.compose.material.icons.filled.PhotoCamera
import androidx.compose.material.icons.filled.Menu
import androidx.compose.material.icons.filled.Psychology
import androidx.compose.material.icons.filled.Send
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material.icons.filled.Tune
import androidx.compose.material3.*
import androidx.compose.foundation.Image
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.AlertDialog
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.graphics.Color
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import android.widget.Toast
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.navigation.NavController
import com.halilibo.richtext.markdown.Markdown
import com.halilibo.richtext.ui.material3.RichText
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.contentOrNull

/** Display names for goose's four approval modes.
 *
 *  The server sends snake_case ids (auto, approve, smart_approve, chat) and, for some builds, no
 *  label at all -- de-snaking gives "smart approve", which reads like a verb. One word each,
 *  naming the AMOUNT of autonomy rather than describing the prompt you get: they sit in a row
 *  in a picker and read as a scale, which "Ask when risky" next to "Chat only" did not. The
 *  one-line blurb underneath still says what actually happens, so nothing is lost by the
 *  shorter label. Unknown ids fall through de-snaked rather than being hidden, so a new
 *  upstream mode still appears and still works.
 */
internal fun prettyMode(value: String?): String = when (value) {
    "auto" -> "Auto"
    "approve" -> "Manual"
    "smart_approve" -> "Smart"
    "chat" -> "None"
    null, "" -> "Mode"
    else -> value.replace('_', ' ').replaceFirstChar { it.uppercase() }
}

/** One-line explanation under each mode in the picker, from GooseMode's own descriptions. */
internal fun modeBlurb(value: String): String = when (value) {
    "auto" -> "Runs tools without asking"
    "approve" -> "Asks before every tool call"
    "smart_approve" -> "Asks only for sensitive tool calls"
    "chat" -> "No tools at all — plain chat"
    else -> ""
}

/** The config option ids this app exposes in every session's Settings sheet. Single shared
 *  source (also used by ConnectionManager) so the saved-option and picker surfaces never
 *  drift apart. */

// ---- Landing (home on open) -------------------------------------------------

/** Home page shown on a fresh app open: connection status, and one-tap entry to
 *  the Assistant, a brand-new chat, and the most recent sessions. Drawn here so a
 *  cold start surfaces choices instead of dropping straight into the last chat. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LandingScreen(
    cm: ConnectionManager,
    onOpenDrawer: () -> Unit,
    onNewChat: () -> Unit,
    onOpenAssistant: () -> Unit,
    onOpenSession: (String) -> Unit,
) {
    val recent = cm.sessions.value
        .filter { !it.sessionId.startsWith("roam:") }
        .sortedByDescending { it.updatedAt }
        .take(5)

    Scaffold(topBar = {
        TopAppBar(
            title = {
                Column {
                    Text(stringResource(R.string.app_name), style = MaterialTheme.typography.titleLarge)
                    Text(
                        when {
                            cm.online.value -> "Connected"
                            cm.status.value.contains("connect", true) -> cm.status.value
                            else -> cm.status.value.ifBlank { "not connected" }
                        },
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.outline)
                }
            },
            navigationIcon = {
                IconButton(onClick = onOpenDrawer) {
                    Icon(Icons.Filled.Menu, contentDescription = "menu")
                }
            },
        )
    }) { pad ->
        LazyColumn(Modifier.padding(pad).padding(horizontal = 16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            item {
                Text(stringResource(R.string.what_would_you_like_to_do), style = MaterialTheme.typography.titleMedium,
                    modifier = Modifier.padding(top = 8.dp))
            }
            item {
                Button(onClick = onNewChat, modifier = Modifier.fillMaxWidth().height(52.dp)) {
                    Icon(Icons.Filled.Add, contentDescription = null)
                    Spacer(Modifier.width(8.dp))
                    Text(stringResource(R.string.new_chat))
                }
            }
            if (cm.assistantEnabled.value) {
                item {
                    OutlinedButton(onClick = onOpenAssistant, modifier = Modifier.fillMaxWidth().height(48.dp)) {
                        Icon(Icons.Filled.Psychology, contentDescription = null)
                        Spacer(Modifier.width(8.dp))
                        Text("Assistant")
                    }
                }
            }
            if (recent.isNotEmpty()) {
                item {
                    Text(stringResource(R.string.recent), style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.primary,
                        modifier = Modifier.padding(top = 10.dp))
                }
                items(recent, key = { it.sessionId }) { s ->
                    Surface(
                        onClick = { onOpenSession(s.sessionId) },
                        shape = RoundedCornerShape(12.dp),
                        color = MaterialTheme.colorScheme.surfaceVariant,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Column(Modifier.padding(horizontal = 14.dp, vertical = 12.dp)) {
                            Text(s.title.ifBlank { "Untitled chat" },
                                style = MaterialTheme.typography.bodyLarge, maxLines = 1,
                                overflow = TextOverflow.Ellipsis)
                            Text(relativeTime(s.updatedAt),
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.outline)
                        }
                    }
                }
            } else {
                item {
                    Text(stringResource(R.string.no_chats_yet),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.outline)
                }
            }
        }
    }
}

// ---- Connect (onboarding) ---------------------------------------------------

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConnectScreen(cm: ConnectionManager, onConnected: () -> Unit) {
    var host by remember { mutableStateOf(cm.store.host) }
    var port by remember { mutableStateOf(cm.store.port) }
    var key by remember { mutableStateOf("") }
    var workdir by remember { mutableStateOf(cm.store.workingDir) }
    var showKey by remember { mutableStateOf(false) }
    var verifyTls by remember { mutableStateOf(cm.store.verifyTls) }
    var caCert by remember { mutableStateOf(cm.store.caCertPem) }
    Scaffold(topBar = { TopAppBar(title = { Text(stringResource(R.string.connect_to_grouse)) }) }) { pad ->
        Column(
            Modifier.padding(pad).padding(24.dp).fillMaxWidth().verticalScroll(rememberScrollState()),
            horizontalAlignment = Alignment.CenterHorizontally
        ) {
            Spacer(Modifier.height(16.dp))
            Icon(Icons.Filled.Psychology, contentDescription = null,
                modifier = Modifier.size(64.dp), tint = MaterialTheme.colorScheme.primary)
            Spacer(Modifier.height(10.dp))
            Text(stringResource(R.string.welcome_to_grouse), style = MaterialTheme.typography.headlineSmall)
            Text(stringResource(R.string.connect_blurb),
                style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.outline,
                textAlign = TextAlign.Center, modifier = Modifier.padding(top = 4.dp))
            Spacer(Modifier.height(28.dp))
            OutlinedTextField(host, { host = it }, label = { Text(stringResource(R.string.host)) },
                supportingText = { Text(stringResource(R.string.host_hint)) },
                singleLine = true, modifier = Modifier.fillMaxWidth())
            OutlinedTextField(port, { port = it }, label = { Text(stringResource(R.string.port)) }, singleLine = true,
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp))
            OutlinedTextField(key, { key = it }, label = { Text(stringResource(R.string.secret_key)) }, singleLine = true,
                visualTransformation = if (showKey) VisualTransformation.None else PasswordVisualTransformation(),
                trailingIcon = {
                    TextButton(onClick = { showKey = !showKey }) { Text(if (showKey) "Hide" else "Show") }
                },
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp))
            // goose refuses a session/new whose cwd is not absolute, and has no per-user default
            // to fall back on, so this is asked for rather than guessed. It is the directory the
            // agent's tools run in -- AGENTS.md is read from here up to the git root.
            OutlinedTextField(workdir, { workdir = it }, label = { Text(stringResource(R.string.working_directory)) },
                supportingText = { Text(stringResource(R.string.workdir_hint)) },
                singleLine = true, modifier = Modifier.fillMaxWidth().padding(top = 8.dp))
            Row(verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp)) {
                Checkbox(checked = verifyTls, onCheckedChange = { verifyTls = it })
                Column {
                    Text(stringResource(R.string.verify_server_certificate), style = MaterialTheme.typography.bodyMedium)
                    Text(stringResource(R.string.cert_note),
                        style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.outline)
                }
            }
            if (verifyTls) {
                OutlinedTextField(caCert, { caCert = it },
                    label = { Text(stringResource(R.string.ca_certificate)) },
                    supportingText = { Text(stringResource(R.string.ca_certificate_hint)) },
                    singleLine = true, modifier = Modifier.fillMaxWidth().padding(top = 8.dp))
            }
            Spacer(Modifier.height(20.dp))
            Button(
                onClick = {
                    cm.store.verifyTls = verifyTls
                    cm.store.caCertPem = caCert
                    cm.connect(host.trim(), port.trim(), key.trim(), workdir.trim()); onConnected()
                },
                enabled = key.isNotBlank() && host.isNotBlank() && workdir.trim().startsWith("/"),
                modifier = Modifier.fillMaxWidth()
            ) { Text(stringResource(R.string.connect)) }
        }
    }
}

// ---- Chat -------------------------------------------------------------------
