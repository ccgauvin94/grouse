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

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ChatTopBar(
    cm: ConnectionManager,
    nav: NavController,
    onOpenDrawer: () -> Unit,
    ui: ChatUiState,
    showContextDetail: Boolean,
    onToggleContextDetail: () -> Unit,
    onShowSchedule: () -> Unit,
    onShowActivity: () -> Unit,
    onShowChatMenu: () -> Unit,
    onShowTurnLog: () -> Unit,
) {
    val link = ui.link
    val roamPeer = ui.roamPeer
    val debugTools = ui.debugTools
    val briefingHealthy = ui.briefing.healthy
    val briefingLate = ui.briefing.late
    Column {
        TopAppBar(
            title = {
                Column {
                    Row(verticalAlignment = Alignment.CenterVertically,
                        // fillMaxWidth, and the name COLUMN carries the weight, so the cluster
                        // sits at the far right and the name ellipsizes into what is left.
                        // CentreVertically is against the whole column — a remote chat's
                        // two-line header puts the ring in the middle of both lines instead
                        // of on the first one's baseline.
                        modifier = Modifier.fillMaxWidth()
                            // Long-press the title for the core's turn log: the one
                            // place the client can show WHY a chat thinks it is busy.
                            .pointerInput(debugTools) {
                                // Long-press is the debug gesture only; with the switch
                                // off the title is just the title. Reconnecting is no
                                // longer a tap HERE — see the mark beside the name.
                                if (debugTools) {
                                    detectTapGestures(onLongPress = { onShowTurnLog() })
                                }
                            }) {
                        // The name and the endpoint pill are ONE block, and the header row's
                        // weight lives HERE rather than on the name. Measured against the name
                        // alone, everything to the right centred on the FIRST line and hung
                        // high the moment a remote chat grew a second one — the ring sat on
                        // the title's baseline instead of in the middle of the header.
                        //
                        // Both lines are CENTRED in this column, which is what makes the bar
                        // read like the other assistant app's: the name sits in the middle
                        // with its pill under it rather than hanging off the left margin.
                        // The centre is the column's, not the window's, so it is a few dp
                        // left of true centre whenever the running glyph is also out — the
                        // readouts on the right are part of this slot. Correcting that would
                        // mean drawing the title outside the app bar's own layout, which
                        // costs more than the few dp are worth.
                        Column(Modifier.weight(1f), horizontalAlignment = Alignment.CenterHorizontally) {
                            Row(verticalAlignment = Alignment.CenterVertically,
                                horizontalArrangement = Arrangement.Center,
                                modifier = Modifier.fillMaxWidth()) {
                                // The connection mark, and ONLY when there is something wrong:
                                // the green dot that used to sit here said "fine" on every
                                // glance at a name the user was already reading, so a healthy
                                // wire now draws nothing and the name sits alone. A roam chat
                                // draws nothing here either — its pill two lines below already
                                // carries that endpoint's dot — and the mark that does appear
                                // is where the wire's settings live.
                                if (roamPeer == null) {
                                    LinkProblemIcon(
                                        state = link,
                                        onClick = { nav.navigate("instance") },
                                    )
                                    Spacer(Modifier.width(2.dp))
                                }
                                // Single line + ellipsis: this Row shares the app bar with the
                                // status cluster, so on a compact width the default (titleLarge,
                                // unbounded) title used to wrap to a second line and collide with
                                // the icons. A tighter style plus a hard single-line cap keeps it
                                // readable and self-eliding instead of visually breaking.
                                //
                                // The TITLE is the chat's NAME, full stop. It used to be replaced by
                                // "Reconnecting…" / "Connecting…" / "Offline · tap to reconnect" and
                                // then it carried " · working…" while a turn ran — both cost the user
                                // the name of the chat they were in at exactly the moment they needed
                                // to know where they were. Turn progress is the composer's thinking
                                // line, which does not overwrite anything.
                                Text(
                                    if (cm.onAssistant) "Assistant" else cm.currentChatTitle ?: "Grouse",
                                    style = MaterialTheme.typography.titleMedium,
                                    maxLines = 1,
                                    // No weight here any more: the COLUMN is what carries the header
                                    // row's weight, so a long name still ellipsizes into the space
                                    // the cluster leaves instead of pushing the donut off the bar.
                                    overflow = TextOverflow.Ellipsis,
                                )
                            }
                            // A remote chat names its endpoint UNDER its name, the way the other
                            // app puts "Remote Control" there. It used to be an AssistChip in
                            // `actions`, where it fought the donut for the one line and reported
                            // the wire's state a second time right beside the connection dot.
                            // Tapping it opens the endpoint page — reconnect / disconnect /
                            // remove live there.
                            if (roamPeer != null) {
                                RoamSubtitle(
                                    peer = roamPeer,
                                    status = cm.roamStatus[roamPeer],
                                    onClick = { nav.navigate("roam_endpoint/" + Uri.encode(roamPeer)) },
                                )
                            }
                        }
                        // The right-hand cluster: live work, then the context donut. Both are
                        // readouts about this chat, not part of its name.
                        val running = cm.activity.value.count { it.running }
                        // LIVE work only. It used to read "3 agents" for the life of the chat
                        // and, after a cancel, kept saying it for work that no longer existed
                        // (2026-11-13). It is the background-tasks mark now — drawn the way the
                        // other app draws it, so the header and the menu row are one symbol —
                        // and it is on screen only while something is actually running.
                        if (running > 0) {
                            // The bubble sits OUTSIDE the tap target: a 28dp box, unchanged
                            // from the count-free header, with the live count hung off its
                            // top end. Wrapping the glyph in a second box keeps the ripple
                            // circular and stops the badge from stealing the click.
                            Box(Modifier.size(28.dp)) {
                                Box(
                                    modifier = Modifier
                                        .size(28.dp)
                                        .clip(CircleShape)
                                        .clickable { onShowActivity() },
                                    contentAlignment = Alignment.Center,
                                ) {
                                    BackgroundTasksGlyph(
                                        modifier = Modifier.size(17.dp),
                                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                }
                                BackgroundTasksCount(
                                    count = running,
                                    modifier = Modifier.align(Alignment.TopEnd)
                                        .offset(x = 2.dp, y = (-2).dp),
                                )
                            }
                        }
                        cm.usage.value?.let { u ->
                            if (u.size > 0 && !cm.compacting.value) {
                                if (running > 0) Spacer(Modifier.width(4.dp))
                                // A 24dp target around a 14dp ring: the tap is the only way
                                // into the breakdown, and 14dp is a miss waiting to happen on
                                // a phone held one-handed.
                                Box(
                                    modifier = Modifier
                                        .size(24.dp)
                                        .clickable {
                                            onToggleContextDetail()
                                        },
                                    contentAlignment = Alignment.Center,
                                ) {
                                    ContextRing(
                                        pct = (u.used * 100 / u.size).coerceIn(0, 100),
                                        modifier = Modifier.size(14.dp),
                                    )
                                }
                                // No spacer after the ring: the margin to the "…" beside it is
                                // already built twice over — this Box is 24dp around a 14dp
                                // ring, and the action button that follows carries its own
                                // inset around its glyph. The 6dp that used to sit here read
                                // as a hole in the bar, which is what "move the donut up
                                // against the …" was pointing at.
                            }
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
                if (cm.onAssistant) IconButton(onClick = { onShowSchedule() }) {
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
                // The chat's own "…", where the other app keeps its overflow: background
                // tasks at the head, then the session actions that used to be reachable
                // only by long-pressing this chat's row in the drawer. The roam chip that
                // used to sit here is a subtitle under the chat's name now.
                IconButton(onClick = { onShowChatMenu() }) {
                    Icon(
                        Icons.Filled.MoreVert,
                        contentDescription = stringResource(R.string.chat_menu),
                    )
                }
            }
        )
                        // A session/load replay is streaming into the buffer: count replayed
                        // messages live — a big history can take tens of seconds and a static
                        // spinner looked hung. The counter proves it's advancing. It lives here
                        // rather than in the title: the title is the chat's NAME and no
                        // connection/loading event is allowed to replace it.
                        if (cm.replayActive.value) {
                            Row(verticalAlignment = Alignment.CenterVertically,
                                modifier = Modifier.padding(start = 18.dp, top = 2.dp)) {
                                LinearProgressIndicator(modifier = Modifier.width(40.dp).height(3.dp))
                                Spacer(Modifier.width(6.dp))
                                Text(
                                    if (cm.replayTotal.value > 0)
                                        "Loading… ${cm.replayProgress.value} / ${cm.replayTotal.value}"
                                    else "Loading… ${cm.replayProgress.value}",
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.outline)
                            }
                        } else if (cm.compacting.value) {
                        // Compacting (manual /compact or a server-triggered auto-compact) takes priority
                        // over the usage line — it's transient and explains why the numbers are about to
                        // change. INDETERMINATE: the protocol only ever sends text status lines, never a
                        // numeric percentage, so there's no real fraction to show.
                            Row(verticalAlignment = Alignment.CenterVertically,
                                modifier = Modifier.padding(start = 18.dp, top = 2.dp)) {
                                LinearProgressIndicator(modifier = Modifier.width(40.dp).height(3.dp))
                                Spacer(Modifier.width(6.dp))
                                // goose's own status line ("goose is compacting the
                                // conversation...") when the server armed it; the local
                                // placeholder only for a client-initiated /compact.
                                Text(cm.compactionStatus.value.ifBlank { stringResource(R.string.compacting) },
                                    style = MaterialTheme.typography.labelSmall,
                                    maxLines = 2, overflow = TextOverflow.Ellipsis,
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
                            // The breakdown lives OUTSIDE the usage gate: a server can answer
                            // the report without ever having pushed a usage update (and a roam
                            // peer's custom notifications are the only usage source there).
                            // Keyed to the chat on screen: a roam chat that has not answered yet
                            // must show "measuring", never the previous chat's numbers.
                            val report = cm.contextReport.value
                                ?.takeIf { cm.contextReportSession.value == cm.currentSession.value }
                            // Feedback for the wait ITSELF. Measuring a 600k-token chat takes
                            // seconds, and a tap that changes nothing on screen reads as a tap
                            // that missed — this is the line that says it landed.
                            if (showContextDetail && report == null && cm.contextReportLoading.value) {
                                Text(stringResource(R.string.context_measuring_long),
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.outline,
                                    modifier = Modifier.padding(start = 18.dp, top = 4.dp))
                            }
                            AnimatedVisibility(visible = showContextDetail && report != null,
                                enter = expandVertically() + fadeIn(),
                                exit = shrinkVertically() + fadeOut()) {
                                report?.let {
                                    ContextBreakdown(
                                        it,
                                        loading = cm.contextReportLoading.value,
                                        onRefresh = { cm.requestContextReport(force = true) },
                                    )
                                }
                            }
                        }
    }
}
