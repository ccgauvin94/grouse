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


@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun InstanceScreen(cm: ConnectionManager, nav: NavController) {
    var host by remember { mutableStateOf(cm.store.host) }
    var port by remember { mutableStateOf(cm.store.port) }
    var newKey by remember { mutableStateOf("") }
    var persistent by remember { mutableStateOf(cm.persistent) }
    val ctx = LocalContext.current
    Scaffold(topBar = {
        TopAppBar(
            title = { Text(stringResource(R.string.instance)) },
            navigationIcon = {
                IconButton(onClick = { nav.popBackStack() }) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "back")
                }
            },
        )
    }) { pad ->
        Column(Modifier.padding(pad).padding(horizontal = 16.dp).fillMaxSize()
            .verticalScroll(rememberScrollState())) {
            SettingsSection("Connection") {
                OutlinedTextField(host, { host = it }, label = { Text(stringResource(R.string.host)) }, singleLine = true,
                    modifier = Modifier.fillMaxWidth())
                OutlinedTextField(port, { port = it }, label = { Text(stringResource(R.string.port)) }, singleLine = true,
                    modifier = Modifier.fillMaxWidth().padding(top = 8.dp))
                OutlinedTextField(newKey, { newKey = it }, label = { Text(stringResource(R.string.replace_secret_key)) },
                    singleLine = true, modifier = Modifier.fillMaxWidth().padding(top = 8.dp))
                var wdir by remember { mutableStateOf(cm.store.workingDir) }
                OutlinedTextField(wdir, { wdir = it }, label = { Text(stringResource(R.string.working_directory)) },
                    supportingText = { Text(stringResource(R.string.instance_workdir_hint)) },
                    singleLine = true, modifier = Modifier.fillMaxWidth().padding(top = 8.dp))
                Button(onClick = {
                    val key = newKey.trim().ifBlank { cm.store.secretKey }
                    cm.connect(host.trim(), port.trim(), key, wdir.trim()); nav.popBackStack()
                }, modifier = Modifier.fillMaxWidth().padding(top = 12.dp)) { Text(stringResource(R.string.save_and_reconnect)) }
            }

            SettingsSection("Notifications & background") {
                SettingsSwitchRow("Keep connection alive", persistent) { persistent = it; cm.setPersistent(it) }
                SettingCaption("On: stay connected in the background even when idle (persistent notification, more battery). " +
                    "Off: the connection is still held while one is live or re-dialing — but an idle " +
                    "app releases it. Either way you get a finished-turn notification.")
                HorizontalDivider(Modifier.padding(vertical = 6.dp))
                // The no-notification middle ground: a battery-optimization exemption keeps the
                // process (and its socket) unfrozen in the background WITHOUT a foreground
                // service. OS-granted, OS-revocable, OEM-dependent in how much it helps — but
                // it directly extends how long a background blip stays a non-event.
                val pm = ctx.getSystemService(android.content.Context.POWER_SERVICE)
                    as android.os.PowerManager
                var exempt by remember { mutableStateOf(pm.isIgnoringBatteryOptimizations(ctx.packageName)) }
                val owner2 = LocalLifecycleOwner.current
                DisposableEffect(owner2) {
                    val obs = LifecycleEventObserver { _, e ->
                        if (e == Lifecycle.Event.ON_RESUME)
                            exempt = pm.isIgnoringBatteryOptimizations(ctx.packageName)
                    }
                    owner2.lifecycle.addObserver(obs)
                    onDispose { owner2.lifecycle.removeObserver(obs) }
                }
                SettingsSwitchRow("Unrestricted battery", exempt) { want ->
                    val intent = if (want && !exempt)
                        android.content.Intent(
                            android.provider.Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                            android.net.Uri.parse("package:${ctx.packageName}"))
                    else // revoking has no direct intent; open the app's battery settings
                        android.content.Intent(
                            android.provider.Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
                            android.net.Uri.parse("package:${ctx.packageName}"))
                    runCatching { ctx.startActivity(intent) }
                }
                SettingCaption("Exempts Grouse from the background app freezer so short absences " +
                    "keep the connection — no notification needed. The system dialog asks for consent.")
            }


            Spacer(Modifier.height(28.dp))
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ProvidersScreen(cm: ConnectionManager, nav: NavController) {
    val ctx = LocalContext.current
    // Every row below reads and writes goose's own config over ACP, except speech, which is
    // this device calling LocalAI directly (goose has no TTS, and its dictation transcribes for
    // goose's own UI rather than returning text to a client).
    LaunchedEffect(cm.online.value) {
        if (cm.online.value) {
            cm.readServerConfig(
                "GOOSE_PROVIDER", "GOOSE_MODEL", "GOOSE_FAST_MODEL")
            // Refresh the inventory on entry: providers can be configured server-side while
            // the app is running, and this screen is where that would be noticed.
            cm.refreshProviders()
        }
    }
    fun cfg(k: String) = cm.serverConfig[k].orEmpty()
    Scaffold(topBar = {
        TopAppBar(
            title = { Text(stringResource(R.string.providers)) },
            navigationIcon = {
                IconButton(onClick = { nav.popBackStack() }) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "back")
                }
            },
        )
    }) { pad ->
        Column(Modifier.padding(pad).padding(horizontal = 16.dp).fillMaxSize()
            .verticalScroll(rememberScrollState())) {

            ModelRow(
                title = "Chat",
                caption = "The model new chats start on. The picker above the message box " +
                    "changes the current chat only. A cloud model under provider \"openai\" is " +
                    "sent to the local server and 404s — move both together.",
                enabled = true, onEnabled = null,
                provider = cfg("GOOSE_PROVIDER"),
                onProvider = { cm.setServerConfig("GOOSE_PROVIDER", it) },
                model = cfg("GOOSE_MODEL"),
                onModel = { cm.setServerConfig("GOOSE_MODEL", it) },
                providers = cm.providerChoices(cfg("GOOSE_PROVIDER")),
                modelChoices = cm.knownModels.value.toList(),
            )

            ModelRow(
                title = "Fast",
                caption = "Session naming, compaction and summarising. Off uses the chat model. " +
                    "goose resolves this against whichever provider serves the model NAME, so a " +
                    "name only one provider knows works from any chat — and one nobody knows " +
                    "fails silently back to the chat model.",
                enabled = cfg("GOOSE_FAST_MODEL").isNotBlank(),
                onEnabled = { on -> if (!on) cm.setServerConfig("GOOSE_FAST_MODEL", "") },
                provider = cfg("GOOSE_PROVIDER"),
                onProvider = { cm.setServerConfig("GOOSE_PROVIDER", it) },
                model = cfg("GOOSE_FAST_MODEL"),
                onModel = { cm.setServerConfig("GOOSE_FAST_MODEL", it) },
                providers = cm.providerChoices(cfg("GOOSE_PROVIDER")),
                modelChoices = cm.knownModels.value.toList(),
            )

            SettingsSection("Catalog") {
                SettingsSwitchRow("Show all providers", cm.showAllProviders.value) {
                    cm.setShowAllProviders(it)
                }
                // Without the inventory the pickers can only offer what is already selected.
                // Say so, rather than letting an empty dropdown read as a broken screen.
                if (cm.providers.value.isEmpty()) {
                    SettingCaption(if (cm.online.value)
                        "This server did not return a provider list, so the pickers only show " +
                            "what is already set. Type a provider name to change it."
                        else "Connect to load the provider list.")
                }
                SettingCaption("Off shows only providers set up on your goose. On lists goose's " +
                    "full catalog.")
            }

            Spacer(Modifier.height(28.dp))
        }
    }
}
