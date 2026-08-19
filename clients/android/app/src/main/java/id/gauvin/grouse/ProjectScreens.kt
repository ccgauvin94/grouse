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
    var info by remember { mutableStateOf<String?>(null) }
    var infoBusy by remember { mutableStateOf(false) }
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
    val projectId = cm.projects.value.firstOrNull { it.name.equals(project, true) }?.id
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
                Text(stringResource(R.string.goosehints_memory), style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.padding(start = 6.dp, top = 18.dp, bottom = 4.dp))
            }
            item {
                Card(Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
                    Column(Modifier.fillMaxWidth().padding(14.dp)) {
                        when {
                            infoBusy -> Row(verticalAlignment = Alignment.CenterVertically) {
                                CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                                Spacer(Modifier.width(10.dp))
                                Text(stringResource(R.string.asking_fast_model),
                                    style = MaterialTheme.typography.bodySmall)
                            }
                            info != null -> {
                                Text(info!!, style = MaterialTheme.typography.bodySmall)
                                Spacer(Modifier.height(6.dp))
                                TextButton(onClick = {
                                    infoBusy = true
                                    cm.fetchProjectInfo(project) { err, text ->
                                        infoBusy = false; info = err ?: text
                                    }
                                }) { Text(stringResource(R.string.reload)) }
                            }
                            else -> {
                                Text(stringResource(R.string.project_hints_info),
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.outline)
                                Spacer(Modifier.height(6.dp))
                                TextButton(onClick = {
                                    infoBusy = true
                                    cm.fetchProjectInfo(project) { err, text ->
                                        infoBusy = false; info = err ?: text
                                    }
                                }) { Text(stringResource(R.string.load)) }
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

// ---- Assistant settings ------------------------------------------------------

/** The Assistant control panel. Server-side behavior (schedule, models, prompts, kill
 *  switches) lives in goose's config.yaml, written over ACP and read by deliver.sh per
 *  timer firing — changes apply at the next firing, no restarts. Tests touch a /state drop
 *  file via a direct tool call; a host systemd path unit runs the real pipeline with every
 *  gate bypassed. Thread tools edit the live Assistant session's extension set, which the
 *  daily rotation copies forward. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AssistantSettingsScreen(cm: ConnectionManager, nav: NavController) {
    val ctx = LocalContext.current
    LaunchedEffect(cm.online.value) {
        if (cm.online.value) {
            cm.refreshSchedules()
        }
    }

    Scaffold(topBar = {
        TopAppBar(
            title = { Text("Assistant") },
            navigationIcon = {
                IconButton(onClick = { nav.popBackStack() }) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "back")
                }
            }
        )
    }) { pad ->
        Column(Modifier.padding(pad).padding(horizontal = 16.dp).fillMaxSize()
            .verticalScroll(rememberScrollState())) {

            // The Morning digest / Updates sections that stood here are GONE (2026-08-01), and
            // so is the "Assistant features" master switch. All three wrote config keys --
            // MORNING_*, BRIEFING_*, ASSISTANT_ENABLED -- that only deliver.sh ever read, and
            // deliver.sh no longer runs the jobs. They rendered current-looking values and
            // changed nothing. Their "Run test now" buttons were worse than useless: they still
            // tripped the systemd path unit that runs deliver.sh, so a test would have delivered
            // a SECOND briefing by the retired route.
            //
            // Everything they claimed to control is real on the Schedules screen: enabled is
            // pause, time is cron, model/provider/prompt are the recipe, and "run test now" is
            // Run now on the actual job.
            SettingsSection("Scheduled jobs") {
                SettingsNavRow("Recipes",
                    cm.schedules.value.let { j ->
                        if (j.isEmpty()) "none scheduled"
                        else j.count { !it.paused }.toString() + " of " + j.size + " active"
                    }) { nav.navigate("recipes") }
                SettingCaption("The morning digest, the hourly briefing and the nightly " +
                    "compaction. Pausing one is what \"off\" used to mean; what each one runs " +
                    "is its recipe, under Recipes in the menu.")
            }

                SettingsSection("Thread") {
                    // The "Assistant actions" policy (auto-approve / read-only / ask) is GONE.
                    // It was a client-side permission system layered on top of goose's own mode
                    // -- so the mode picker in the thread appeared to govern tool approval and
                    // did not, and a thread left on "auto-approve" ran shell without asking
                    // however its mode read. Set the mode in the thread, like any other chat.
                    var confirmReset by remember { mutableStateOf(false) }
                    SettingsNavRow("Reset assistant thread",
                        "Start fresh now. The old thread is kept, renamed aside. (Happens " +
                        "automatically every morning.)") {
                        confirmReset = true
                    }
                    if (confirmReset) AlertDialog(
                        onDismissRequest = { confirmReset = false },
                        title = { Text(stringResource(R.string.reset_assistant_thread)) },
                        text = { Text(stringResource(R.string.reset_note)) },
                        confirmButton = { TextButton(onClick = {
                            confirmReset = false; cm.resetAssistant(); nav.popBackStack()
                        }) { Text(stringResource(R.string.reset)) } },
                        dismissButton = { TextButton(onClick = { confirmReset = false }) { Text(stringResource(R.string.cancel)) } },
                    )
                }

                // "Thread tools" stood here: a second tool editor that existed only for this
                // one conversation. The in-chat tools sheet already does exactly that for
                // whichever chat you are in, this thread included -- so the settings copy was a
                // parallel implementation of the same operation, and the one that was wrong.
            }
            Spacer(Modifier.height(28.dp))
        }
    }

// ---- Settings ---------------------------------------------------------------

// ---- Reusable settings building blocks --------------------------------------
