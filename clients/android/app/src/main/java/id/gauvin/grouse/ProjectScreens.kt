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


/** A project's home: its chats, its .goosehints and local memory (fetched on demand -- there
 *  is no file read over ACP, so a throwaway fast-model session cats them and echoes the output),
 *  and deletion. Delete archives the project's chats and rmdir's the server directory ONLY if
 *  empty -- a project with files keeps them and merely leaves the list. */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalFoundationApi::class)
@Composable
fun ProjectScreen(cm: ConnectionManager, nav: NavController, project: String) {
    LaunchedEffect(Unit) { cm.refreshSidebar() }
    var actionsFor by remember { mutableStateOf<SessionInfo?>(null) }
    var confirmDelete by remember { mutableStateOf(false) }
    var deleteBusy by remember { mutableStateOf(false) }
    var deleteNote by remember { mutableStateOf<String?>(null) }
    // Project memory: the global store's topic named after the project (the
    // seeded instructions teach the model to keep durable notes there).
    var mem by remember { mutableStateOf<String?>(null) }
    var memDraft by remember { mutableStateOf<String?>(null) }
    var memBusy by remember { mutableStateOf(false) }
    var memNote by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(project) {
        memBusy = true
        cm.memoryRead(project) { err, text ->
            memBusy = false
            mem = if (err != null) "($err)" else text
        }
    }
    // Instructions editor: seeded from the project's content and RESEEDED when
    // the list refreshes (the save's re-list) — remember keyed on content, the
    // recipe-instructions idiom. Note clears on the next edit.
    var instr by rememberSaveable { mutableStateOf<String?>(null) }
    var instrBusy by remember { mutableStateOf(false) }
    var instrNote by remember { mutableStateOf<String?>(null) }
    val savedMsg = stringResource(R.string.saved)
    fun goToChat() = nav.navigate("chat") { launchSingleTop = true; popUpTo("chat") { inclusive = true } }

    actionsFor?.let { s -> SessionActionsDialog(cm, s) { actionsFor = null } }
    if (confirmDelete) AlertDialog(
        onDismissRequest = { if (!deleteBusy) confirmDelete = false },
        title = { Text(stringResource(R.string.delete_project_question)) },
        text = { Column {
            Text("Archives this project's chats and removes /workspace/$project from the " +
                "server — but ONLY if the directory is empty. A project with files keeps " +
                "them and just leaves this list.")
            if (deleteBusy) {
                Spacer(Modifier.height(10.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                    Spacer(Modifier.width(10.dp)); Text(stringResource(R.string.working))
                }
            }
        } },
        confirmButton = {
            TextButton(enabled = !deleteBusy, onClick = {
                deleteBusy = true
                cm.deleteProject(project) { note ->
                    deleteBusy = false; confirmDelete = false; deleteNote = note
                }
            }) { Text(stringResource(R.string.delete)) }
        },
        dismissButton = { TextButton(onClick = { confirmDelete = false }, enabled = !deleteBusy) { Text(stringResource(R.string.cancel)) } },
    )
    deleteNote?.let { note ->
        AlertDialog(
            onDismissRequest = { deleteNote = null; nav.popBackStack() },
            title = { Text(stringResource(R.string.project_deleted)) },
            text = { Text(note) },
            confirmButton = { TextButton(onClick = { deleteNote = null; nav.popBackStack() }) { Text(stringResource(R.string.ok)) } },
        )
    }

    // Membership is projectId now. The cwd test is kept as a FALLBACK for the directory-era
    // projects (Cooking, Hacking, Inbox) whose sessions were filed by working directory and
    // never migrated -- dropping it would empty those screens.
    val proj = cm.projects.value.firstOrNull { it.name.equals(project, true) }
    val projectId = proj?.id
    val instrText = instr ?: proj?.content.orEmpty()
    val chats = cm.sessions.value.filter { s ->
        ConnectionManager.sessionKind(s) != SessionKind.ASSISTANT &&
            projectId != null && s.projectId == projectId
    }
    Scaffold(topBar = {
        TopAppBar(
            title = { Text(project, maxLines = 1, overflow = TextOverflow.Ellipsis) },
            navigationIcon = {
                IconButton(onClick = { nav.popBackStack() }) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "back")
                }
            }
        )
    }) { pad ->
        LazyColumn(Modifier.padding(pad).padding(horizontal = 12.dp).fillMaxSize()) {
            item {
                Text(stringResource(R.string.instructions), style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.padding(start = 6.dp, top = 10.dp, bottom = 4.dp))
            }
            item {
                Card(Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
                    Column(Modifier.fillMaxWidth().padding(14.dp)) {
                        OutlinedTextField(
                            value = instrText,
                            onValueChange = { instr = it; instrNote = null },
                            modifier = Modifier.fillMaxWidth().heightIn(min = 120.dp),
                            label = { Text(stringResource(R.string.project_instructions_hint)) },
                            maxLines = 16,
                        )
                        Spacer(Modifier.height(6.dp))
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            TextButton(enabled = instrBusy || (proj != null && proj.path.isNotEmpty() &&
                                        instrText != proj.content),
                                onClick = {
                                    if (proj == null) return@TextButton
                                    instrBusy = true
                                    cm.saveProjectInstructions(proj, instrText) { err ->
                                        instrBusy = false
                                        // Success: the re-list reseeds the editor (instr clears
                                        // below); the note says so. Failure keeps the draft.
                                        if (err == null) instr = null
                                        instrNote = err ?: savedMsg
                                    }
                                }) { Text(stringResource(R.string.save)) }
                            if (instrBusy) {
                                Spacer(Modifier.width(10.dp))
                                CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                            }
                            instrNote?.let {
                                Spacer(Modifier.width(10.dp))
                                Text(it, style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.outline)
                            }
                        }
                    }
                }
            }
            item {
                Text(stringResource(R.string.chats), style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.padding(start = 6.dp, top = 10.dp, bottom = 4.dp))
            }
            item {
                Card(Modifier.fillMaxWidth().padding(vertical = 4.dp)
                    // Filing is by project id. A project is a TAG, not a directory -- creating
                    // a session in a path named after it produced "invalid directory path" for
                    // every project made since projects went virtual.
                    .clickable {
                        projectId?.let { cm.newChatInProject(it); goToChat() }
                    }) {
                    Row(Modifier.padding(14.dp), verticalAlignment = Alignment.CenterVertically) {
                        Icon(Icons.Filled.Add, contentDescription = null)
                        Spacer(Modifier.width(10.dp))
                        Text(stringResource(R.string.new_chat), style = MaterialTheme.typography.titleMedium)
                    }
                }
            }
            items(chats, key = { it.sessionId }) { s ->
                Card(Modifier.fillMaxWidth().padding(vertical = 4.dp)
                    .combinedClickable(onClick = { cm.openSession(s.sessionId); goToChat() },
                        onLongClick = { actionsFor = s })) {
                    Row(Modifier.padding(14.dp), verticalAlignment = Alignment.CenterVertically) {
                        val peer = ConnectionManager.roamPeer(s.sessionId)
                        Column(Modifier.weight(1f)) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                if (peer != null) {
                                    Icon(Icons.Filled.Public, contentDescription = "remote — on $peer",
                                        modifier = Modifier.size(15.dp), tint = MaterialTheme.colorScheme.outline)
                                    Spacer(Modifier.width(6.dp))
                                }
                                Text(s.title.ifBlank { "Untitled chat" }, style = MaterialTheme.typography.titleMedium,
                                    maxLines = 1, overflow = TextOverflow.Ellipsis)
                            }
                            Spacer(Modifier.height(2.dp))
                            Text(listOf(peer?.let { "on $it" } ?: "", "${s.messageCount} msgs", s.model, relativeTime(s.updatedAt))
                                .filter { it.isNotBlank() }.joinToString("  ·  "),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.outline, maxLines = 1,
                                overflow = TextOverflow.Ellipsis)
                        }
                        Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, contentDescription = null,
                            tint = MaterialTheme.colorScheme.outline)
                    }
                }
            }
            item {
                Text(stringResource(R.string.project_memory), style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.padding(start = 6.dp, top = 18.dp, bottom = 4.dp))
            }
            item {
                Card(Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
                    Column(Modifier.fillMaxWidth().padding(14.dp)) {
                        val shown = memDraft ?: mem
                        when {
                            shown == null && memBusy -> Row(verticalAlignment = Alignment.CenterVertically) {
                                CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                                Spacer(Modifier.width(10.dp))
                                Text(stringResource(R.string.loading), style = MaterialTheme.typography.bodySmall)
                            }
                            shown == null -> Text(stringResource(R.string.connect_open_chat_first),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.outline)
                            else -> {
                                OutlinedTextField(
                                    value = shown,
                                    onValueChange = { memDraft = it; memNote = null },
                                    modifier = Modifier.fillMaxWidth().heightIn(min = 120.dp),
                                    label = { Text(stringResource(R.string.project_memory_hint, project)) },
                                    maxLines = 16,
                                )
                                Spacer(Modifier.height(6.dp))
                                Row(verticalAlignment = Alignment.CenterVertically) {
                                    TextButton(
                                        enabled = !memBusy && memDraft != null && mem != null &&
                                            memDraft != mem,
                                        onClick = {
                                            val body = memDraft ?: return@TextButton
                                            memBusy = true
                                            cm.memoryWrite(project, body) { err ->
                                                memBusy = false
                                                if (err == null) { mem = body; memDraft = null }
                                                memNote = err ?: savedMsg
                                            }
                                        }) { Text(stringResource(R.string.save)) }
                                    TextButton(
                                        enabled = !memBusy && memDraft == null,
                                        onClick = {
                                            memBusy = true
                                            cm.memoryRead(project) { err, text ->
                                                memBusy = false
                                                mem = if (err != null) "($err)" else text
                                            }
                                        }) { Text(stringResource(R.string.reload)) }
                                    if (memBusy) {
                                        Spacer(Modifier.width(10.dp))
                                        CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                                    }
                                    memNote?.let {
                                        Spacer(Modifier.width(10.dp))
                                        Text(it, style = MaterialTheme.typography.bodySmall,
                                            color = MaterialTheme.colorScheme.outline)
                                    }
                                }
                            }
                        }
                    }
                }
            }
            item {
                TextButton(onClick = { confirmDelete = true },
                    modifier = Modifier.padding(top = 18.dp, bottom = 24.dp)) {
                    Text(stringResource(R.string.delete_project), color = MaterialTheme.colorScheme.error)
                }
            }
        }
    }
}


// ---- Settings ---------------------------------------------------------------

// ---- Global memory store (the server's builtin Memory extension files) -----

/** Topic list: one file per topic; the first line is the keyword list. The
 *  store is global and project-blind — a "project memory" is simply a topic
 *  named after the project (the seeded project instructions say so). Reached
 *  from Settings; the skills screen is the structural twin. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MemoryScreen(cm: ConnectionManager, nav: NavController) {
    var topics by remember { mutableStateOf<List<Pair<String, String>>>(emptyList()) }
    var busy by remember { mutableStateOf(false) }
    var note by remember { mutableStateOf<String?>(null) }
    var draftName by remember { mutableStateOf("") }
    fun load() {
        busy = true
        cm.memoryList { err, rows -> busy = false; topics = rows; note = err }
    }
    LaunchedEffect(Unit) { load() }
    Scaffold(topBar = {
        TopAppBar(
            title = { Text(stringResource(R.string.memory_topics)) },
            navigationIcon = {
                IconButton(onClick = { nav.popBackStack() }) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "back")
                }
            },
            actions = {
                IconButton(onClick = { load() }) {
                    Icon(Icons.Filled.Refresh, contentDescription = "reload")
                }
            }
        )
    }) { pad ->
        LazyColumn(Modifier.padding(pad).padding(horizontal = 12.dp).fillMaxSize()) {
            item {
                Text(stringResource(R.string.memory_topics_hint),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.outline,
                    modifier = Modifier.padding(start = 6.dp, bottom = 6.dp))
            }
            note?.let { n ->
                item {
                    Text(n, style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.padding(start = 6.dp, bottom = 6.dp))
                }
            }
            item {
                Row(verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.padding(vertical = 4.dp)) {
                    OutlinedTextField(draftName, { draftName = it }, singleLine = true,
                        label = { Text(stringResource(R.string.new_topic)) },
                        modifier = Modifier.weight(1f))
                    Spacer(Modifier.width(8.dp))
                    TextButton(enabled = draftName.isNotBlank() && cm.memoryReady(),
                        onClick = {
                            val t = draftName.trim()
                            draftName = ""
                            busy = true
                            cm.memoryWrite(t, "# $t\n") { err ->
                                busy = false
                                note = err
                                if (err == null) nav.navigate("memory/" + Uri.encode(t))
                                else load()
                            }
                        }) { Text(stringResource(R.string.add)) }
                }
            }
            items(topics, key = { it.first }) { row ->
                Card(Modifier.fillMaxWidth().padding(vertical = 4.dp)
                    .clickable { nav.navigate("memory/" + Uri.encode(row.first)) }) {
                    Row(Modifier.padding(14.dp), verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f)) {
                            Text(row.first, style = MaterialTheme.typography.titleMedium,
                                maxLines = 1, overflow = TextOverflow.Ellipsis)
                            if (row.second.isNotBlank())
                                Text(row.second, style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.outline, maxLines = 1,
                                    overflow = TextOverflow.Ellipsis)
                        }
                        Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, contentDescription = null,
                            tint = MaterialTheme.colorScheme.outline)
                    }
                }
            }
            if (busy && topics.isEmpty()) item {
                Row(Modifier.padding(14.dp), verticalAlignment = Alignment.CenterVertically) {
                    CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                    Spacer(Modifier.width(10.dp))
                    Text(stringResource(R.string.loading), style = MaterialTheme.typography.bodySmall)
                }
            }
        }
    }
}

/** One memory topic: the whole file, editable (the store has no versioning —
 *  saving replaces the file, exactly like the skills editor). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MemoryTopicScreen(cm: ConnectionManager, nav: NavController, topic: String) {
    var text by remember { mutableStateOf<String?>(null) }      // current editor content
    var saved by remember { mutableStateOf<String?>(null) }     // last known server copy
    var busy by remember { mutableStateOf(false) }
    var note by remember { mutableStateOf<String?>(null) }
    val savedMsg = stringResource(R.string.saved)
    fun load() {
        busy = true
        cm.memoryRead(topic) { err, t ->
            busy = false
            val v = if (err != null) "" else t
            text = v; saved = v
            note = err
        }
    }
    LaunchedEffect(topic) { load() }
    Scaffold(topBar = {
        TopAppBar(
            title = { Text(topic, maxLines = 1, overflow = TextOverflow.Ellipsis) },
            navigationIcon = {
                IconButton(onClick = { nav.popBackStack() }) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "back")
                }
            }
        )
    }) { pad ->
        Column(Modifier.padding(pad).padding(horizontal = 12.dp, vertical = 8.dp).fillMaxSize()) {
            Text(stringResource(R.string.memory_topics_hint),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.outline)
            Spacer(Modifier.height(8.dp))
            OutlinedTextField(
                value = text ?: "",
                onValueChange = { text = it; note = null },
                modifier = Modifier.fillMaxWidth().weight(1f),
                label = { Text("$topic.txt") },
            )
            Spacer(Modifier.height(8.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                TextButton(
                    enabled = !busy && text != null && saved != null && text != saved,
                    onClick = {
                        val body = text ?: return@TextButton
                        busy = true
                        cm.memoryWrite(topic, body) { err ->
                            busy = false
                            if (err == null) saved = body
                            note = err ?: savedMsg
                        }
                    }) { Text(stringResource(R.string.save)) }
                TextButton(enabled = !busy, onClick = { load() }) {
                    Text(stringResource(R.string.reload))
                }
                if (busy) {
                    Spacer(Modifier.width(10.dp))
                    CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                }
                note?.let {
                    Spacer(Modifier.width(10.dp))
                    Text(it, style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.outline)
                }
            }
        }
    }
}

// ---- Reusable settings building blocks --------------------------------------
