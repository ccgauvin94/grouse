// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import id.gauvin.grouse.ui.theme.statusColors
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
import androidx.compose.material.icons.filled.Search
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


/** The drawer's chats area: projects (collapsible groups of sessions, grouped by cwd —
 *  grouped by goose's project id) on top,
 *  free chats below. The project list is the union of names seen in session cwds and the
 *  typed-recents store, so a just-created project with no sessions yet still shows.
 *  Tap opens a session; long-press offers Rename / Archive. Lives INSIDE ModalDrawerSheet —
 *  everything here is the app's main menu, not a separate screen. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun DrawerChats(cm: ConnectionManager, onOpen: () -> Unit, onOpenProject: (String) -> Unit,
    query: String = "", onClearQuery: () -> Unit = {}) {
    var showNewProject by remember { mutableStateOf(false) }
    var actionsFor by remember { mutableStateOf<SessionInfo?>(null) }
    var expanded by rememberSaveable { mutableStateOf(listOf<String>()) }
    // Entire PROJECTS section collapsed/expanded (master toggle), independent of
    // per-project `expanded`.
    var projectsCollapsed by rememberSaveable { mutableStateOf(false) }

    actionsFor?.let { s -> SessionActionsDialog(cm, s) { actionsFor = null } }
    if (showNewProject) NewProjectDialog(
        cm = cm,
        // Start the first chat FILED under the new project. Not a session in
        // <home>/Projects/<name>: createProject makes a virtual project and no directory,
        // so that path does not exist and session/new refuses it with "invalid directory path".
        // The id is the name the dialog already constrains to the server's slug alphabet.
        onCreated = { name ->
            showNewProject = false
            cm.newChatInProject(name.trim().lowercase())
            onOpen()
        },
        onDismiss = { showNewProject = false },
    )

    // Projects come from the SERVER now (sources/list type=project), and a session belongs to one
    // by project_id. This used to derive both from cwd -- names were the last path segment, which
    // is why they rendered capitalised (the DIRECTORY was capitalised), and the list was padded
    // with locally-remembered names from SharedPreferences, which is why deleted projects lingered
    // as ghosts. Neither source could be renamed, shared between clients, or trusted.
    // The Assistant has its own pinned row at the top of the drawer, so it must not ALSO appear
    // in the grouped lists. It used to fall out naturally: grouping keyed on cwd and the
    // assistant lived at /state, which was not a project. Now it carries a project_id like any
    // other session (the 2026-08-01 migration filed it under "inbox"), so it needs excluding
    // explicitly or it renders twice -- once pinned, once as an ordinary chat.
    val all = cm.sessions.value.filter { ConnectionManager.sessionKind(it) != SessionKind.ASSISTANT }
    // All sessions here are LOCAL — remote (roam peer) sessions moved to the Roam tab's
    // per-peer groups (RoamTabBody). Grouped naively a federated SessionInfo carries the
    // REMOTE server's project_id, so it must never mix into the local project/free split.
    // Filter them out of the drawer explicitly: onCoreSessions keeps roam entries in the
    // shared list for the Roam tab, so without this the Main drawer shows peer chats.
    val localChats = all.filterNot { it.sessionId.startsWith("roam:") }
    val searching = query.isNotBlank()
    // Project names are part of the haystack, not just a label: "grouse" should find the chats
    // filed under the grouse project without knowing their titles. Built once per recomposition
    // from the same list the project cards render from, so the two can't disagree.
    val projectName = cm.projects.value.associate { it.id to it.name }
    // `group` is the project NAME (an id would match nothing but its own digits).
    fun matches(s: SessionInfo) =
        ConnectionManager.chatMatches(query, s, s.projectId?.let { projectName[it] })
    val visibleChats = if (searching) localChats.filter(::matches) else localChats
    val activeChats = visibleChats.filterNot { it.archived }
    val byProjectId = activeChats.groupBy { it.projectId }
    val freeChats = byProjectId[null].orEmpty()
    val archivedChats = visibleChats.filter { it.archived }
    // A query hides a project card entirely unless the name itself matches or one of its chats
    // does. Left standing empty, the card reads "this project has no chats" — a false negative
    // that pushes you to conclude the chat was deleted.
    val projects = cm.projects.value.filter { proj ->
        !searching ||
            ConnectionManager.queryMatches(query, listOf(proj.name)) ||
            byProjectId[proj.id]?.isNotEmpty() == true
    }
    // Nothing left anywhere: the add-buttons and section headers would be the only rows, which
    // is worse than saying "no matches" and offering the way out.
    val nothingFound = searching && activeChats.isEmpty() && freeChats.isEmpty() &&
        archivedChats.isEmpty() && projects.isEmpty()

    @Composable
    fun sessionRow(s: SessionInfo, indent: Boolean) {
        Row(Modifier.fillMaxWidth()
            .combinedClickable(onClick = { cm.openSession(s.sessionId); onOpen() },
                onLongClick = { actionsFor = s })
            .padding(start = if (indent) 34.dp else 10.dp, end = 8.dp)
            .padding(vertical = 9.dp),
            verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(s.title.ifBlank { "Untitled chat" }, style = MaterialTheme.typography.bodyLarge,
                        maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
                // Last-message preview beats "N msgs" as a scent for which chat is which; the
                // count/time line stays as the fallback when the server didn't send one.
                if (s.snippet.isNotBlank())
                    Text(s.snippet, style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.outline,
                        maxLines = 1, overflow = TextOverflow.Ellipsis)
                else Text(listOf("${s.messageCount} msgs", relativeTime(s.updatedAt))
                    .filter { it.isNotBlank() }.joinToString("  ·  "),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.outline, maxLines = 1)
            }
            // Green dot: backgrounded content arrived for a chat that's not on
            // screen (serve parity with the roam staging indicator).
            if (s.hasNew) {
                Spacer(Modifier.width(10.dp))
                Box(Modifier.size(10.dp).background(MaterialTheme.statusColors.online, CircleShape))
            }
        }
    }

    @Composable
    fun addRow(label: String, indent: Boolean, onClick: () -> Unit) {
        Row(Modifier.fillMaxWidth().clickable(onClick = onClick)
            .padding(start = if (indent) 34.dp else 10.dp)
            .padding(vertical = 9.dp),
            verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Filled.Add, contentDescription = null,
                modifier = Modifier.size(18.dp), tint = MaterialTheme.colorScheme.outline)
            Spacer(Modifier.width(8.dp))
            Text(label, style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.outline)
        }
    }

    LazyColumn(Modifier.fillMaxWidth().padding(horizontal = 14.dp)) {
      // Section chrome disappears under a query; only matches and the empty state remain.
      if (!nothingFound) {
        item {
          Row(Modifier.fillMaxWidth().clickable { projectsCollapsed = !projectsCollapsed }
              .padding(start = 10.dp, top = 10.dp, bottom = 2.dp),
              verticalAlignment = Alignment.CenterVertically) {
              Text(stringResource(R.string.projects), style = MaterialTheme.typography.labelMedium,
                  color = MaterialTheme.colorScheme.primary)
              Spacer(Modifier.weight(1f))
              Icon(if (projectsCollapsed) Icons.Filled.KeyboardArrowUp else Icons.Filled.KeyboardArrowDown,
                  contentDescription = if (projectsCollapsed) "expand projects" else "collapse projects",
                  modifier = Modifier.size(18.dp), tint = MaterialTheme.colorScheme.outline)
          }
        }
      }
        if (!projectsCollapsed || searching) {
            projects.forEach { proj ->
                val p = proj.name
                val inProject = byProjectId[proj.id].orEmpty()
                // While searching a card is open regardless of the tapped state: a hit you have
                // to tap to reveal isn't a search result, and the collapsed card is exactly as
                // misleading as an empty one.
                val open = proj.id in expanded || searching
                item(key = "project:" + proj.id) {
                    // Project card: Folder icon + name + session count + new-chat button.
                    // Click ANYWHERE toggles the inline session list; LONG-PRESS opens
                    // the project page (options: rename/description/instructions/delete).
                    // No chevron — expansion is driven by tapping the card itself.
                    Card(
                        modifier = Modifier.fillMaxWidth()
                            .padding(start = 10.dp, end = 10.dp, bottom = 6.dp),
                        shape = RoundedCornerShape(14.dp),
                        colors = CardDefaults.cardColors(
                            containerColor = MaterialTheme.colorScheme.surfaceVariant
                                .copy(alpha = 0.5f))
                    ) {
                        Column {
                            Row(Modifier.fillMaxWidth()
                                .clip(RoundedCornerShape(14.dp))
                                .combinedClickable(
                                    onClick = { expanded = if (open) expanded - proj.id else expanded + proj.id },
                                    onLongClick = { onOpenProject(p) })
                                .padding(start = 14.dp, end = 4.dp).padding(vertical = 8.dp),
                                verticalAlignment = Alignment.CenterVertically) {
                                Icon(Icons.Filled.Folder, contentDescription = null,
                                    modifier = Modifier.size(20.dp), tint = MaterialTheme.colorScheme.primary)
                                Spacer(Modifier.width(10.dp))
                                Text(p, style = MaterialTheme.typography.bodyLarge, modifier = Modifier.weight(1f),
                                    maxLines = 1, overflow = TextOverflow.Ellipsis)
                                if (inProject.isNotEmpty()) Text("${inProject.size}",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.outline)
                                // New chat filed under this project, like roam's Add on the card.
                                Box(Modifier.size(24.dp).clickable {
                                    cm.newChatInProject(proj.id); onOpen()
                                }, contentAlignment = Alignment.Center) {
                                    Icon(Icons.Filled.Add,
                                        contentDescription = "new chat in $p",
                                        tint = MaterialTheme.colorScheme.primary)
                                }
                            }
                            // Sessions live INSIDE the expanded card, like the roam
                            // endpoint cards. Divider under the header, then each chat.
                            if (open && inProject.isNotEmpty()) {
                                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant,
                                    modifier = Modifier.padding(horizontal = 10.dp))
                                inProject.forEach { s ->
                                    ProjectSessionRow(s, onOpen = { cm.openSession(s.sessionId); onOpen() },
                                        onLongPress = { actionsFor = s })
                                }
                            }
                        }
                    }
                }
            }
            // Creation rows are not results; they drop out of a filtered list so the only thing
            // on screen is what matched.
            if (!searching) item { addRow("New project", indent = false) { showNewProject = true } }
        }
        if (!nothingFound) {
            item {
                Text(stringResource(R.string.chats), style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.padding(start = 10.dp, top = 16.dp, bottom = 2.dp))
            }
        }
        if (!searching) item { addRow("New chat", indent = false) { cm.newSession(); onOpen() } }
        // Remote (roam peer) sessions no longer render here — the Roam tab owns them.
        items(freeChats, key = { "s:" + it.sessionId }) { s -> sessionRow(s, indent = false) }
        if (nothingFound) {
            item(key = "nomatch") { DrawerNoMatches(query, onClearQuery) }
        }
        // Archived chats stay reachable (restorable via long-press -> Unarchive).
        if (archivedChats.isNotEmpty()) {
            item {
                Text(stringResource(R.string.archived), style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.outline,
                    modifier = Modifier.padding(start = 10.dp, top = 16.dp, bottom = 2.dp))
            }
            items(archivedChats, key = { "a:" + it.sessionId }) { s -> sessionRow(s, indent = false) }
        }
    }
}

/** Indented session row used inside an expanded project card (a plain Column, so it
 *  cannot reuse the LazyColumn-scoped `sessionRow`). Tap opens the chat; long-press
 *  opens the session actions dialog. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ProjectSessionRow(s: SessionInfo, onOpen: () -> Unit, onLongPress: () -> Unit) {
    Row(Modifier.fillMaxWidth()
        .combinedClickable(onClick = onOpen, onLongClick = onLongPress)
        .padding(start = 30.dp, end = 12.dp).padding(vertical = 9.dp),
        verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text(s.title.ifBlank { "Untitled chat" }, style = MaterialTheme.typography.bodyLarge,
                maxLines = 1, overflow = TextOverflow.Ellipsis)
            if (s.snippet.isNotBlank())
                Text(s.snippet, style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.outline,
                    maxLines = 1, overflow = TextOverflow.Ellipsis)
            else Text(listOf("${s.messageCount} msgs", relativeTime(s.updatedAt))
                .filter { it.isNotBlank() }.joinToString("  ·  "),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.outline, maxLines = 1)
        }
        if (s.hasNew) {
            Spacer(Modifier.width(10.dp))
            Box(Modifier.size(10.dp).background(MaterialTheme.statusColors.online, CircleShape))
        }
    }
}

/** The drawer's search field. Sits under the Main/Roam tab row, above the chat list. Owns no
 *  state — the query is hoisted to the drawer host so switching tabs keeps what you typed and
 *  there is ONE filter rather than two that disagree.
 *
 *  BasicTextField, not TextField/SearchBar: Material's own container and outline would draw a
 *  second surface inside this one, and SearchBar wants a full-screen search route that a narrow,
 *  scrimmed, swipe-to-close drawer sheet cannot host. Same reasoning as the chat composer's. */
@Composable
fun DrawerSearchField(query: String, onQuery: (String) -> Unit, placeholder: String) {
    Surface(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 6.dp),
        shape = RoundedCornerShape(12.dp),
        color = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.5f),
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Row(Modifier.padding(start = 12.dp, end = 6.dp).padding(vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Filled.Search, contentDescription = null,
                modifier = Modifier.size(18.dp), tint = MaterialTheme.colorScheme.outline)
            Spacer(Modifier.width(10.dp))
            Box(Modifier.weight(1f)) {
                if (query.isEmpty()) {
                    Text(placeholder, style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.outline,
                        maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
                BasicTextField(
                    value = query,
                    onValueChange = onQuery,
                    textStyle = MaterialTheme.typography.bodyMedium.copy(
                        color = MaterialTheme.colorScheme.onSurface),
                    cursorBrush = SolidColor(MaterialTheme.colorScheme.primary),
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                    modifier = Modifier.fillMaxWidth(),
                )
            }
            // The X appears only once there's something to clear: an always-present affordance
            // inside a swipe-to-close drawer invites a mis-tap that shuts the menu.
            if (query.isNotEmpty()) {
                Box(Modifier.size(28.dp).clickable { onQuery("") },
                    contentAlignment = Alignment.Center) {
                    Icon(Icons.Filled.Close, contentDescription = "clear search",
                        modifier = Modifier.size(16.dp),
                        tint = MaterialTheme.colorScheme.outline)
                }
            }
        }
    }
}

/** The drawer's empty-result row, shared by both tabs so "nothing matched" reads the same
 *  whether you filtered local chats or endpoints. It NAMES ITS OWN SCOPE: a search that covers
 *  only titles has to say so, or a miss reads as "that chat is gone". */
@Composable
fun DrawerNoMatches(query: String, onClear: () -> Unit) {
    Column(Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 24.dp),
        horizontalAlignment = Alignment.CenterHorizontally) {
        Text(stringResource(R.string.no_matching_chats, query),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurface,
            textAlign = TextAlign.Center)
        Spacer(Modifier.height(4.dp))
        Text(stringResource(R.string.no_matching_chats_note),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.outline,
            textAlign = TextAlign.Center)
        TextButton(onClick = onClear) { Text(stringResource(R.string.clear_search)) }
    }
}

/** Long-press actions for one session: Rename / Move to project / Archive / Delete.
 *  Archive hides (history stays server-side, restorable via unarchive); Delete is goose's real
 *  session/delete (≥1.44 -- the old "-32601 no delete" note is obsolete) and is permanent.
 *  Move is the sanctioned working_dir rewrite -- also the repair for chats stranded by a
 *  renamed project directory. */
@Composable
internal fun SessionActionsDialog(cm: ConnectionManager, s: SessionInfo, onDone: () -> Unit) {
    var mode by remember(s.sessionId) { mutableStateOf("menu") }
    when (mode) {
        "menu" -> AlertDialog(
            onDismissRequest = onDone,
            title = { Text(s.title.ifBlank { "Untitled chat" }, maxLines = 1, overflow = TextOverflow.Ellipsis) },
            text = {
                val peer = ConnectionManager.roamPeer(s.sessionId)
                Column {
                    if (peer != null) Text("Lives on $peer",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.outline)
                    TextButton(onClick = { mode = "rename" }) { Text(stringResource(R.string.rename_chat_menu)) }
                    // Projects and export are LOCAL concepts — the fork's federation routes
                    // rename/archive/delete to the owning peer (since roam-4), but a remote
                    // session can't be filed into this server's projects or exported here.
                    if (peer == null) {
                        TextButton(onClick = { mode = "move" }) { Text(stringResource(R.string.move_to_project_menu)) }
                        TextButton(onClick = { cm.exportSession(s.sessionId); onDone() }) { Text(stringResource(R.string.export_chat)) }
                    }
                    if (s.archived) {
                        TextButton(onClick = { cm.unarchiveSession(s.sessionId); onDone() }) {
                            Text(stringResource(R.string.unarchive_chat))
                        }
                    }
                    TextButton(onClick = { mode = "archive" }) { Text(stringResource(R.string.archive_chat_menu)) }
                    TextButton(onClick = { mode = "delete" }) {
                        Text(stringResource(R.string.delete_chat_menu), color = MaterialTheme.colorScheme.error)
                    }
                }
            },
            confirmButton = {},
            dismissButton = { TextButton(onClick = onDone) { Text(stringResource(R.string.cancel)) } },
        )
        "rename" -> {
            var newName by remember(s.sessionId) { mutableStateOf(s.title) }
            AlertDialog(
                onDismissRequest = onDone,
                title = { Text(stringResource(R.string.rename_chat)) },
                text = { OutlinedTextField(newName, { newName = it }, singleLine = true,
                    modifier = Modifier.fillMaxWidth()) },
                confirmButton = {
                    TextButton(enabled = newName.isNotBlank(), onClick = {
                        cm.renameSession(s.sessionId, newName); onDone()
                    }) { Text(stringResource(R.string.rename)) }
                },
                dismissButton = { TextButton(onClick = onDone) { Text(stringResource(R.string.cancel)) } },
            )
        }
        "move" -> {
            AlertDialog(
                onDismissRequest = onDone,
                title = { Text(stringResource(R.string.move_to_project)) },
                text = {
                    Column {
                        // Sets project_id. It used to rewrite working_dir, which also moved where
                        // the session's tools ran -- filing a chat and re-homing it were one act.
                        TextButton(enabled = s.projectId != null, onClick = {
                            cm.fileSession(s.sessionId, null); onDone()
                        }) { Text(stringResource(R.string.chats_no_project)) }
                        cm.projects.value.forEach { proj ->
                            TextButton(enabled = proj.id != s.projectId, onClick = {
                                cm.fileSession(s.sessionId, proj.id); onDone()
                            }) { Text(proj.name) }
                        }
                    }
                },
                confirmButton = {},
                dismissButton = { TextButton(onClick = onDone) { Text(stringResource(R.string.cancel)) } },
            )
        }
        "archive" -> AlertDialog(
            onDismissRequest = onDone,
            title = { Text(stringResource(R.string.archive_chat_question)) },
            text = { Text("“${s.title.ifBlank { "Untitled chat" }}” leaves this list; the history " +
                "stays on the server.") },
            confirmButton = { TextButton(onClick = {
                cm.archiveSession(s.sessionId); onDone()
            }) { Text(stringResource(R.string.archive)) } },
            dismissButton = { TextButton(onClick = onDone) { Text(stringResource(R.string.cancel)) } },
        )
        "delete" -> AlertDialog(
            onDismissRequest = onDone,
            title = { Text(stringResource(R.string.delete_chat_question)) },
            text = { Text("Permanently deletes “${s.title.ifBlank { "Untitled chat" }}” and its " +
                "history from the server. Archive instead if you might want it back.") },
            confirmButton = { TextButton(onClick = {
                cm.deleteSession(s.sessionId); onDone()
            }) { Text(stringResource(R.string.delete), color = MaterialTheme.colorScheme.error) } },
            dismissButton = { TextButton(onClick = onDone) { Text(stringResource(R.string.cancel)) } },
        )
    }
}

/** Create a project on the server, then open its first chat. No ACP directory-listing or mkdir
 *  method exists, so ConnectionManager.createProject runs a throwaway fast-model session that
 *  executes the mkdir (see its doc) — the busy state here covers that round trip. */
@Composable
private fun NewProjectDialog(cm: ConnectionManager, onCreated: (String) -> Unit, onDismiss: () -> Unit) {
    var name by rememberSaveable { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    AlertDialog(
        onDismissRequest = { if (!busy) onDismiss() },
        title = { Text(stringResource(R.string.new_project)) },
        text = {
            Column {
                Text(stringResource(R.string.new_project_guidelines),
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.outline)
                Spacer(Modifier.height(8.dp))
                OutlinedTextField(name, { name = it }, singleLine = true, enabled = !busy,
                    placeholder = { Text(stringResource(R.string.example_project)) }, modifier = Modifier.fillMaxWidth())
                if (busy) {
                    Spacer(Modifier.height(10.dp))
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                        Spacer(Modifier.width(10.dp))
                        Text(stringResource(R.string.creating_on_server), style = MaterialTheme.typography.bodySmall)
                    }
                }
                error?.let {
                    Spacer(Modifier.height(8.dp))
                    Text(it, style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error)
                }
            }
        },
        confirmButton = {
            TextButton(enabled = name.isNotBlank() && !busy, onClick = {
                busy = true; error = null
                cm.createProject(name) { err ->
                    if (err == null) onCreated(name) else { busy = false; error = err }
                }
            }) { Text(stringResource(R.string.create)) }
        },
        dismissButton = { TextButton(onClick = onDismiss, enabled = !busy) { Text(stringResource(R.string.cancel)) } },
    )
}
