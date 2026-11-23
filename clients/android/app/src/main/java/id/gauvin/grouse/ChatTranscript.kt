// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import id.gauvin.grouse.ui.theme.GrouseShapes
import id.gauvin.grouse.ui.theme.statusColors
import android.content.Context
import android.content.Intent
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
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.core.MutableTransitionState
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.clickable
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material.icons.filled.ArrowUpward
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.ErrorOutline
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeJoin
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
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.interaction.DragInteraction
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material.icons.filled.Download
import androidx.compose.material.icons.automirrored.filled.OpenInNew
import androidx.compose.material.icons.filled.Mic
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Chat
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.ArrowDropDown
import androidx.compose.material.icons.filled.Build
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.FactCheck
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.Image
import androidx.compose.material.icons.filled.InsertDriveFile
import androidx.compose.material.icons.filled.PhotoCamera
import androidx.compose.material.icons.filled.Menu
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Psychology
import androidx.compose.material.icons.filled.PushPin
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
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.height
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.mapSaver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.toMutableStateMap
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
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.navigation.NavController
import com.halilibo.richtext.markdown.Markdown
import com.halilibo.richtext.ui.material3.RichText
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.contentOrNull
import androidx.compose.runtime.snapshots.SnapshotStateMap

/** The transcript and everything that moves its end: the follower, the stream freeze, the
 *  jump button and the scroll-back loader. `listState` and `follow` are hoisted to
 *  [ChatScreen] (the reading position is saved there), but every read of `cm.messages`
 *  now lives in this child, so a streamed token no longer recomposes the header/composer. */
@Composable
internal fun ColumnScope.ChatTranscript(
    cm: ConnectionManager,
    listState: LazyListState,
    sessionKey: String,
    follow: Boolean,
    onFollowChange: (Boolean) -> Unit,
    savedPositions: SnapshotStateMap<String, String>,
    ui: ChatUiState,
    pins: String?,
) {
    val ctx = LocalContext.current
    val link = ui.link
    val debugTools = ui.debugTools
    val scope = rememberCoroutineScope()
    val chatRows = groupChatItems(cm.messages)
    // "At bottom" = the last item is visible; drives autoscroll + the jump-to-bottom button.
    // With reverseLayout the bottom is index 0; we're at the bottom when it's fully shown.
    val atBottom by remember {
        derivedStateOf {
            listState.firstVisibleItemIndex == 0 && listState.firstVisibleItemScrollOffset == 0
        }
    }

    // Follow decisions are keyed on a FINGER, not on scrolling: this code scrolls the list too
    // (the pin, and the jump-to-bottom button), and a programmatic scroll that read as "the
    // reader took over" would unpin itself and then never recover.
    var fingerOnList by remember(sessionKey) { mutableStateOf(false) }

    // The FREEZE: the text a live reply is held at, while the reader is not following it.
    //
    // A row that grows is the one thing that can drag a reading position with it. The newest
    // bubble is anchored at the bottom of this reversed list, so every token it gains pushes
    // everything above it up the screen — including a reader who scrolled up mid-reply, and a
    // reader who is simply reading the first lines of a reply that has grown past the screen
    // (which is why the follower stops following at that crossing rather than waiting for a
    // drag). Nothing is lost: the turn keeps arriving in the core, and the row catches up —
    // jumping to the newest text, with the list re-pinning — the moment the reader asks for
    // it. The held text lives HERE rather than in the bubble because the bubble is disposed
    // when it scrolls out of view, and a re-created one would silently reanimate mid-read.
    var heldStream by remember(sessionKey) {
        mutableStateOf<Pair<Long, String>?>(null)
    }
    LaunchedEffect(sessionKey, follow, cm.streamingRowId.value) {
        val id = cm.streamingRowId.value
        heldStream = when {
            // Back at the bottom: the row catches up and the follower re-pins.
            follow -> null
            // No reply in flight and nothing held: an idle chat needs no freeze, so scrolling
            // back through an old conversation keeps rendering it in full.
            id == null -> heldStream
            // A row that was already captured stays as captured — re-reading it here would
            // re-freeze at the newest token and defeat the point.
            heldStream?.first == id -> heldStream
            // Otherwise capture NOW, in the effect and not in composition: `messages` is the
            // whole transcript, and subscribing the screen to it would recompose everything
            // once per token.
            else -> cm.messages.firstOrNull { it.id == id }?.let { it.id to it.text }
        }
    }

    // Scroll-back: ask the core for older items when the oldest end nears. With
    // reverseLayout the oldest is the LAST index; the batch is prepended to the
    // model (which is the far end of the reversed layout) and the LazyColumn keeps
    // its position because every row is keyed by its stable id.
    val shouldLoadOlder by remember {
        derivedStateOf {
            val info = listState.layoutInfo
            val last = info.visibleItemsInfo.lastOrNull()?.index ?: -1
            cm.itemHasOlder.value && info.totalItemsCount > 0 &&
                last >= info.totalItemsCount - 3
        }
    }
    LaunchedEffect(shouldLoadOlder) { if (shouldLoadOlder) cm.loadOlder() }

    // The follower: one effect owns everything that moves the end.
    //
    // While following, rows arriving/leaving, the newest row growing as it lays out and the
    // viewport resizing all move the end without any scroll, so any of them re-pins. Not keyed
    // on `messages` content: that recomposed the whole screen per token.
    //
    // …until the live reply outgrows the SCREEN. Four lines into a long answer, "pinned to the
    // bottom" and "keeping the reader's line still" are opposites: every token pushes the text
    // they are reading off the top, which is what "it's still scrolling the replies" was.
    //
    // So the crossing of one viewport is a latch: the follower stops following, and the hold in
    // `heldStream` freezes the row at the text it has — the reader's line cannot move because
    // the row stops growing, and the jump-to-bottom button appears (a held row counts as "not
    // at the bottom") carrying the busy dot, which is how the rest of the reply is reachable.
    // Tapping it follows again, and there is no re-latch until the next reply, because the
    // latch fires on the CROSSING rather than on "is over one screen".
    LaunchedEffect(listState) {
        var overScreen = false
        snapshotFlow {
            val info = listState.layoutInfo
            val id = cm.streamingRowId.value
            val row = if (id == null) null else info.visibleItemsInfo.firstOrNull { it.key == id }
            EndFrame(info.totalItemsCount, info.viewportSize.height, id, row?.size ?: -1)
        }.collect { f ->
            if (f.streamId == null) {
                overScreen = false
                if (follow && !listState.isScrollInProgress) listState.scrollToItem(0)
                return@collect
            }
            val nowOverScreen = f.viewport > 0 && f.size >= f.viewport
            val crossed = crossedStreamingViewport(f, overScreen)
            overScreen = nowOverScreen
            if (follow && crossed) {
                onFollowChange(false)   // freezes the row via heldStream, below
                return@collect
            }
            if (follow && !listState.isScrollInProgress) listState.scrollToItem(0)
        }
    }
    // What decisions follow is the SETTLE of a finger's scroll, not the start: at the newest
    // row means keep following, a drag that stopped anywhere above it means don't.
    LaunchedEffect(listState) {
        snapshotFlow { listState.isScrollInProgress }.collect { moving ->
            if (!moving && fingerOnList) {
                fingerOnList = false
                onFollowChange(atBottom)
            }
        }
    }
    // A FINGER on the list stops following at once, settle or no settle. A drag lasts about a
    // second, and a reply that gained a line during it made the follower's own emission fire
    // while `follow` was still true — snapping the reader back to the end they were leaving.
    // It is also what arms the text hold below (`heldStream`), so the bubble freezes as the
    // reader takes over rather than racing them.
    LaunchedEffect(listState) {
        listState.interactionSource.interactions.collect { i ->
            if (i is DragInteraction.Start) {
                fingerOnList = true
                onFollowChange(false)
            }
        }
    }
    // Opening a session with no saved place (or swapping in a fresh transcript after a replay)
    // starts at the newest row. A RETURNING reader is restored BEFORE the follower is armed:
    // arming it first would let the next layout frame pin them to the bottom of the chat they
    // were reading halfway up. The follower still owns the pinning during backfill (600ms a
    // page); this only decides whether it is armed at all. Identical Rebuilds never touch
    // `messages`, so a plain re-render still preserves the reading position.
    LaunchedEffect(sessionKey, cm.replayDoneTick.value) {
        val saved = ReadingPosition.decode(savedPositions[sessionKey])
        if (saved == null) {
            listState.scrollToItem(0)
            onFollowChange(true)
        } else {
            listState.scrollToItem(saved.index, saved.offset)
            onFollowChange(saved.atBottom)
        }
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
                                Text(
                                    if (cm.replayTotal.value > 0)
                                        "${cm.replayProgress.value} / ${cm.replayTotal.value} messages"
                                    else
                                        "${cm.replayProgress.value} messages",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.outline)
                                // A reload rebuilds the whole history: say how far
                                // along it is instead of leaving a spinner to stand
                                // in for minutes. Thin, and gone the moment it ends.
                                if (cm.replayTotal.value > 0) {
                                    Spacer(Modifier.height(6.dp))
                                    LinearProgressIndicator(
                                        progress = { cm.replayProgress.value.toFloat() /
                                            cm.replayTotal.value.toFloat() },
                                        modifier = Modifier.width(180.dp).height(3.dp),
                                        strokeCap = androidx.compose.ui.graphics.StrokeCap.Round,
                                    )
                                }
                            }
                        }
                    } else if (cm.messages.isEmpty() && !cm.busy.value) {
                        Column(
                            Modifier.fillMaxSize().padding(32.dp),
                            horizontalAlignment = Alignment.CenterHorizontally,
                            verticalArrangement = Arrangement.Center
                        ) {
                            // A chat that FAILED to open must say so: the core
                            // substitutes no session (grouse#19), so the copy
                            // below would read as "this chat is empty" over a
                            // thread whose contents never loaded.
                            val failure = cm.openFailed.value
                            if (failure != null) {
                                Icon(Icons.Filled.ErrorOutline, contentDescription = null,
                                    modifier = Modifier.size(48.dp), tint = MaterialTheme.colorScheme.error)
                                Spacer(Modifier.height(12.dp))
                                Text(stringResource(R.string.open_failed_title),
                                    style = MaterialTheme.typography.titleMedium)
                                Spacer(Modifier.height(4.dp))
                                Text(failure, style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.outline, textAlign = TextAlign.Center)
                            } else {
                                Icon(Icons.Filled.Psychology, contentDescription = null,
                                    modifier = Modifier.size(56.dp), tint = MaterialTheme.colorScheme.primary)
                                Spacer(Modifier.height(12.dp))
                                Text(
                                    when (link) {
                                        LinkState.Down -> "Not connected"
                                        LinkState.Retrying -> "Reconnecting…"
                                        LinkState.Connected -> "Ask Grouse anything"
                                    },
                                    style = MaterialTheme.typography.titleMedium)
                                Spacer(Modifier.height(4.dp))
                                Text(stringResource(R.string.wired_up_hint),
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.outline, textAlign = TextAlign.Center)
                            }
                        }
                    } else {
                        // reverseLayout: index 0 renders at the BOTTOM. Typing indicator first (very
                        // bottom), then messages newest→oldest upward. Bottom is always index 0, so
                        // "open at bottom", "jump to bottom", and streaming-stays-pinned are trivial.
                        LazyColumn(state = listState, reverseLayout = true, modifier = Modifier.fillMaxSize()) {
                            if (cm.busy.value) item {
                                // The readout only fills in when the turn has gone
                                // quiet (see ConnectionManager.turnDiag). Gated with the
                                // rest of the debug surface: with the switch off nothing
                                // polls and the indicator is the plain typing dots.
                                val diag = if (debugTools) turnStateReadout(cm) else ""
                                TypingIndicator(diag)
                            }
                            // Consecutive tool calls collapse into one dropdown (goose often fires a run of
                            // 5-10 shell/read calls back to back — a wall of individual chips otherwise).
                            // Grouped on the forward list so adjacency is chronological, then reversed for
                            // display like the flat list was. NOT remembered on messages.size: the streaming
                            // assistant message updates via .copy() (new object, same id, size unchanged), so
                            // a size-keyed cache would go stale mid-stream. Grouping is O(#messages) — cheap.
                            val grouped = chatRows.asReversed()
                            // key on the stable id of the group's first message so a growing tool-call run
                            // (or the streaming assistant bubble) reuses its composition instead of rebuilding;
                            // i==0 is the newest item → mark an assistant Msg streaming so it renders as plain
                            // text until the turn finishes (skips the per-token Markdown re-parse).
                            itemsIndexed(grouped, key = { _, item -> item.firstId }) { i, item ->
                                when (item) {
                                    // `live` is what lets a running step's chip pulse: it is this
                                    // CHAT's turn, and a chip in a finished chat must not animate.
                                    is ChatItem.Tools -> ToolChipGroup(item.items, live = cm.busy.value)
                                    // A file the SERVER shared draws as a card: name,
                                    // type and size came with the tool call, the bytes
                                    // did not (a blob in a tool result would be stored in
                                    // the session and replayed on every open). Tapping
                                    // fetches it; tapping a fetched one opens it.
                                    is ChatItem.Msg -> if (item.m.role == "file") {
                                        SharedFileRow(cm, item.m, ctx)
                                    } else {
                                        val pinned = item.m.role == "mcpapp" &&
                                            item.m.appKey.isNotEmpty() && item.m.appKey == pins
                                        // Held still while the reader is scrolled away from the
                                        // bottom — see heldStream above. Matched on the id so a
                                        // NEW reply never inherits the old row's text.
                                        val held = heldStream
                                            ?.takeIf { it.first == item.m.id }?.second
                                        MessageBubble(
                                            item.m,
                                            // Only the bubble the core is writing into renders
                                            // progressively (settled paragraphs as Markdown, the
                                            // tail as plain text); the previous reply keeps its
                                            // finished Markdown while the next one starts (see
                                            // ConnectionManager.streamingRowId). A HELD row keeps
                                            // that progressive render too, including after its
                                            // turn ends: switching it to finalized Markdown under
                                            // a reader's eyes would reflow the text they are on by
                                            // everything it grew while they were away.
                                            streaming = held != null ||
                                                (item.m.role == "assistant" &&
                                                    item.m.id == cm.streamingRowId.value),
                                            heldText = held,
                                            live = cm.busy.value,
                                            // Each message carries its own stats now, so an older reply
                                            // long-presses to ITS numbers rather than the latest turn's.
                                            usage = item.m.usage,
                                            pinned = pinned,
                                            onPin = { cm.pinApp(item.m.appKey) },
                                            onUnpin = { cm.unpinApp() },
                                        )
                                    }
                                }
                            }
                            // The top of the scroll-back is where the CORE goes to the
                            // wire: past what this device has already fetched, a batch
                            // is a round trip (docs/TRANSCRIPT_MODEL.md, "Windows"). Say
                            // so, or the list just stops with nothing to show for the
                            // gesture. Rendered AFTER the rows, which reverseLayout puts
                            // at the visual top.
                            if (cm.loadingOlderItems.value) item(key = "older-loading") {
                                Row(
                                    modifier = Modifier.fillMaxWidth().padding(vertical = 12.dp),
                                    horizontalArrangement = Arrangement.Center,
                                    verticalAlignment = Alignment.CenterVertically,
                                ) {
                                    CircularProgressIndicator(Modifier.size(13.dp), strokeWidth = 1.5.dp)
                                    Spacer(Modifier.width(8.dp))
                                    Text(
                                        "loading older messages…",
                                        style = MaterialTheme.typography.bodySmall,
                                        color = MaterialTheme.colorScheme.outline,
                                    )
                                }
                            }
                        }
                        if (!atBottom || heldStream != null) Box(Modifier.align(Alignment.BottomEnd).padding(10.dp)) {
                            SmallFloatingActionButton(
                                // Follow first: the animation lands on the end as it is
                                // measured NOW, and the follower finishes the trip if a row
                                // measured after that moved it again.
                                // Asking for the newest line resumes following: this is the
                                // reader choosing to watch the reply arrive again.
                                onClick = {
                                    onFollowChange(true)
                                    scope.launch { listState.animateScrollToItem(0) }
                                },
                            ) { Icon(Icons.Filled.KeyboardArrowDown, contentDescription = "scroll to bottom") }
                            // A streaming row deliberately stops updating while the reader is
                            // away from the bottom, so say that the turn is still arriving down
                            // there — otherwise a bubble that stopped growing reads as one that
                            // stalled.
                            if (cm.busy.value) Surface(
                                color = MaterialTheme.colorScheme.primary,
                                shape = CircleShape,
                                modifier = Modifier.size(9.dp).align(Alignment.TopEnd),
                            ) {}
                        }
                    }
                }


                // Pull-up reload handle: a dropped turn can leave the local view
                // partial, and there is otherwise no way to force a re-fetch. When
                // already at the bottom the ARMED pull reloads FROM THE START; when
                // scrolled back it just tails, because the reader asked to see the
                // end, not to re-download the chat. A dedicated handle (not a
                // list-edge swipe) because the transcript is reverseLayout, where an
                // upward drag is ordinary scroll-back.
                //
                // The armed pull is the FULL reload (grouse#18), matching what the
                // desktop's Reload button means, and it used to be the QUICK one:
                // QUICK is allowed to decide the cache is current and fetch nothing,
                // so the one gesture the phone offers could look exactly like a
                // reload that silently did nothing — reported 2026-11-12 as "full
                // refresh doesn't grab the latest messages, especially from another
                // client". The label under the bar always said "Release to reload",
                // so this is the action it promised. QUICK still runs on every open
                // (and on a reconnect's resume); it needs no gesture of its own.
                val reloadHint = when (link) {
                    LinkState.Down -> "Offline — pull up to reload"
                    LinkState.Retrying -> "Reconnecting… pull up to reload"
                    LinkState.Connected -> null
                }
                ReloadHandle(
                    hint = reloadHint,
                    onPullUp = {
                        if (atBottom) cm.reloadCurrentFromStart()
                        else { onFollowChange(true); scope.launch { listState.animateScrollToItem(0) } }
                    },
                    onTail = { onFollowChange(true); scope.launch { listState.animateScrollToItem(0) } },
                    onReloadFromStart = { cm.reloadCurrentFromStart() },
                )

    ChatDelegateStrip(cm, listState)
}

/** A delegate that is still running but whose row is off screen gets a one-line strip just
 *  above the composer. Tapping it scrolls to the row (past the typing indicator when busy). */
@Composable
internal fun ChatDelegateStrip(cm: ConnectionManager, listState: LazyListState) {
    val scope = rememberCoroutineScope()
                // A delegate runs for minutes with nothing on screen to show it: its row
                // may be scrolled away, or folded inside a collapsed tool run. Show a
                // one-line strip exactly WHILE that is true — never when the row is in
                // view, which would just duplicate what is already in front of you.
                val chatRows = groupChatItems(cm.messages)
                val activeDelegate = cm.messages.lastOrNull {
                    it.role == "tool" && it.status == "in_progress" && it.steps.isNotEmpty()
                }
                val delegateRow = activeDelegate?.let { d ->
                    chatRows.firstOrNull { row ->
                        when (row) {
                            is ChatItem.Msg -> row.m.id == d.id
                            is ChatItem.Tools -> row.items.any { it.id == d.id }
                        }
                    }
                }
                val delegateOnScreen = delegateRow != null &&
                    listState.layoutInfo.visibleItemsInfo.any { it.key == delegateRow.firstId }
                if (activeDelegate != null && !delegateOnScreen) {
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .clickable {
                                val idx = chatRows.asReversed()
                                    .indexOfFirst { it.firstId == delegateRow?.firstId }
                                // reverseLayout index 0 is the typing indicator while a
                                // turn is busy — which it always is with a live delegate.
                                val offset = if (cm.busy.value) 1 else 0
                                if (idx >= 0) scope.launch { listState.animateScrollToItem(idx + offset) }
                            }
                            .padding(start = 4.dp, top = 2.dp, bottom = 2.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        CircularProgressIndicator(Modifier.size(11.dp), strokeWidth = 1.4.dp)
                        Spacer(Modifier.width(8.dp))
                        Text(
                            "subagent · " +
                                (activeDelegate.steps.lastOrNull()?.name?.replace('_', ' ')
                                    ?: "working") +
                                " · ${activeDelegate.steps.size} steps",
                            style = MaterialTheme.typography.labelMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1, overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
}
