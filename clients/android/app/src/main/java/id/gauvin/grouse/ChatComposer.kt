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

/** Everything below the transcript: slash autocomplete, picked-file cards, the delegate
 *  strip slot, queued sends, the advisor card and the text/action box itself. */
@Composable
internal fun ChatComposer(
    cm: ConnectionManager,
    input: String,
    onInputChange: (String) -> Unit,
    onShowComposer: () -> Unit,
    onShowModelSheet: () -> Unit,
    onSend: () -> Unit,
    delegateStrip: @Composable () -> Unit,
) {
    // The same snapshot list the parent hands to cm.send: the cards below are the drafts
    // about to go out, so they must read the list, not a copy of it.
    val attachments = cm.draftAttachments
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
                                    .clickable { onInputChange("/$name ") }.padding(horizontal = 12.dp, vertical = 8.dp),
                                    style = MaterialTheme.typography.bodyMedium)
                            }
                        }
                    }
                }

                // Scrolls sideways rather than clipping: three cards already
                // exceed a phone's width, and a card that is cut off hides the
                // size — which is half the reason it is a card.
                if (attachments.isNotEmpty() || cm.draftFiles.isNotEmpty()) Row(
                    Modifier.padding(vertical = 4.dp).horizontalScroll(rememberScrollState())) {
                    // An image is a pick too, so it gets the same card; its size
                    // is the base64 payload's, which is what will actually go on
                    // the wire (the original bytes were ~3/4 of it).
                    attachments.forEachIndexed { i, img ->
                        AttachmentCard(
                            AttachedFile(
                                "image ${i + 1}",
                                img.mimeType,
                                (img.dataB64.length.toLong() * 3) / 4,
                            ),
                            modifier = Modifier.padding(end = 6.dp).widthIn(max = 240.dp),
                            onRemove = { attachments.removeAt(i) },
                        )
                    }
                    // A picked file shows the same card the sent bubble will:
                    // icon by kind, name, and size, so what is about to go is
                    // identifiable before it is uploaded rather than a name alone.
                    cm.draftFiles.forEachIndexed { i, f ->
                        AttachmentCard(
                            f.toAttachedFile(),
                            modifier = Modifier.padding(end = 6.dp).widthIn(max = 240.dp),
                            onRemove = { cm.draftFiles.removeAt(i) },
                        )
                    }
                }

    delegateStrip()

                // A parked message is a CARD above the composer, not a count: the text stays
                // visible with a "Queued" label, and the user can promote it into the running
                // turn (steer) or drop it. Sending mid-turn parks by default — a steer
                // REDIRECTS the reply being written, so it is always a deliberate tap.
                val queuedSends = cm.queuedSends.value
                if (queuedSends.isNotEmpty()) {
                    val steerable = cm.busy.value && cm.activeRunIdState.value != null
                    val offline = !cm.wireUpForCurrentChat
                    Column(Modifier.fillMaxWidth().padding(top = 4.dp)) {
                        queuedSends.forEach { q ->
                            key(q.id) {
                                QueuedSendCard(
                                    q = q,
                                    canSteer = steerable && q.attachments == 0,
                                    offline = offline,
                                    onSteer = { cm.steerQueued(q.id) },
                                    onCancel = { cm.cancelQueued(q.id) },
                                )
                            }
                        }
                    }
                }

                // The dev advisor's review of the turn that just ended (Settings → Dev).
                // A card above the composer, not a bubble: a review is ABOUT the turn, so
                // putting it in the transcript would replay it into the conversation and
                // send it back to the model as history next turn — the advisor would end
                // up reviewing itself.
                cm.advisorReport.value?.let { (reviewSession, review) ->
                    if (reviewSession == cm.currentSession.value) {
                        AdvisorCard(
                            text = review,
                            onDismiss = cm::dismissAdvisorReport,
                        )
                    }
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
                                    // Mid-turn a send PARKS the message; promoting it into the
                                    // running turn is the queue card's Steer button, so the
                                    // placeholder must not promise a steer.
                                    when {
                                        cm.busy.value -> "Queue message…"
                                        else -> "Message Grouse…"
                                    },
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                            BasicTextField(
                                value = input,
                                onValueChange = onInputChange,
                                textStyle = MaterialTheme.typography.bodyLarge.copy(
                                    color = MaterialTheme.colorScheme.onSurface),
                                cursorBrush = SolidColor(MaterialTheme.colorScheme.primary),
                                maxLines = 8,
                                modifier = Modifier.fillMaxWidth().heightIn(max = 200.dp),
                            )
                        }
                        // The voice state line: the ONLY place the loop reports itself, so a
                        // silent mic never looks like a dead button again. The level bar is the
                        // proof the mic is live even when nothing is being transcribed.
                        val vState = cm.voiceState.value
                        val vNotice = cm.voiceNotice.value
                        if (vState != VoiceState.IDLE || vNotice != null) {
                            Row(Modifier.fillMaxWidth().padding(bottom = 6.dp),
                                verticalAlignment = Alignment.CenterVertically) {
                                when (vState) {
                                    VoiceState.LISTENING -> {
                                        LevelBar(cm.voiceLevel.value)
                                        Spacer(Modifier.width(8.dp))
                                        Text("Listening…", style = MaterialTheme.typography.labelMedium,
                                            color = MaterialTheme.colorScheme.primary)
                                    }
                                    VoiceState.TRANSCRIBING -> {
                                        CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp)
                                        Spacer(Modifier.width(8.dp))
                                        Text("Transcribing…", style = MaterialTheme.typography.labelMedium,
                                            color = MaterialTheme.colorScheme.onSurfaceVariant)
                                    }
                                    VoiceState.SPEAKING -> {
                                        Icon(Icons.Filled.Mic, contentDescription = null,
                                            modifier = Modifier.size(14.dp),
                                            tint = MaterialTheme.colorScheme.primary)
                                        Spacer(Modifier.width(8.dp))
                                        Text("Speaking… talk over me to interrupt",
                                            style = MaterialTheme.typography.labelMedium,
                                            color = MaterialTheme.colorScheme.primary)
                                    }
                                    else -> Text(vNotice ?: "",
                                        style = MaterialTheme.typography.labelMedium,
                                        color = MaterialTheme.colorScheme.error)
                                }
                            }
                        }
                        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                            // "+" pill, left -- Claude-style composer: attach (camera/photos/files),
                            // the tool-approval mode, and the tools-for-this-chat list all live in
                            // the bottom sheet it opens. The MODEL pill stays beside it (the model
                            // you're about to prompt with, worth seeing at a glance while typing).
                            Surface(
                                shape = RoundedCornerShape(20.dp),
                                color = MaterialTheme.colorScheme.surface,
                                modifier = Modifier.clickable { onShowComposer() },
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
                            // `weight(fill = false)`: takes the slack when there is any,
                            // and SHRINKS (ellipsising its label) when there is not — so no
                            // future control in this row can push the send button off the
                            // edge the way the old Plan chip did.
                            ModelPill(modelOpt, cm.knownModels.value, cm::setOption,
                                onOpenSheet = { onShowModelSheet() },
                                modifier = Modifier.weight(1f, fill = false))
                            Spacer(Modifier.weight(1f))
                            // Conversation mode: one tap in, one tap out. A plain Box, not a
                            // Button — a Button's own clickable consumes the press (and a
                            // pointerInput nested inside one silently no-ops; the first cut did).
                            val vs = cm.voiceState.value
                            val on = vs != VoiceState.IDLE
                            Box(
                                modifier = Modifier
                                    .size(42.dp)
                                    .clip(CircleShape)
                                    .background(
                                        when (vs) {
                                            VoiceState.IDLE, VoiceState.TRANSCRIBING ->
                                                MaterialTheme.colorScheme.surface
                                            else -> MaterialTheme.colorScheme.primary
                                        }
                                    )
                                    .clickable(enabled = vs != VoiceState.TRANSCRIBING) { cm.toggleConversation() },
                                contentAlignment = Alignment.Center,
                            ) {
                                if (vs == VoiceState.TRANSCRIBING) {
                                    CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                                } else {
                                    Icon(
                                        Icons.Filled.Mic,
                                        contentDescription =
                                            if (on) "stop conversation" else "start conversation",
                                        modifier = Modifier.size(20.dp),
                                        tint = if (on) MaterialTheme.colorScheme.onPrimary
                                               else MaterialTheme.colorScheme.onSurface,
                                    )
                                }
                            }
                            Spacer(Modifier.width(6.dp))
                            // One circle, two states: stop while a turn runs, send when there is
                            // something to send. Sending mid-turn queues, so the arrow is never
                            // wrong -- it just may not go out immediately.
                            // An image-only message IS a message: the core takes prompt blocks
                            // with no text at all (the desktop already sends one), so the arrow
                            // must light for attachments alone.
                            val canSend = input.isNotBlank() ||
                                attachments.isNotEmpty() || cm.draftFiles.isNotEmpty()
                            FilledIconButton(
                                onClick = { if (canSend) onSend() else if (cm.busy.value) cm.cancel() },
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
                                        canSend -> when {
                                            cm.busy.value -> "queue"
                                            else -> "send"
                                        }
                                        else -> "stop"
                                    },
                                    modifier = Modifier.size(20.dp),
                                )
                            }
                        }
                    }
                }
}
