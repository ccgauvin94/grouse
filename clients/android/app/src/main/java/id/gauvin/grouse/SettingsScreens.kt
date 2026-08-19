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


/** A titled group of settings rows, grouped visually in a rounded card. */
@Composable
internal fun SettingsSection(title: String, content: @Composable ColumnScope.() -> Unit) {
    Text(title.uppercase(), style = MaterialTheme.typography.labelMedium,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 6.dp, top = 22.dp, bottom = 8.dp))
    Card(
        shape = RoundedCornerShape(16.dp),
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.35f)),
    ) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp), content = content)
    }
}

/** A tappable settings row: label (+ optional subtitle) with a trailing chevron. */
@Composable
internal fun SettingsNavRow(label: String, subtitle: String? = null, onClick: () -> Unit) {
    Row(Modifier.fillMaxWidth().clickable(onClick = onClick).padding(vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text(label, style = MaterialTheme.typography.bodyLarge)
            if (subtitle != null) Text(subtitle, style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.outline)
        }
        Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, contentDescription = null,
            tint = MaterialTheme.colorScheme.outline)
    }
}

/** Explanatory caption under a setting. */
@Composable
internal fun SettingCaption(text: String) {
    Text(text, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.outline,
        modifier = Modifier.padding(bottom = 2.dp))
}

/** A label + trailing switch row. */
@Composable
internal fun SettingsSwitchRow(label: String, checked: Boolean, onChange: (Boolean) -> Unit) {
    Row(Modifier.fillMaxWidth().padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
        Text(label, Modifier.weight(1f))
        Switch(checked = checked, onCheckedChange = onChange)
    }
}


/** Expandable per-extension tool list, shared by Settings (global default, writes config.yaml) and
 *  the in-chat sheet (this session only). `onSave` is the only difference between the two.
 *
 *  The full catalogue is not directly queryable -- goose reports only ALLOWED tools -- so expanding
 *  triggers ConnectionManager.discoverTools, which briefly runs the extension unfiltered in this
 *  session to read the whole set. That is why the list can take a moment to populate the first time,
 *  and why it is cached afterwards. */
@Composable
fun ToolList(cm: ConnectionManager, e: ExtInfo, active: Set<String>,
             onExpand: (() -> Unit)? = null, onSave: (Set<String>) -> Unit) {
    // Only mcp-backed extensions namespace their tools as `name__tool`, so only those can be mapped
    // back to an owner. Builtins (developer -> shell/edit/tree, summon -> delegate) cannot be, and
    // rendering a row for them showed a permanent "0 tools" that opened onto nothing.
    if (!cm.toolsAttributable(e)) return
    var open by remember { mutableStateOf(false) }
    val catalog = cm.catalogOf(e)
    // Local echo so a checkbox responds instantly; the server round-trip refreshes it after.
    var sel by remember(e.name, active) { mutableStateOf(active) }

    Row(Modifier.fillMaxWidth().clickable {
            open = !open
            if (open) (onExpand ?: { cm.discoverTools(e) })()
        }.padding(vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        Icon(if (open) Icons.Filled.KeyboardArrowUp else Icons.Filled.KeyboardArrowDown,
            contentDescription = if (open) "hide tools" else "show tools")
        Spacer(Modifier.width(6.dp))
        Text(
            when {
                catalog != null -> "${sel.size} of ${catalog.size} tools"
                active.isNotEmpty() -> "${active.size} tools"
                else -> "tools"     // list not back yet — don't claim zero
            },
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.outline,
        )
    }
    if (open) {
        if (catalog.isNullOrEmpty()) {
            Text(if (catalog == null) "reading tool list…" else "no tools reported",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.outline,
                modifier = Modifier.padding(start = 30.dp, bottom = 6.dp))
        } else {
            Column(Modifier.padding(start = 30.dp, bottom = 8.dp)) {
                catalog.sorted().forEach { t ->
                    Row(Modifier.fillMaxWidth().padding(vertical = 1.dp),
                        verticalAlignment = Alignment.CenterVertically) {
                        Text(t, Modifier.weight(1f), style = MaterialTheme.typography.bodySmall)
                        Checkbox(checked = t in sel, onCheckedChange = { on ->
                            sel = if (on) sel + t else sel - t
                            onSave(sel)
                        })
                    }
                }
            }
        }
    }
}

/** Unwrap the hosting Activity from a Compose context (needed for the UnifiedPush distributor picker). */
private fun Context.findActivity(): android.app.Activity? {
    var c = this
    while (c is android.content.ContextWrapper) { if (c is android.app.Activity) return c; c = c.baseContext }
    return null
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(cm: ConnectionManager, nav: NavController, onOpenDrawer: () -> Unit) {
    val ctx = LocalContext.current
    Scaffold(topBar = {
        TopAppBar(
            title = { Text(stringResource(R.string.settings)) },
            navigationIcon = {
                IconButton(onClick = onOpenDrawer) { Icon(Icons.Filled.Menu, contentDescription = "menu") }
            }
        )
    }) { pad ->
        Column(Modifier.padding(pad).padding(horizontal = 16.dp).fillMaxSize()
            .verticalScroll(rememberScrollState())) {

            // Status header: connection + current model at a glance.
            Spacer(Modifier.height(4.dp))
            run {
                val model = cm.config.value.firstOrNull { it.id == "model" }?.currentValue
                    ?.takeIf { it.isNotBlank() && it != "current" } ?: "—"
                val isOnline = cm.online.value
                Card(shape = RoundedCornerShape(16.dp)) {
                    Row(Modifier.fillMaxWidth().padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
                        Icon(if (isOnline) Icons.Filled.Check else Icons.Filled.Close, contentDescription = null,
                            tint = if (isOnline) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.error)
                        Spacer(Modifier.width(12.dp))
                        Column(Modifier.weight(1f)) {
                            Text(if (isOnline) "Connected" else cm.status.value.replaceFirstChar { it.uppercase() },
                                style = MaterialTheme.typography.titleSmall)
                            Text("${cm.store.host}:${cm.store.port}  ·  $model",
                                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.outline)
                        }
                        if (!isOnline) TextButton(onClick = { cm.connectSaved() }) { Text(stringResource(R.string.connect)) }
                    }
                }
            }

            SettingsSection("Server") {
                SettingsNavRow("Instance", "${cm.store.host}:${cm.store.port}" +
                    if (cm.online.value) "  ·  connected" else "  ·  offline") {
                    nav.navigate("instance")
                }
                SettingsNavRow("Providers", "Chat model, vision, speech") {
                    nav.navigate("providers")
                }
                SettingsNavRow("Tools", "Extensions and the tools they expose") {
                    nav.navigate("extensions")
                }
                // Only reachable when the feature is switched on below — otherwise this row
                // would lead to a screen configuring jobs the server does not run.
                if (cm.assistantEnabled.value) {
                    SettingsNavRow("Assistant", "The persistent thread and its scheduled jobs") {
                        nav.navigate("assistant_settings")
                    }
                }
            }

            SettingsSection("Assistant") {
                SettingsSwitchRow("Enable assistant", cm.assistantEnabled.value) {
                    cm.setAssistantEnabled(it)
                }
                SettingCaption("Off by default. The assistant is one persistent thread kept fed " +
                    "by scheduled recipes running on the server. Turn it on only if your server " +
                    "has those jobs — without them the thread stays empty and its status reads " +
                    "permanently stale. With it off, this is a plain chat client.")
            }

            SettingsSection("Notifications") {
                var pushOn by remember { mutableStateOf(cm.store.pushEnabled) }
                SettingsSwitchRow("Push notifications", pushOn) { on ->
                    pushOn = on
                    val act = ctx.findActivity()
                    if (on && act != null) Push.enable(act) else Push.disable(ctx)
                }
                SettingCaption("Receive server-pushed briefings and alerts through a UnifiedPush " +
                    "distributor (e.g. NextPush on your Nextcloud) — no FCM and no always-on socket. " +
                    "Turning this on prompts you to pick the distributor, then registers this device.")
                val endpoint = cm.store.pushEndpoint
                if (pushOn && endpoint.isNotBlank()) {
                    SelectionContainer {
                        Text("Endpoint: $endpoint", style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.outline)
                    }
                    SettingCaption("This device's endpoint is published to the server automatically " +
                        "(GROUSE_PUSH_ENDPOINT), so it stays current across reinstalls.")
                } else if (pushOn) {
                    SettingCaption("Waiting for the distributor to issue an endpoint…")
                }
            }

            SettingsSection("Appearance") {
                SettingsSwitchRow("Material You dynamic color", cm.dynamicColor.value) { cm.setDynamicColor(it) }
                SettingCaption("Off uses the built-in goose-green palette.")
            }

            SettingsSection("Security") {
                // Recomputed each recomposition (NOT remembered) so enrolling a biometric while the
                // screen is open reflects immediately. Only affects the caption, not the toggle.
                val bioAvailable = ctx.findActivity()
                    ?.let { it is androidx.fragment.app.FragmentActivity && Biometric.available(it) } ?: false
                var bioLock by remember { mutableStateOf(cm.store.biometricLock) }
                // The switch shows and controls the REAL stored value (not ANDed with availability),
                // so it can always be turned back off — and turning it on when no authenticator is
                // enrolled is harmless (the lock only engages when Biometric.available is true).
                SettingsSwitchRow("Require biometric unlock", bioLock) { on ->
                    bioLock = on; cm.store.biometricLock = on
                }
                SettingCaption("Off by default. When on, opening the app (or reconnecting with the " +
                    "saved key) needs your fingerprint/face or device PIN; takes effect next launch." +
                    if (!bioAvailable) " (No authenticator is enrolled on this device yet, so it " +
                        "won't engage until you add one.)" else "")
            }

            Spacer(Modifier.height(24.dp))
        }
    }
}

// ---- Extensions -------------------------------------------------------------

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ExtensionsScreen(cm: ConnectionManager, nav: NavController) {
    LaunchedEffect(Unit) { cm.loadExtensions() }
    Scaffold(topBar = {
        TopAppBar(
            title = { Text(stringResource(R.string.tools)) },
            navigationIcon = {
                IconButton(onClick = { nav.popBackStack() }) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "back")
                }
            },
            actions = {
                if (cm.extensionsBusy.value)
                    CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                Spacer(Modifier.width(12.dp))
            }
        )
    }) { pad ->
        Column(Modifier.padding(pad).fillMaxSize()) {
            Text(stringResource(R.string.extensions_note),
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.outline,
                modifier = Modifier.padding(16.dp))
            LazyColumn(Modifier.fillMaxSize()) {
                items(cm.extensions.value) { e ->
                    Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
                        verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f).padding(end = 12.dp)) {
                            Text(e.name, style = MaterialTheme.typography.bodyLarge)
                            if (e.description.isNotBlank())
                                Text(e.description, style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.outline, maxLines = 2)
                        }
                        Switch(checked = e.enabled, enabled = !cm.extensionsBusy.value,
                            onCheckedChange = { cm.toggleExtension(e, it) })
                    }
                    if (e.enabled) Box(Modifier.padding(horizontal = 16.dp)) {
                        // Seed the checkboxes from the SAVED global allowlist (e.raw), not from the
                        // current session's tool state. The session reflects whatever this chat
                        // happens to be running (and after a catalogue discovery, the FULL set), so
                        // seeding from it made every box show checked again after a save that had in
                        // fact persisted -- "my pruning didn't save" when config.yaml said otherwise.
                        // Empty allowlist means "all tools", so fall back to the catalogue then.
                        val saved = (e.raw["available_tools"] as? JsonArray)
                            ?.mapNotNull { it.jsonPrimitive.contentOrNull }?.toSet().orEmpty()
                        val active = if (saved.isEmpty())
                            cm.catalogOf(e)?.toSet()
                                ?: cm.sessionTools.value[e.name].orEmpty().toSet()
                        else saved
                        ToolList(cm, e, active) {
                            cm.setDefaultTools(e, it)   // config.yaml; applies to new chats
                        }
                    }
                    HorizontalDivider()
                }
                if (cm.extensions.value.isEmpty() && !cm.extensionsBusy.value) item {
                    Text(stringResource(R.string.couldnt_load_extensions),
                        style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.padding(16.dp))
                }
            }
        }
    }
}

// ---- Model pickers ----------------------------------------------------------

/** Full-width model picker sheet: source (provider), model (per source), and
 *  thinking effort when the current model exposes it (goose 1.46 sends
 *  `thinking_effort` only for models that support extended thinking). Opens
 *  from the model pill; the Tune-panel dropdowns share the same widgets. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ModelSheet(
    options: List<ConfigOption>,
    providers: List<String>,
    knownModels: Set<String>,
    onPick: (String, String) -> Unit,
    onDismiss: () -> Unit,
) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(Modifier.padding(horizontal = 20.dp).padding(bottom = 28.dp)) {
            Text(stringResource(R.string.model_title), style = MaterialTheme.typography.titleLarge)
            Spacer(Modifier.height(12.dp))
            if (options.isEmpty()) {
                Text(stringResource(R.string.loading_model_options), style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.outline)
            } else {
                val byId = options.associateBy { it.id }
                // Source first — the model list is provider-scoped. The provider list comes
                // from ConnectionManager.providerChoices() (the single shared source honoring
                // showAllProviders), never re-derived here (A-10).
                byId["provider"]?.let { opt ->
                    ConfigDropdown(
                        if (providers.isEmpty()) opt
                        else opt.copy(choices = opt.choices.filter {
                            it.value in providers || it.value == opt.currentValue
                        }),
                        onPick,
                    )
                }
                // Models for the CURRENT source.
                byId["model"]?.let { ModelDropdown(it, knownModels, onPick) }
                // Effort, only when the server offers it (model-dependent).
                byId["thinking_effort"]?.let { ConfigDropdown(it, onPick) }
            }
        }
    }
}

/** Compact model pill for the input row — the same slot/shape as the MODE pill
 *  beside it. Lists goose's featured models for the current provider plus the
 *  live-known set (same entries as the Tune panel's ModelDropdown), and a
 *  "Custom model…" escape hatch for ids the catalog doesn't feature. */
@Composable
internal fun ModelPill(
    opt: ConfigOption?,
    knownModels: Set<String>,
    onPick: (String, String) -> Unit,
    onOpenSheet: () -> Unit,
) {
    fun labelFor(v: String) = if (v == "current") "Provider default"
        else opt?.choices?.firstOrNull { it.value == v }?.label ?: v
    val currentLabel = when {
        opt == null || opt.currentValue.isBlank() -> "Provider default"
        else -> labelFor(opt.currentValue)
    }
    Surface(
        shape = RoundedCornerShape(20.dp),
        color = MaterialTheme.colorScheme.surface,
        modifier = Modifier.clickable(enabled = opt != null) { onOpenSheet() },
    ) {
        Row(
            Modifier.padding(horizontal = 12.dp, vertical = 7.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(Icons.Filled.Psychology, contentDescription = null,
                modifier = Modifier.size(16.dp),
                tint = MaterialTheme.colorScheme.onSurface)
            Spacer(Modifier.width(6.dp))
            Text(currentLabel, style = MaterialTheme.typography.labelLarge,
                maxLines = 1, overflow = TextOverflow.Ellipsis,
                modifier = Modifier.widthIn(max = 130.dp))
        }
    }
}

internal fun fmtTokens(n: Int): String = when {
    n >= 1_000_000 -> "%.1fM".format(n / 1_000_000.0)
    n >= 1_000 -> "${n / 1000}k"
    else -> "$n"
}

/** "2h ago" style timestamp from goose's ISO updatedAt; falls back to the raw value. */
internal fun relativeTime(iso: String): String = runCatching {
    val inst = runCatching { java.time.Instant.parse(iso) }
        .recoverCatching { java.time.OffsetDateTime.parse(iso).toInstant() }
        .recoverCatching { java.time.LocalDateTime.parse(iso).toInstant(java.time.ZoneOffset.UTC) }
        .getOrThrow()
    val secs = java.time.Duration.between(inst, java.time.Instant.now()).seconds
    when {
        secs < 60 -> "just now"
        secs < 3600 -> "${secs / 60}m ago"
        secs < 86400 -> "${secs / 3600}h ago"
        secs < 604800 -> "${secs / 86400}d ago"
        else -> iso.take(10)
    }
}.getOrElse { iso.take(16).replace('T', ' ') }

/** One-time hint on the assistant thread explaining what this privileged conversation is. */
@Composable
internal fun AssistantHint(onDismiss: () -> Unit) {
    Surface(color = MaterialTheme.colorScheme.surfaceVariant, shape = RoundedCornerShape(12.dp),
        modifier = Modifier.fillMaxWidth().padding(top = 8.dp)) {
        Row(Modifier.padding(start = 14.dp, top = 10.dp, bottom = 10.dp), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(stringResource(R.string.always_on_assistant), style = MaterialTheme.typography.titleSmall)
                Text(stringResource(R.string.assistant_actions_note), style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            IconButton(onClick = onDismiss) { Icon(Icons.Filled.Close, contentDescription = "dismiss") }
        }
    }
}
