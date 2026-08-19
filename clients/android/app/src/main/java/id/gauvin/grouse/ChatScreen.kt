// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import id.gauvin.grouse.ui.theme.GrouseShapes
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


/** A donut that shows how full the context window is, colored by % used —
 *  green until ~60%, amber through ~90%, red beyond (matches the compacting
 *  coverage of the usage line). `pct` is 0..100. */
@Composable
fun ContextRing(pct: Int, modifier: Modifier = Modifier, onClick: (() -> Unit)? = null) {
    val fraction = (pct / 100f).coerceIn(0f, 1f)
    val track = MaterialTheme.colorScheme.outlineVariant
    val color = when {
        pct >= 90 -> MaterialTheme.colorScheme.error
        pct >= 60 -> MaterialTheme.statusColors.connecting   // amber — nearing compaction
        else -> MaterialTheme.statusColors.online        // green — comfortable
    }
    val base = Modifier
    val m = if (onClick != null) base.clickable(onClick = onClick) else base
    Canvas(m.then(modifier)) {
        val stroke = Stroke(width = 2.dp.toPx(), cap = StrokeCap.Round)
        // Full ring = track; the filled arc from 12 o'clock = usage.
        drawArc(color = track, startAngle = -90f, sweepAngle = 360f,
            useCenter = false, style = stroke,
            topLeft = Offset.Zero,
            size = Size(size.width, size.height))
        if (fraction > 0f) {
            drawArc(color = color, startAngle = -90f, sweepAngle = 360f * fraction,
                useCenter = false, style = stroke,
                topLeft = Offset.Zero,
                size = Size(size.width, size.height))
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ChatScreen(cm: ConnectionManager, onOpenDrawer: () -> Unit) {
    val ctx = LocalContext.current
    var showSchedule by remember { mutableStateOf(false) }
    var showContextDetail by remember { mutableStateOf(false) }    // Assistant health from last-briefing recency (briefings run hourly 7 AM–10 PM). Green = fresh,
    // yellow = late, red = stale/never. Overnight the gap grows to ~9h and that's still healthy.
    val lastBriefing = cm.store.lastBriefingAt
    val briefingAgo = if (lastBriefing > 0)
        relativeTime(java.time.Instant.ofEpochMilli(lastBriefing).toString()) else "none yet"
    val briefingAgeMin = if (lastBriefing > 0) (System.currentTimeMillis() - lastBriefing) / 60000 else Long.MAX_VALUE
    val briefingActiveHrs = java.time.ZonedDateTime.now().hour in 7..22
    val briefingHealthy = lastBriefing > 0 &&
        (if (briefingActiveHrs) briefingAgeMin <= 90 else briefingAgeMin <= 12 * 60)
    val briefingLate = lastBriefing > 0 && !briefingHealthy &&
        (if (briefingActiveHrs) briefingAgeMin <= 180 else briefingAgeMin <= 15 * 60)
    var hintDismissed by remember { mutableStateOf(cm.store.assistantHintSeen) }
    // rememberSaveable so a rotation/dark-mode recreate doesn't wipe the typed draft.
    var input by rememberSaveable { mutableStateOf("") }
    // Hoisted to ConnectionManager so picked images survive recreation (see draftAttachments).
    val attachments = cm.draftAttachments
    val listState = rememberLazyListState()

    val picker = rememberLauncherForActivityResult(
        ActivityResultContracts.PickVisualMedia()
    ) { uri: Uri? -> uri?.let { readImage(ctx, it)?.let(attachments::add) } }

    // Camera capture (returns a JPEG bitmap; the "camera square" in the + sheet).
    // TakePicturePreview launches the system camera app, which demands the CAMERA
    // permission be granted to US first — launching revoked crashes with
    // SecurityException ("Permission Denial … with revoked permission").
    var cameraPending by remember { mutableStateOf(false) }
    val cameraLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.TakePicturePreview()
    ) { bmp: Bitmap? -> bmp?.let { attachments.add(bitmapToImage(it)) }; cameraPending = false }
    val cameraPerm = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) { granted ->
        if (granted) cameraLauncher.launch(null) else cameraPending = false
    }
    fun launchCamera() {
        cameraPending = true
        if (androidx.core.content.ContextCompat.checkSelfPermission(
                ctx, android.Manifest.permission.CAMERA) ==
            android.content.pm.PackageManager.PERMISSION_GRANTED) {
            cameraLauncher.launch(null)
        } else {
            cameraPerm.launch(android.Manifest.permission.CAMERA)
        }
    }

    // Generic file attach (the "Files square"): text files become text blocks, the rest blobs.
    val filePicker = rememberLauncherForActivityResult(
        ActivityResultContracts.OpenDocument()
    ) { uri: Uri? ->
        uri ?: return@rememberLauncherForActivityResult
        val name = runCatching {
            ctx.contentResolver.query(uri, null, null, null, null)?.use { c ->
                val i = c.getColumnIndex(android.provider.OpenableColumns.DISPLAY_NAME)
                if (i >= 0) c.getString(i) else null
            }
        }.getOrNull() ?: uri.lastPathSegment?.substringAfterLast('/') ?: "file"
        readFile(ctx, uri, name)?.let(cm.draftFiles::add)
    }
    var showComposer by remember { mutableStateOf(false) }
    var showModelSheet by remember { mutableStateOf(false) }

    // Content shared into Goose from another app — append (don't clobber an in-progress draft).
    LaunchedEffect(cm.pendingShareText.value) {
        cm.pendingShareText.value?.let { input = (input.trim() + " " + it).trim(); cm.pendingShareText.value = null }
    }
    LaunchedEffect(cm.pendingShareImages.size) {
        if (cm.pendingShareImages.isNotEmpty()) {
            attachments.addAll(cm.pendingShareImages); cm.pendingShareImages.clear()
        }
    }

    val currentModel = cm.config.value.firstOrNull { it.id == "model" }?.currentValue ?: ""
    var showVisionWarn by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    // "At bottom" = the last item is visible; drives autoscroll + the jump-to-bottom button.
    // With reverseLayout the bottom is index 0; we're at the bottom when it's fully shown.
    val atBottom by remember {
        derivedStateOf {
            listState.firstVisibleItemIndex == 0 && listState.firstVisibleItemScrollOffset == 0
        }
    }

    fun reallySend() {
        cm.send(input.trim(), attachments.toList(), cm.draftFiles.toList())
        input = ""; attachments.clear(); cm.draftFiles.clear()
    }
    fun doSend() {
        if (input.isBlank() && attachments.isEmpty() && cm.draftFiles.isEmpty()) return
        // Sending images to a non-vision model (esp. LocalAI without mmproj) hangs the session.
        // The heuristic is a name-substring guess and gets it wrong (it missed Qwen3.6-35B-A3B,
        // which reads images fine); the user's own past answer for this exact model overrides it.
        if (attachments.isNotEmpty() && !isLikelyVisionModel(currentModel) &&
            !cm.store.visionOk(currentModel)) { showVisionWarn = true; return }
        reallySend()
    }

    // Stay pinned to the bottom (index 0) as messages stream in / arrive — unless the user scrolled up.
    // Driven off snapshotFlow so per-token text growth doesn't recompose the whole ChatScreen (this
    // used to read the last message's length in the composable body). Instant scrollToItem avoids
    // restarting a scroll animation on every token.
    LaunchedEffect(listState) {
        snapshotFlow { cm.messages.size to (cm.messages.lastOrNull()?.text?.length ?: 0) }
            .collect { if (atBottom) listState.scrollToItem(0) }
    }
    // Opening/switching a session: snap to the bottom (index 0). reverseLayout keeps it pinned
    // as history replays in.
    LaunchedEffect(cm.currentSession.value) { listState.scrollToItem(0) }
    // A replay swapped in a genuinely different transcript: go to the bottom. Identical
    // Rebuilds never touch `messages`, so the reading position survives untouched and this
    // never fires for them.
    LaunchedEffect(cm.replayDoneTick.value) { listState.scrollToItem(0) }

    cm.elicitations.firstOrNull()?.let { e ->
        ElicitationSheet(e,
            onAccept = { values -> cm.answerElicitation(e, values) },
            onDecline = { cm.answerElicitation(e, null) },
            onCancel = { cm.answerElicitation(e, null, cancelled = true) })
    }

    // Background RPC failures surface as a toast, not a transcript bubble — the chat is
    // not where a failed sidebar refresh belongs.
    val bgCtx = LocalContext.current
    LaunchedEffect(cm.backgroundNotice.value) {
        cm.backgroundNotice.value?.let {
            Toast.makeText(bgCtx, it.take(200), Toast.LENGTH_SHORT).show()
            cm.backgroundNotice.value = null
        }
    }

    // A session export hands the JSON to the system share sheet — files app, Drive, whatever
    // the user points it at. Text share deliberately: no FileProvider plumbing needed.
    LaunchedEffect(cm.exportData.value) {
        cm.exportData.value?.let { data ->
            val intent = android.content.Intent(android.content.Intent.ACTION_SEND).apply {
                type = "application/json"
                putExtra(android.content.Intent.EXTRA_TEXT, data)
            }
            bgCtx.startActivity(android.content.Intent.createChooser(intent, "Export session"))
            cm.exportData.value = null
        }
    }
    cm.permissions.firstOrNull()?.let { req ->
        PermissionSheet(req, onChoose = { cm.answerPermission(req, it) })
    }

    if (showComposer) {
        ComposerSheet(cm,
            onCamera = { launchCamera() },
            onPhotos = { picker.launch(PickVisualMediaRequest(
                ActivityResultContracts.PickVisualMedia.ImageOnly)) },
            onFiles = { filePicker.launch(arrayOf("*/*")) },
            onDismiss = { showComposer = false },
        )
    }

    if (showModelSheet) {
        ModelSheet(
            cm.config.value,
            cm.providerChoices(
                cm.config.value.firstOrNull { it.id == "provider" }?.currentValue.orEmpty()
            ),
            if (ConnectionManager.roamPeer(cm.currentSession.value) != null) emptySet()
            else cm.knownModels.value,
            cm::setOption,
            onDismiss = { showModelSheet = false },
        )
    }

    if (showVisionWarn) AlertDialog(
        onDismissRequest = { showVisionWarn = false },
        title = { Text(stringResource(R.string.model_may_not_support_images)) },
        text = { Text("“$currentModel” probably can't read images and may get stuck on them — " +
            "even later text messages. If that happens, start a New chat.\n\n" +
            "Sending anyway won't ask again for this model.") },
        confirmButton = { TextButton(onClick = {
            showVisionWarn = false
            cm.store.markVisionOk(currentModel)   // remember: don't ask again for THIS model
            reallySend()
        }) { Text(stringResource(R.string.send_anyway)) } },
        dismissButton = { TextButton(onClick = { showVisionWarn = false }) { Text(stringResource(R.string.cancel)) } },
    )

    if (showSchedule) AlertDialog(
        onDismissRequest = { showSchedule = false },
        confirmButton = { TextButton(onClick = { showSchedule = false }) { Text(stringResource(R.string.close)) } },
        title = { Text(stringResource(R.string.assistant_briefings)) },
        text = {
            Column {
                Text(stringResource(R.string.briefing_schedule_note))
                Spacer(Modifier.height(6.dp))
                Text("Last briefing: $briefingAgo")
                Text("Status: " + when {
                    briefingHealthy -> "up to date"
                    briefingLate -> "running late"
                    lastBriefing <= 0L -> "no briefings yet"
                    else -> "not updating"
                })
                Spacer(Modifier.height(8.dp))
                Text(stringResource(R.string.schedule_managed_on_server),
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.outline)
            }
        },
    )

    Scaffold(topBar = {
        Column {
        TopAppBar(
            title = {
                val online = cm.online.value
                val busy = cm.busy.value
                val dot = when {
                    online -> MaterialTheme.statusColors.online                       // connected → green
                    cm.status.value.contains("connect", true) ||
                        cm.status.value.contains("load", true) -> MaterialTheme.statusColors.connecting  // connecting → amber
                    else -> MaterialTheme.colorScheme.error          // offline → red
                }
                Column {
                    Row(verticalAlignment = Alignment.CenterVertically,
                        // fillMaxWidth lets the weight spacer push the donut to the
                        // far right of the title row.
                        modifier = Modifier.fillMaxWidth()
                            .clickable(enabled = !online) { cm.connectSaved() }) {
                        Surface(color = dot, shape = RoundedCornerShape(50), modifier = Modifier.size(10.dp)) {}
                        Spacer(Modifier.width(8.dp))
                        // Single line + ellipsis: this Row shares the app bar with up to 3 action
                        // icons, so on a compact width the default (titleLarge, unbounded) title text
                        // used to wrap to a second line and visually collide with/get clipped by the
                        // icons (e.g. "Assistant · working…" broke across two lines mid-word). A
                        // tighter style plus a hard single-line cap keeps it readable and self-eliding
                        // instead of visually breaking.
                        Text(
                            when {
                                // A session/load replay is streaming into the buffer: count
                                // replayed messages live — a big history can take tens of
                                // seconds and a static "Connecting…" looked hung. The counter
                                // proves it's advancing; it also covers the fresh-install
                                // case where there is no cached snapshot to paint meanwhile.
                                cm.replayActive.value -> "Loading… ${cm.replayProgress.value}"
                                cm.onAssistant && busy -> "Assistant · working…"
                                cm.onAssistant -> "Assistant"
                                online && busy -> "Grouse · working…"
                                online -> "Grouse"
                                cm.status.value.contains("connect", true) ||
                                    cm.status.value.contains("load", true) -> "Connecting…"
                                else -> "Offline · tap to reconnect"
                            },
                            style = MaterialTheme.typography.titleMedium,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        // Context window fullness, as a colored donut aligned to the
                        // FAR RIGHT of the title row (it's a status readout, not part of
                        // the chat name). Shown once the first turn reports usage.
                        val u = cm.usage.value
                        if (u != null && u.size > 0 && !cm.compacting.value) {
                            Spacer(Modifier.weight(1f))   // push the donut to the right edge
                            Spacer(Modifier.width(8.dp))  // breathing room off the action icons
                            ContextRing(
                                pct = (u.used * 100 / u.size).coerceIn(0, 100),
                                modifier = Modifier.size(14.dp),
                                onClick = { showContextDetail = !showContextDetail }
                            )
                        }
                    }
                }
            },
            navigationIcon = {
                IconButton(onClick = onOpenDrawer) { Icon(Icons.Filled.Menu, contentDescription = "menu") }
            },
            actions = {
                // Assistant/Sessions/Settings are now drawer items — this icon's navigation role
                // moved there. On the Assistant thread itself it stays as the page-local briefing-
                // health indicator (opens the schedule dialog); off it, there's nothing to show here.
                if (cm.onAssistant) IconButton(onClick = { showSchedule = true }) {
                    val dot = when {
                        briefingHealthy -> MaterialTheme.statusColors.online                     // fresh → green
                        briefingLate -> MaterialTheme.statusColors.connecting                        // late → amber
                        else -> MaterialTheme.colorScheme.error                  // stale/never → red
                    }
                    Box {
                        Icon(Icons.Filled.Psychology, contentDescription = "assistant status",
                            tint = MaterialTheme.colorScheme.primary)
                        Surface(color = dot, shape = RoundedCornerShape(50),
                            modifier = Modifier.size(9.dp).align(Alignment.TopEnd)) {}
                    }
                }
                val roamPeer = ConnectionManager.roamPeer(cm.currentSession.value)
                // Tools and model/mode now live in the + sheet and model pill — nothing
                // in the top bar for them. The roam chip stays as the "this chat is
                // remote" marker.
                if (roamPeer != null) AssistChip(
                    onClick = {},
                    label = { Text(roamPeer) },
                    leadingIcon = { Icon(Icons.Filled.Public, contentDescription = "remote session",
                        modifier = Modifier.size(16.dp)) },
                    modifier = Modifier.padding(end = 4.dp),
                )
            }
        )
                        // Compacting (manual /compact or a server-triggered auto-compact) takes priority
                        // over the usage line — it's transient and explains why the numbers are about to
                        // change. INDETERMINATE: the protocol only ever sends text status lines, never a
                        // numeric percentage, so there's no real fraction to show.
                        if (cm.compacting.value) {
                            Row(verticalAlignment = Alignment.CenterVertically,
                                modifier = Modifier.padding(start = 18.dp, top = 2.dp)) {
                                LinearProgressIndicator(modifier = Modifier.width(40.dp).height(3.dp))
                                Spacer(Modifier.width(6.dp))
                                Text(stringResource(R.string.compacting), style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.outline)
                            }
                        } else {
                            // Context window panel — slides down under the title when
                            // the donut is tapped: colored bar, number, Compact button.
                            val u = cm.usage.value
                            AnimatedVisibility(visible = showContextDetail && u != null && u.size > 0,
                                enter = expandVertically() + fadeIn(),
                                exit = shrinkVertically() + fadeOut()) {
                                u?.let { uu ->
                                    val pct = (uu.used * 100 / uu.size).coerceIn(0, 100)
                                    val barColor = when {
                                        pct >= 90 -> MaterialTheme.colorScheme.error
                                        pct >= 60 -> MaterialTheme.statusColors.connecting
                                        else -> MaterialTheme.statusColors.online
                                    }
                                    Row(verticalAlignment = Alignment.CenterVertically,
                                        modifier = Modifier.padding(start = 18.dp, top = 2.dp).fillMaxWidth()) {
                                        Row(verticalAlignment = Alignment.CenterVertically,
                                            modifier = Modifier.weight(1f)) {
                                            Box(Modifier.width(64.dp).height(6.dp)
                                                .clip(RoundedCornerShape(3.dp))
                                                .background(MaterialTheme.colorScheme.surfaceVariant)) {
                                                Box(Modifier.fillMaxWidth(pct / 100f).fillMaxHeight()
                                                    .background(barColor))
                                            }
                                            Spacer(Modifier.width(8.dp))
                                            Text("${fmtTokens(uu.used)} / ${fmtTokens(uu.size)} · ${pct}%",
                                                style = MaterialTheme.typography.labelSmall,
                                                color = if (pct >= 90) MaterialTheme.colorScheme.error
                                                        else MaterialTheme.colorScheme.outline)
                                        }
                                        TextButton(onClick = { cm.compact() },
                                            enabled = !cm.compacting.value) {
                                            Text(stringResource(R.string.compact))
                                        }
                                    }
                                }
                            }
                        }
        }
    }) { pad ->
        Column(Modifier.padding(pad).padding(horizontal = 12.dp).fillMaxSize()) {
            if (cm.onAssistant && !hintDismissed) AssistantHint {
                hintDismissed = true; cm.store.assistantHintSeen = true
            }
            Box(Modifier.weight(1f).fillMaxWidth()) {
                if (cm.messages.isEmpty() && !cm.busy.value && cm.replayActive.value) {
                    // Opening an existing chat with nothing painted yet: the wire
                    // is fetching it. The landing copy below would read as "this
                    // chat is empty", which is the opposite of what is happening.
                    Column(
                        Modifier.fillMaxSize().padding(32.dp),
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.Center
                    ) {
                        CircularProgressIndicator(Modifier.size(28.dp), strokeWidth = 3.dp)
                        Spacer(Modifier.height(14.dp))
                        Text(stringResource(R.string.loading_conversation), style = MaterialTheme.typography.titleMedium)
                        if (cm.replayProgress.value > 0) {
                            Spacer(Modifier.height(4.dp))
                            Text("${cm.replayProgress.value} messages",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.outline)
                        }
                    }
                } else if (cm.messages.isEmpty() && !cm.busy.value) {
                    Column(
                        Modifier.fillMaxSize().padding(32.dp),
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.Center
                    ) {
                        Icon(Icons.Filled.Psychology, contentDescription = null,
                            modifier = Modifier.size(56.dp), tint = MaterialTheme.colorScheme.primary)
                        Spacer(Modifier.height(12.dp))
                        Text(if (cm.online.value) "Ask Grouse anything" else "Connecting…",
                            style = MaterialTheme.typography.titleMedium)
                        Spacer(Modifier.height(4.dp))
                        Text(stringResource(R.string.wired_up_hint),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.outline, textAlign = TextAlign.Center)
                    }
                } else {
                    // reverseLayout: index 0 renders at the BOTTOM. Typing indicator first (very
                    // bottom), then messages newest→oldest upward. Bottom is always index 0, so
                    // "open at bottom", "jump to bottom", and streaming-stays-pinned are trivial.
                    LazyColumn(state = listState, reverseLayout = true, modifier = Modifier.fillMaxSize()) {
                        if (cm.busy.value) item { TypingIndicator() }
                        // Consecutive tool calls collapse into one dropdown (goose often fires a run of
                        // 5-10 shell/read calls back to back — a wall of individual chips otherwise).
                        // Grouped on the forward list so adjacency is chronological, then reversed for
                        // display like the flat list was. NOT remembered on messages.size: the streaming
                        // assistant message updates via .copy() (new object, same id, size unchanged), so
                        // a size-keyed cache would go stale mid-stream. Grouping is O(#messages) — cheap.
                        val grouped = groupChatItems(cm.messages).asReversed()
                        // key on the stable id of the group's first message so a growing tool-call run
                        // (or the streaming assistant bubble) reuses its composition instead of rebuilding;
                        // i==0 is the newest item → mark an assistant Msg streaming so it renders as plain
                        // text until the turn finishes (skips the per-token Markdown re-parse).
                        itemsIndexed(grouped, key = { _, item -> item.firstId }) { i, item ->
                            when (item) {
                                is ChatItem.Tools -> ToolChipGroup(item.items)
                                is ChatItem.Msg -> MessageBubble(
                                    item.m, streaming = i == 0 && cm.busy.value && item.m.role == "assistant",
                                    // Each message carries its own stats now, so an older reply
                                    // long-presses to ITS numbers rather than the latest turn's.
                                    usage = item.m.usage)
                            }
                        }
                    }
                    if (!atBottom) SmallFloatingActionButton(
                        onClick = { scope.launch { listState.animateScrollToItem(0) } },
                        modifier = Modifier.align(Alignment.BottomEnd).padding(10.dp)
                    ) { Icon(Icons.Filled.KeyboardArrowDown, contentDescription = "scroll to bottom") }
                }
            }

            // Slash-command autocomplete (goose's available commands).
            val slash = input.startsWith("/") && !input.contains(' ')
            if (slash) {
                val matches = cm.commands.value.filter { it.startsWith(input.drop(1), true) }.take(6)
                if (matches.isNotEmpty()) Surface(
                    color = MaterialTheme.colorScheme.surfaceVariant,
                    shape = MaterialTheme.shapes.small, modifier = Modifier.fillMaxWidth()
                ) {
                    Column {
                        matches.forEach { name ->
                            Text("/$name", modifier = Modifier.fillMaxWidth()
                                .clickable { input = "/$name " }.padding(horizontal = 12.dp, vertical = 8.dp),
                                style = MaterialTheme.typography.bodyMedium)
                        }
                    }
                }
            }

            if (attachments.isNotEmpty() || cm.draftFiles.isNotEmpty()) Row(Modifier.padding(vertical = 4.dp)) {
                attachments.forEachIndexed { i, _ ->
                    AssistChip(onClick = { attachments.removeAt(i) },
                        label = { Text("image ${i + 1}") },
                        trailingIcon = { Icon(Icons.Filled.Close, contentDescription = "remove",
                            modifier = Modifier.size(16.dp)) },
                        modifier = Modifier.padding(end = 6.dp))
                }
                cm.draftFiles.forEachIndexed { i, f ->
                    AssistChip(onClick = { cm.draftFiles.removeAt(i) },
                        label = { Text(f.name, maxLines = 1,
                            overflow = TextOverflow.Ellipsis) },
                        leadingIcon = { Icon(Icons.Filled.InsertDriveFile, contentDescription = null,
                            modifier = Modifier.size(16.dp)) },
                        trailingIcon = { Icon(Icons.Filled.Close, contentDescription = "remove",
                            modifier = Modifier.size(16.dp)) },
                        modifier = Modifier.padding(end = 6.dp))
                }
            }

            // A queued message's bubble is identical to a sent one, so without this there is no way
            // to tell "waiting its turn" from "silently dropped".
            if (cm.queuedCount.value > 0) {
                Text("${cm.queuedCount.value} queued — will send when this turn finishes",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(start = 4.dp, top = 2.dp))
            }

            // Composer modelled on Claude's: ONE rounded, outlined container holding the text
            // field and the action row together, rather than a pill field with buttons floating
            // underneath it. The container is the affordance -- everything inside belongs to the
            // message you are composing.
            //
            // Send/stop is a single filled circle on the right that CHANGES MEANING with state
            // (arrow to send, square to stop), which is why it reads at a glance. The previous
            // layout showed stop and send as two separate square buttons simultaneously.
            Surface(
                shape = GrouseShapes.composer,
                color = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.35f),
                border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.5f)),
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp, bottom = 4.dp),
            ) {
                Column(Modifier.padding(start = 18.dp, end = 10.dp, top = 14.dp, bottom = 8.dp)) {
                    // BasicTextField, not TextField: Material's own container/padding/indicator
                    // would draw a second surface inside this one. Here the Surface IS the field.
                    Box(Modifier.fillMaxWidth().padding(bottom = 6.dp)) {
                        if (input.isEmpty()) {
                            Text(
                                if (cm.busy.value) "Queue message…" else "Message Grouse…",
                                style = MaterialTheme.typography.bodyLarge,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        BasicTextField(
                            value = input,
                            onValueChange = { input = it },
                            textStyle = MaterialTheme.typography.bodyLarge.copy(
                                color = MaterialTheme.colorScheme.onSurface),
                            cursorBrush = SolidColor(MaterialTheme.colorScheme.primary),
                            maxLines = 8,
                            modifier = Modifier.fillMaxWidth().heightIn(max = 200.dp),
                        )
                    }
                    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                        // "+" pill, left -- Claude-style composer: attach (camera/photos/files),
                        // the tool-approval mode, and the tools-for-this-chat list all live in
                        // the bottom sheet it opens. The MODEL pill stays beside it (the model
                        // you're about to prompt with, worth seeing at a glance while typing).
                        Surface(
                            shape = RoundedCornerShape(20.dp),
                            color = MaterialTheme.colorScheme.surface,
                            modifier = Modifier.clickable { showComposer = true },
                        ) {
                            Row(
                                Modifier.padding(horizontal = 12.dp, vertical = 7.dp),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Icon(Icons.Filled.Add, contentDescription = "attach, mode, tools",
                                    modifier = Modifier.size(18.dp),
                                    tint = MaterialTheme.colorScheme.onSurface)
                            }
                        }
                        Spacer(Modifier.width(6.dp))
                        // MODEL pill, right beside "+": the model you're about to prompt with,
                        // switchable without opening the Tune panel. Opens the full-width
                        // source/model/effort sheet.
                        val modelOpt = cm.config.value.firstOrNull { it.id == "model" }
                        ModelPill(modelOpt, cm.knownModels.value, cm::setOption,
                            onOpenSheet = { showModelSheet = true })
                        Spacer(Modifier.weight(1f))
                        // One circle, two states: stop while a turn runs, send when there is
                        // text. Sending mid-turn queues, so the arrow is never wrong -- it just
                        // may not go out immediately.
                        val canSend = input.isNotBlank()
                        FilledIconButton(
                            onClick = { if (canSend) doSend() else if (cm.busy.value) cm.cancel() },
                            enabled = canSend || cm.busy.value,
                            modifier = Modifier.size(42.dp),
                            colors = IconButtonDefaults.filledIconButtonColors(
                                containerColor = if (canSend) MaterialTheme.colorScheme.primary
                                                 else MaterialTheme.colorScheme.surface,
                            ),
                        ) {
                            Icon(
                                if (canSend) Icons.Filled.ArrowUpward else Icons.Filled.Stop,
                                contentDescription = when {
                                    canSend -> if (cm.busy.value) "queue" else "send"
                                    else -> "stop"
                                },
                                modifier = Modifier.size(20.dp),
                            )
                        }
                    }
                }
            }
        }
    }
}

/** Tool-approval bottom sheet. Options come straight from goose (allow/reject × once/always). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PermissionSheet(req: AcpEvent.Permission, onChoose: (String?) -> Unit) {
    ModalBottomSheet(onDismissRequest = { onChoose(null) }) {
        Column(Modifier.padding(horizontal = 20.dp).padding(bottom = 28.dp)) {
            Text(stringResource(R.string.allow_tool), style = MaterialTheme.typography.titleLarge)
            Spacer(Modifier.height(6.dp))
            Text(req.title, style = MaterialTheme.typography.titleMedium,
                color = MaterialTheme.colorScheme.primary)
            if (req.detail.isNotBlank()) {
                Spacer(Modifier.height(4.dp))
                // Cap + internally scroll: a big tool call's rawInput (e.g. a large `write` body)
                // must never grow this box past a fixed height, or it pushes the allow/deny buttons
                // below the sheet's visible/reachable area on the phone.
                Surface(color = MaterialTheme.colorScheme.surfaceVariant, shape = RoundedCornerShape(8.dp),
                    modifier = Modifier.fillMaxWidth()) {
                    Text(req.detail, style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier
                            .heightIn(max = 220.dp)
                            .verticalScroll(rememberScrollState())
                            .padding(10.dp))
                }
            }
            Spacer(Modifier.height(16.dp))
            req.options.forEach { opt ->
                val reject = opt.kind.startsWith("reject")
                if (reject) {
                    OutlinedButton(onClick = { onChoose(opt.optionId) },
                        modifier = Modifier.fillMaxWidth().padding(vertical = 3.dp)) { Text(prettyOption(opt.label)) }
                } else {
                    Button(onClick = { onChoose(opt.optionId) },
                        modifier = Modifier.fillMaxWidth().padding(vertical = 3.dp)) { Text(prettyOption(opt.label)) }
                }
            }
        }
    }
}

/** Form elicitation (MCP/ACP): a tool asked for structured input. Renders the requested
 *  schema as native controls — switch for booleans, choice chips for enums, keyboard-typed
 *  fields otherwise — and answers accept/decline/cancel. Dismissing the sheet cancels. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ElicitationSheet(
    e: AcpEvent.Elicitation,
    onAccept: (Map<String, kotlinx.serialization.json.JsonPrimitive>) -> Unit,
    onDecline: () -> Unit,
    onCancel: () -> Unit,
) {
    val text = remember(e.requestKey) { mutableStateMapOf<String, String>() }
    val bools = remember(e.requestKey) { mutableStateMapOf<String, Boolean>() }
    ModalBottomSheet(onDismissRequest = onCancel) {
        Column(Modifier.padding(horizontal = 20.dp).padding(bottom = 28.dp)
            .verticalScroll(rememberScrollState())) {
            Text(e.title.ifBlank { "Input requested" }, style = MaterialTheme.typography.titleLarge)
            Spacer(Modifier.height(4.dp))
            Text(e.message, style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant)
            e.fields.forEach { f ->
                Spacer(Modifier.height(14.dp))
                when {
                    f.type == "boolean" -> Row(verticalAlignment = Alignment.CenterVertically) {
                        Switch(checked = bools[f.name] ?: false,
                            onCheckedChange = { bools[f.name] = it })
                        Spacer(Modifier.width(10.dp))
                        Text(f.title, style = MaterialTheme.typography.bodyLarge)
                    }
                    f.options.isNotEmpty() -> Column {
                        Text(f.title + if (f.required) " *" else "",
                            style = MaterialTheme.typography.labelLarge)
                        Spacer(Modifier.height(6.dp))
                        Row(Modifier.horizontalScroll(rememberScrollState()),
                            horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                            f.options.forEach { c ->
                                FilterChip(selected = text[f.name] == c.value,
                                    onClick = { text[f.name] = c.value },
                                    label = { Text(c.label) })
                            }
                        }
                    }
                    else -> OutlinedTextField(
                        text[f.name] ?: "", { text[f.name] = it }, singleLine = true,
                        label = { Text(f.title + if (f.required) " *" else "") },
                        supportingText = if (f.description.isNotBlank())
                            ({ Text(f.description) }) else null,
                        keyboardOptions = if (f.type == "number" || f.type == "integer")
                            KeyboardOptions(keyboardType = KeyboardType.Number)
                        else KeyboardOptions.Default,
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
            }
            Spacer(Modifier.height(18.dp))
            val complete = e.fields.filter { it.required }.all { f ->
                if (f.type == "boolean") true else !text[f.name].isNullOrBlank()
            }
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                TextButton(onClick = onDecline) { Text(stringResource(R.string.decline)) }
                Spacer(Modifier.width(8.dp))
                Button(enabled = complete, onClick = {
                    val values = buildMap {
                        e.fields.forEach { f ->
                            when {
                                f.type == "boolean" ->
                                    put(f.name, kotlinx.serialization.json.JsonPrimitive(bools[f.name] ?: false))
                                f.type == "number" || f.type == "integer" ->
                                    text[f.name]?.toDoubleOrNull()?.let { n ->
                                        if (f.type == "integer")
                                            put(f.name, kotlinx.serialization.json.JsonPrimitive(n.toLong()))
                                        else put(f.name, kotlinx.serialization.json.JsonPrimitive(n))
                                    }
                                else -> text[f.name]?.takeIf { it.isNotBlank() }
                                    ?.let { put(f.name, kotlinx.serialization.json.JsonPrimitive(it)) }
                            }
                        }
                    }
                    onAccept(values)
                }) { Text(stringResource(R.string.submit)) }
            }
        }
    }
}

/** The tools-for-this-chat list body, shared by the composer "+" sheet. */
@Composable
private fun ToolManagementBody(cm: ConnectionManager, title: String = "Tools for this chat") {
    Column(Modifier.padding(horizontal = 20.dp).padding(bottom = 28.dp).verticalScroll(rememberScrollState())) {
        Text(title, style = MaterialTheme.typography.titleLarge)
        Spacer(Modifier.height(4.dp))
        Text(stringResource(R.string.session_only_note),
            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.outline)
        Spacer(Modifier.height(12.dp))
        // A federated session's rows come from the PEER's own session extension list
        // (plus any detached this session, so they can be re-enabled). The peer's global
        // catalog isn't queryable — config/extensions/list has no session id to route on —
        // so extensions not attached to the remote session simply don't appear.
        val remotePeer = ConnectionManager.roamPeer(cm.currentSession.value)
        val rows = if (remotePeer != null)
            (cm.sessionExtensionInfos.value + cm.detachedPeerExts.value).sortedBy { it.name }
        else cm.extensions.value
        if (remotePeer != null) {
            Text("This chat lives on $remotePeer — changes apply there, and only " +
                "extensions already in the chat are listed.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.outline)
            Spacer(Modifier.height(12.dp))
        }
        if (rows.isEmpty()) {
            Text(stringResource(R.string.loading), style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.outline)
        }
        val active = cm.sessionExtensionNames.value.toSet()
        rows.forEach { e ->
            val isOn = e.name in active
            Row(Modifier.fillMaxWidth().padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f).padding(end = 12.dp)) {
                    Text(e.name, style = MaterialTheme.typography.bodyLarge)
                    if (e.description.isNotBlank())
                        Text(e.description, style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.outline, maxLines = 2)
                }
                // Every switch is live, bundled or not. This used to be
                // `enabled = !(e.bundled && isOn)`, meant to stop a core extension being stripped
                // -- but 8 of the 12 enabled extensions are bundled, so most of the sheet was
                // permanently greyed and read as broken. goose itself imposes no such rule:
                // session/extensions/remove drops a bundled extension for this session happily,
                // and the next new chat starts from config.yaml again, so the blast radius is one
                // conversation.
                Switch(
                    checked = isOn,
                    onCheckedChange = { on -> cm.toggleSessionExtension(e, on) },
                )
            }
            if (isOn) ToolList(cm, e, cm.sessionTools.value[e.name].orEmpty().toSet()) {
                cm.setSessionTools(e, it)          // this chat only
            }
            HorizontalDivider()
        }
    }
}

/** The "+" composer sheet (Claude-style): attach squares on top, then the tool-approval
 *  mode, then the tools-for-this-chat list. Replaces the old mode pill + attach button. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ComposerSheet(
    cm: ConnectionManager,
    onCamera: () -> Unit,
    onPhotos: () -> Unit,
    onFiles: () -> Unit,
    onDismiss: () -> Unit,
) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(Modifier.padding(horizontal = 20.dp).padding(bottom = 28.dp)) {
            // Attach squares, Claude-style: three equal tiles across the top.
            Row(Modifier.fillMaxWidth()) {
                listOf(
                    Triple("Camera", Icons.Filled.PhotoCamera, onCamera),
                    Triple("Photos", Icons.Filled.Image, onPhotos),
                    Triple("Files", Icons.Filled.InsertDriveFile, onFiles),
                ).forEach { (label, icon, action) ->
                    Surface(
                        shape = RoundedCornerShape(14.dp),
                        color = MaterialTheme.colorScheme.surfaceVariant,
                        modifier = Modifier.weight(1f).padding(end = 8.dp).clickable { action() },
                    ) {
                        Column(
                            Modifier.fillMaxWidth().padding(vertical = 14.dp),
                            horizontalAlignment = Alignment.CenterHorizontally,
                        ) {
                            Icon(icon, contentDescription = label, tint = MaterialTheme.colorScheme.primary)
                            Spacer(Modifier.height(6.dp))
                            Text(label, style = MaterialTheme.typography.labelMedium)
                        }
                    }
                }
            }
            Spacer(Modifier.height(16.dp))
            HorizontalDivider()
            Spacer(Modifier.height(12.dp))

            // Tool-approval mode: how much the agent may do without asking, this chat.
            // Compact dropdown, not a stack of blocks — the sheet is about attach +
            // tools; the mode is one knob.
            val modeOpt = cm.config.value.firstOrNull { it.id == "mode" }
            ModeDropdown(modeOpt, cm::setOption)
            Spacer(Modifier.height(8.dp))
            HorizontalDivider()
            Spacer(Modifier.height(8.dp))

            // Tools for this chat (session-scoped toggles + per-extension tool lists).
            LaunchedEffect(Unit) {
                if (cm.extensions.value.isEmpty()) cm.loadExtensions()
                cm.refreshSessionSheet()
                // A peer session's explicit calls race its session/load replay —
                // one fetch can return empty (the SDK's routing settles after
                // the reply). Re-ask shortly; a second empty is presumed real.
                if (cm.roamStatus.values.any { it == "ready" }) {
                    kotlinx.coroutines.delay(1500)
                    cm.refreshSessionSheet()
                }
            }
            ToolManagementBody(cm, title = "Tools")
            Spacer(Modifier.height(4.dp))
            HorizontalDivider()
            Spacer(Modifier.height(8.dp))
            // Compact conversation — was in the Tune panel; the + sheet is its home now.
            OutlinedButton(onClick = { cm.compact() }, enabled = !cm.compacting.value,
                modifier = Modifier.fillMaxWidth()) {
                if (cm.compacting.value) {
                    CircularProgressIndicator(modifier = Modifier.size(16.dp), strokeWidth = 2.dp)
                    Spacer(Modifier.width(8.dp))
                }
                Text(if (cm.compacting.value) "Compacting…" else "Compact conversation")
            }
        }
    }
}

/** Best-effort guess whether a model can accept images, to warn before hanging a text-only model. */
private fun isLikelyVisionModel(model: String): Boolean {
    val s = model.lowercase()
    return listOf(
        "gemini", "claude", "gpt-4o", "gpt-4.1", "gpt-5", "o3", "o4-", "llava", "vision",
        "-vl", "qwen2-vl", "qwen2.5-vl", "qwen3-vl", "pixtral", "minicpm", "internvl",
        "gemma-3", "gemma3", "mmproj", "molmo", "phi-3.5-vision", "phi-4-multimodal", "llama-3.2",
    ).any { s.contains(it) }
}

private fun prettyOption(raw: String) = when (raw) {
    "allow_once" -> "Allow once"
    "allow_always" -> "Always allow"
    "reject_once" -> "Reject"
    "reject_always" -> "Always reject"
    else -> raw.replace('_', ' ').replaceFirstChar { it.uppercase() }
}

// ---- Sessions ---------------------------------------------------------------
