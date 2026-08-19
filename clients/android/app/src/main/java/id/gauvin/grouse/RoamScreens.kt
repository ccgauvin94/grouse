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


/** Browse view (DEFAULT): saved endpoints as collapsible groups like projects, each
 *  listing its sessions. Live status shown inline; only ready peers carry sessions. */
@Composable
fun RoamBrowse(cm: ConnectionManager, nav: NavController, onOpen: () -> Unit) {
    var expanded by rememberSaveable { mutableStateOf(listOf<String>()) }
    // Populate the peer list from the persisted store (cards survive restart).
    LaunchedEffect(Unit) { cm.loadRoamPeers() }
    val peers = cm.roamPeers
    val status = cm.roamStatus
    // Remote session groups listed per peer; only peers currently CONNECTED (ready) are listed.
    val sessionsByLocalPeer = cm.sessions.value
        .filter { it.sessionId.startsWith("roam:") }
        .mapNotNull { s ->
            val p = ConnectionManager.roamPeer(s.sessionId) ?: return@mapNotNull null
            if (status[p]?.contentEquals("ready") != true) null else p to s
        }
        .groupBy({ it.first }, { it.second })

    LazyColumn(Modifier.fillMaxWidth().padding(horizontal = 14.dp)) {
        item {
            Text(stringResource(R.string.endpoints), style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.padding(start = 10.dp, top = 10.dp, bottom = 2.dp))
        }
        if (peers.isEmpty()) {
            item {
                Text(stringResource(R.string.no_endpoints),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.outline,
                    modifier = Modifier.padding(horizontal = 10.dp, vertical = 8.dp))
            }
        }
        peers.forEach { peer ->
            val peerKey = "peer:${peer.name}"
            val open = peerKey in expanded
            val st = status[peer.name]
            val ready = st == "ready"
            val sessions = sessionsByLocalPeer[peer.name].orEmpty()
            item(key = peerKey) {
                // Each endpoint is a card: colored status dot + name, count of
                // sessions, and a toggle that expands/collapses the chats below.
                // Clicking the NAME (not a caret) drops it down / slides it up.
                // Status is the dot's color — the textual "ready/connecting" label
                // is gone.
                Card(
                    modifier = Modifier.fillMaxWidth()
                        .padding(start = 10.dp, end = 10.dp, bottom = 6.dp)
                        .clickable { expanded = if (open) expanded - peerKey else expanded + peerKey },
                    shape = RoundedCornerShape(14.dp),
                    colors = CardDefaults.cardColors(
                        containerColor = MaterialTheme.colorScheme.surfaceVariant
                            .copy(alpha = 0.5f))
                ) {
                    Row(Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 12.dp),
                        verticalAlignment = Alignment.CenterVertically) {
                        // Status dot: online/connecting/error, no text.
                        Box(Modifier.size(10.dp)
                            .background(
                                when {
                                    ready -> MaterialTheme.statusColors.online
                                    st?.startsWith("connecting") == true -> MaterialTheme.statusColors.connecting
                                    st?.startsWith("error") == true -> MaterialTheme.colorScheme.error
                                    else -> MaterialTheme.colorScheme.outline
                                },
                                CircleShape))
                        Spacer(Modifier.width(10.dp))
                        Icon(Icons.Filled.Public, contentDescription = null,
                            modifier = Modifier.size(18.dp), tint = MaterialTheme.colorScheme.primary)
                        Spacer(Modifier.width(8.dp))
                        Text(peer.name, style = MaterialTheme.typography.bodyLarge,
                            modifier = Modifier.weight(1f),
                            maxLines = 1, overflow = TextOverflow.Ellipsis)
                        if (sessions.isNotEmpty()) Text("${sessions.size}",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.outline)
                        // New chat ON this peer (session/new on its connection).
                        if (ready) {
                            IconButton(onClick = {
                                cm.newRoamSession(peer.name)
                                // Leave the browse slide: the chat opens on the
                                // peer's on_peer_new_session, and this state change
                                // (currentSession.next) drives the ChatScreen.
                                onOpen()
                            }) {
                                Icon(Icons.Filled.Add,
                                    contentDescription = "new chat on ${peer.name}",
                                    tint = MaterialTheme.colorScheme.primary)
                            }
                        }
                    }
                }
            }
            ConnectionManager.roamStatusDetail(st)?.let { detail ->
                item(key = "$peerKey:detail") {
                    Text(detail, style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.padding(start = 40.dp, end = 10.dp, bottom = 8.dp))
                }
            }
            if (open && sessions.isNotEmpty()) {
                // Stable per-session keys + a deterministic tiebreak (updatedAt desc, then id):
                // a session/load on tap bumps that session's updatedAt, which re-sorts the
                // list mid-gesture; without a stable key the tapped Row is replaced and the
                // click's navigation is swallowed (the classic "click twice to open").
                sessions.sortedWith(
                    compareByDescending<SessionInfo> { it.updatedAt }
                        .thenBy { it.sessionId }
                ).forEach { s ->
                    item(key = "roam-sess:${s.sessionId}") {
                        Row(Modifier.fillMaxWidth()
                            .clickable { cm.openSession(s.sessionId); onOpen() }
                            .padding(start = 34.dp, end = 8.dp).padding(vertical = 9.dp),
                            verticalAlignment = Alignment.CenterVertically) {
                            Column(Modifier.weight(1f)) {
                                Text(s.title.ifBlank { "Untitled chat" },
                                    style = MaterialTheme.typography.bodyLarge,
                                    maxLines = 1, overflow = TextOverflow.Ellipsis)
                                if (s.snippet.isNotBlank())
                                    Text(s.snippet, style = MaterialTheme.typography.bodySmall,
                                        color = MaterialTheme.colorScheme.outline,
                                        maxLines = 1, overflow = TextOverflow.Ellipsis)
                            }
                            // Green dot: backgrounded content arrived for a
                            // chat that's not on screen (roam staging).
                            if (s.hasNew) {
                                Spacer(Modifier.width(10.dp))
                                Box(Modifier.size(10.dp).background(MaterialTheme.statusColors.online, CircleShape))
                            }
                        }
                    }
                }
            }
        }
    }
}

/** Add/management page (standalone route): this device's identity (incl. a host-scannable
 *  QR), add-host form, and per-host connect/disconnect. Opened from the drawer's
 *  "New connection" item. Full-screen on purpose — the camera card scanner must not sit
 *  under the drawer scrim. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun RoamAddConnectionScreen(cm: ConnectionManager, nav: NavController) {
    var name by remember { mutableStateOf("") }
    var card by remember { mutableStateOf("") }
    var error by remember { mutableStateOf<String?>(null) }
    val peers = cm.roamPeers
    val status = cm.roamStatus

    // A scanned card arrives back via the qrscan route's savedStateHandle.
    LaunchedEffect(nav.currentBackStackEntry) {
        nav.currentBackStackEntry?.savedStateHandle
            ?.getStateFlow<String?>("qr_card", null)?.collect { value ->
                if (value != null) {
                    card = value
                    nav.currentBackStackEntry?.savedStateHandle?.set("qr_card", null)
                }
            }
    }

    Scaffold(topBar = {
        TopAppBar(
            title = { Text(stringResource(R.string.new_connection)) },
            navigationIcon = {
                IconButton(onClick = { nav.popBackStack() }) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "back")
                }
            },
        )
    }) { pad ->
        Column(Modifier.padding(pad).padding(horizontal = 16.dp).fillMaxSize()
            .verticalScroll(rememberScrollState())) {
            SettingsSection("This device") {
                SettingCaption("The host you pair with sees this key. Paste the host's card " +
                    "below, or show this QR / copy this card to a host to pair — then accept " +
                    "this device on the host (`goose roam peers accept`). Either direction works.")
                DeviceQr(cm, modifier = Modifier.size(200.dp).align(Alignment.CenterHorizontally)
                    .padding(vertical = 4.dp))
                SelectionContainer {
                    Text(cm.roamPublicKey, style = MaterialTheme.typography.bodySmall,
                        fontFamily = androidx.compose.ui.text.font.FontFamily.Monospace)
                }
                Spacer(Modifier.height(8.dp))
                Row(verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.fillMaxWidth()) {
                    // The shareable card — paste this into `goose roam peers` / a pairing tool.
                    Text(cm.deviceCard, style = MaterialTheme.typography.bodySmall,
                        fontFamily = androidx.compose.ui.text.font.FontFamily.Monospace,
                        maxLines = 2, overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f))
                    val clip = LocalClipboardManager.current
                    val ctx = LocalContext.current
                    IconButton(onClick = {
                        clip.setText(AnnotatedString(cm.deviceCard))
                        Toast.makeText(ctx, "Card copied", Toast.LENGTH_SHORT).show()
                    }) {
                        Icon(Icons.Filled.ContentCopy, contentDescription = "Copy device card")
                    }
                }
            }
            SettingsSection("Add a host") {
                OutlinedTextField(name, { name = it }, label = { Text(stringResource(R.string.name)) }, singleLine = true,
                    modifier = Modifier.fillMaxWidth())
                OutlinedTextField(card, { card = it }, label = { Text(stringResource(R.string.connection_card)) },
                    modifier = Modifier.fillMaxWidth().padding(top = 8.dp), minLines = 2)
                error?.let {
                    Text(it, color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.padding(top = 6.dp))
                }
                Row(Modifier.padding(top = 8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(onClick = {
                        nav.navigate("qrscan") { launchSingleTop = true }
                    }) { Text(stringResource(R.string.scan_qr)) }
                    Button(onClick = {
                        error = if (name.isBlank()) "Give the host a name."
                                else cm.addRoamPeer(name.trim(), card.trim())
                        if (error == null) { name = ""; card = "" }
                    }) { Text(stringResource(R.string.save_host)) }
                }
            }
            SettingsSection("Hosts") {
                if (peers.isEmpty())
                    SettingCaption("No hosts yet — paste a connection card from a " +
                        "`goose serve --roam` or `roam share` host.")
                peers.forEach { peer ->
                    val st = status[peer.name]
                    val ready = st == "ready"
                    val statusColor = when {
                        ready -> MaterialTheme.statusColors.online
                        st?.startsWith("connecting") == true -> MaterialTheme.statusColors.connecting
                        st?.startsWith("error") == true -> MaterialTheme.colorScheme.error
                        else -> MaterialTheme.colorScheme.outline
                    }
                    Row(verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
                        Column(Modifier.weight(1f)) {
                            Text(peer.name, style = MaterialTheme.typography.bodyLarge)
                            Text(peer.fingerprint, style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.outline)
                            Text(ConnectionManager.roamStatusShort(st),
                                style = MaterialTheme.typography.labelSmall, color = statusColor)
                            // This column wraps, so the full explanation fits here too.
                            ConnectionManager.roamStatusDetail(st)?.let { detail ->
                                Text(detail, style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.error)
                            }
                        }
                        if (ready) {
                            TextButton(onClick = { cm.disconnectRoam(peer.name) }) { Text(stringResource(R.string.disconnect)) }
                            // New chat ON the peer: session/new on its connection.
                            TextButton(onClick = {
                                cm.newRoamSession(peer.name)
                                // Jump to the chat surface: the session opens on
                                // the peer's on_peer_new_session once created.
                                nav.navigate("chat") {
                                    launchSingleTop = true
                                    popUpTo("chat") { inclusive = true }
                                }
                            }) { Text(stringResource(R.string.new_chat)) }
                        } else {
                            Button(onClick = { cm.connectRoam(peer.name) }) { Text(stringResource(R.string.connect)) }
                        }
                        IconButton(onClick = { cm.removeRoamPeer(peer.name) }) {
                            Icon(Icons.Filled.Delete, contentDescription = "remove ${peer.name}")
                        }
                    }
                }
            }
            Spacer(Modifier.height(16.dp))
        }
    }
}
