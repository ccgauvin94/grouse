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

/** Every sheet and dialog the chat can raise. The flags are owned by [ChatScreen]; each
 *  dismissal is an explicit callback so the children never write parent state directly. */
@Composable
internal fun ChatOverlays(
    cm: ConnectionManager,
    ctx: Context,
    ui: ChatUiState,
    showActivity: Boolean,
    showChatMenu: Boolean,
    showComposer: Boolean,
    showModelSheet: Boolean,
    showVisionWarn: Boolean,
    showTurnLog: Boolean,
    showSchedule: Boolean,
    onDismissActivity: () -> Unit,
    onDismissChatMenu: () -> Unit,
    onChatMenuActivity: () -> Unit,
    onCamera: () -> Unit,
    onPhotos: () -> Unit,
    onFiles: () -> Unit,
    onDismissComposer: () -> Unit,
    onComposerActivity: () -> Unit,
    onDismissModelSheet: () -> Unit,
    onDismissVisionWarn: () -> Unit,
    onConfirmVisionSend: () -> Unit,
    onDismissTurnLog: () -> Unit,
    onDismissSchedule: () -> Unit,
) {
    val debugTools = ui.debugTools
    val currentModel = ui.currentModel
    val briefingAgo = ui.briefing.ago
    val briefingHealthy = ui.briefing.healthy
    val briefingLate = ui.briefing.late
    val lastBriefing = ui.briefing.lastAt
    if (showActivity) {
        ActivitySheet(cm, onDismiss = onDismissActivity)
    }

    // The "…" menu. session/list only returns chats that have messages, so a brand-new
    // chat is not in cm.sessions yet — build a row from the id rather than come up empty.
    if (showChatMenu) {
        val sid = cm.currentSession.value
        val info = cm.sessions.value.firstOrNull { it.sessionId == sid }
            ?: sid?.let {
                SessionInfo(it, cm.currentChatTitle ?: ctx.getString(R.string.new_chat), "", 0, "")
            }
        if (info != null) SessionActionsDialog(
            cm, info,
            onDone = onDismissChatMenu,
            onActivity = onChatMenuActivity,
            // The Assistant thread is not a chat you rename or archive away: notify and the
            // scheduler find it by id and by the exact title "Assistant".
            sessionActions = !cm.onAssistant,
        )
    }

    if (showComposer) {
        ComposerSheet(cm,
            // Every tile closes the sheet first (ComposerSheet does it): each
            // opens a whole other app, and a sheet that covers the input hides
            // the thumbnail/attachment the user just picked (grouse#17).
            onCamera = onCamera,
            onPhotos = onPhotos,
            onFiles = onFiles,
            onDismiss = onDismissComposer,
            onActivity = onComposerActivity,
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
            onDismiss = onDismissModelSheet,
        )
    }

    if (showVisionWarn) AlertDialog(
        onDismissRequest = onDismissVisionWarn,
        title = { Text(stringResource(R.string.model_may_not_support_images)) },
        text = { Text("“$currentModel” probably can't read images and may get stuck on them — " +
            "even later text messages. If that happens, start a New chat.\n\n" +
            "Sending anyway won't ask again for this model.") },
        confirmButton = { TextButton(onClick = {
            onConfirmVisionSend()
        }) { Text(stringResource(R.string.send_anyway)) } },
        dismissButton = { TextButton(onClick = onDismissVisionWarn) { Text(stringResource(R.string.cancel)) } },
    )

    if (showTurnLog && debugTools) {
        TurnLogDialog(cm, onDismissTurnLog)
    }

    if (showSchedule) AlertDialog(
        onDismissRequest = onDismissSchedule,
        confirmButton = { TextButton(onClick = onDismissSchedule) { Text(stringResource(R.string.close)) } },
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
}
