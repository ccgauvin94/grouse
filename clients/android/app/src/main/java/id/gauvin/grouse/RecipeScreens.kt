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

internal enum class CronKind { HOURLY, DAILY, WEEKLY, CUSTOM }

/** A cron expression in the shapes people actually schedule things in.
 *
 *  Not a general cron editor: the field stays for anything this cannot express, and anything it
 *  cannot PARSE opens as Custom rather than being silently rewritten into something close. A
 *  picker that quietly turns an expression you meant into one it understands is worse than a
 *  text box. */
internal data class CronSpec(
    val kind: CronKind,
    val minute: Int = 0,
    val hour: Int = 6,
    val fromHour: Int = 0,
    val toHour: Int = 23,
    val dow: String = "Mon",
    val raw: String = "",
)

private val CRON_DAYS = listOf("Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat")

internal fun parseCron(cron: String): CronSpec {
    val f = cron.trim().split(Regex("\\s+"))
    // goose prefixes a seconds field to a 5-field expression, so accept both spellings.
    val p = when (f.size) { 6 -> f; 5 -> listOf("0") + f; else -> return CronSpec(CronKind.CUSTOM, raw = cron) }
    val (sec, min, hour) = Triple(p[0], p[1], p[2])
    val (dom, mon, dow) = Triple(p[3], p[4], p[5])
    val m = min.toIntOrNull()
    if (sec != "0" || m == null || dom != "*" || mon != "*") return CronSpec(CronKind.CUSTOM, raw = cron)
    return when {
        hour == "*" && dow == "*" -> CronSpec(CronKind.HOURLY, minute = m, fromHour = 0, toHour = 23)
        hour.matches(Regex("\\d+-\\d+")) && dow == "*" -> {
            val (a, b) = hour.split("-").map { it.toInt() }
            CronSpec(CronKind.HOURLY, minute = m, fromHour = a, toHour = b)
        }
        hour.toIntOrNull() != null && dow == "*" ->
            CronSpec(CronKind.DAILY, minute = m, hour = hour.toInt())
        hour.toIntOrNull() != null && CRON_DAYS.any { it.equals(dow, true) } ->
            CronSpec(CronKind.WEEKLY, minute = m, hour = hour.toInt(),
                dow = CRON_DAYS.first { it.equals(dow, true) })
        else -> CronSpec(CronKind.CUSTOM, raw = cron)
    }
}

internal fun buildCron(s: CronSpec): String = when (s.kind) {
    CronKind.HOURLY ->
        if (s.fromHour == 0 && s.toHour == 23) "0 ${s.minute} * * * *"
        else "0 ${s.minute} ${s.fromHour}-${s.toHour} * * *"
    CronKind.DAILY -> "0 ${s.minute} ${s.hour} * * *"
    CronKind.WEEKLY -> "0 ${s.minute} ${s.hour} * * ${s.dow}"
    CronKind.CUSTOM -> s.raw
}

/** Schedule editor: frequency first, then only the fields that frequency needs. */
@Composable
private fun CronEditor(value: String, onChange: (String) -> Unit) {
    var spec by remember(value) { mutableStateOf(parseCron(value)) }
    fun set(next: CronSpec) { spec = next; onChange(buildCron(next)) }

    @Composable
    fun numberPicker(label: String, current: Int, range: IntRange, step: Int = 1,
                     onPick: (Int) -> Unit) {
        var open by remember { mutableStateOf(false) }
        Box {
            SettingsNavRow(label, "%02d".format(current)) { open = true }
            DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
                range.step(step).forEach { v ->
                    DropdownMenuItem(text = { Text("%02d".format(v)) },
                        onClick = { onPick(v); open = false })
                }
            }
        }
    }

    var kindOpen by remember { mutableStateOf(false) }
    Box {
        SettingsNavRow("Frequency", when (spec.kind) {
            CronKind.HOURLY -> "Hourly"; CronKind.DAILY -> "Daily"
            CronKind.WEEKLY -> "Weekly"; CronKind.CUSTOM -> "Custom"
        }) { kindOpen = true }
        DropdownMenu(expanded = kindOpen, onDismissRequest = { kindOpen = false }) {
            listOf(CronKind.HOURLY to "Hourly", CronKind.DAILY to "Daily",
                   CronKind.WEEKLY to "Weekly", CronKind.CUSTOM to "Custom").forEach { (k, lbl) ->
                DropdownMenuItem(text = { Text(lbl) }, onClick = {
                    kindOpen = false
                    set(spec.copy(kind = k, raw = if (k == CronKind.CUSTOM) buildCron(spec) else spec.raw))
                })
            }
        }
    }

    when (spec.kind) {
        CronKind.HOURLY -> {
            numberPicker("At minute", spec.minute, 0..55, 5) { set(spec.copy(minute = it)) }
            numberPicker("From hour", spec.fromHour, 0..23) {
                set(spec.copy(fromHour = it, toHour = maxOf(it, spec.toHour)))
            }
            numberPicker("To hour", spec.toHour, 0..23) {
                set(spec.copy(toHour = it, fromHour = minOf(it, spec.fromHour)))
            }
        }
        CronKind.DAILY -> {
            numberPicker("Hour", spec.hour, 0..23) { set(spec.copy(hour = it)) }
            numberPicker("Minute", spec.minute, 0..55, 5) { set(spec.copy(minute = it)) }
        }
        CronKind.WEEKLY -> {
            var dayOpen by remember { mutableStateOf(false) }
            Box {
                SettingsNavRow("Day", spec.dow) { dayOpen = true }
                DropdownMenu(expanded = dayOpen, onDismissRequest = { dayOpen = false }) {
                    CRON_DAYS.forEach { d ->
                        DropdownMenuItem(text = { Text(d) },
                            onClick = { set(spec.copy(dow = d)); dayOpen = false })
                    }
                }
            }
            numberPicker("Hour", spec.hour, 0..23) { set(spec.copy(hour = it)) }
            numberPicker("Minute", spec.minute, 0..55, 5) { set(spec.copy(minute = it)) }
        }
        CronKind.CUSTOM -> {
            var raw by remember(spec.raw) { mutableStateOf(spec.raw) }
            OutlinedTextField(raw, { raw = it; set(spec.copy(raw = it)) }, singleLine = true,
                label = { Text(stringResource(R.string.cron_expression)) },
                supportingText = { Text(stringResource(R.string.six_fields_hint)) },
                modifier = Modifier.fillMaxWidth())
        }
    }
}

/** "0 0 7-22 * * *" -> "hourly, 07:00-22:00". Falls back to the raw expression, which is the
 *  honest thing to show for anything this does not recognise -- a wrong plain-English reading of
 *  a cron is worse than the cron. */
fun cronInEnglish(cron: String): String {
    if (cron.isBlank()) return "not scheduled"
    val s = parseCron(cron)
    fun hhmm(h: Int) = "%02d:%02d".format(h, s.minute)
    // Falls back to the raw expression for anything the picker cannot describe. A wrong
    // plain-English reading of a cron is worse than the cron.
    return when (s.kind) {
        CronKind.HOURLY ->
            if (s.fromHour == 0 && s.toHour == 23) "hourly at :%02d".format(s.minute)
            else "hourly, ${hhmm(s.fromHour)}-${hhmm(s.toHour)}"
        CronKind.DAILY -> "daily at ${hhmm(s.hour)}"
        CronKind.WEEKLY -> "${s.dow} at ${hhmm(s.hour)}"
        CronKind.CUSTOM -> cron
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun RecipeScreen(cm: ConnectionManager, nav: NavController, recipeId: String, onOpenChat: () -> Unit) {
    LaunchedEffect(cm.online.value) { if (cm.online.value) cm.refreshSchedules() }
    val r = cm.recipes.value.firstOrNull { it.id == recipeId }
    val job = cm.schedules.value.firstOrNull { it.source == r?.filePath }
    var confirmDelete by remember { mutableStateOf(false) }

    Scaffold(topBar = {
        TopAppBar(
            title = { Text(r?.title ?: "Recipe") },
            navigationIcon = {
                IconButton(onClick = { nav.popBackStack() }) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "back")
                }
            },
        )
    }) { pad ->
        if (r == null) {
            Box(Modifier.padding(pad).fillMaxSize(), contentAlignment = Alignment.Center) {
                Text(stringResource(R.string.recipe_not_found), color = MaterialTheme.colorScheme.outline)
            }
            return@Scaffold
        }
        Column(Modifier.padding(pad).padding(horizontal = 16.dp).fillMaxSize()
            .verticalScroll(rememberScrollState())) {

            Row(Modifier.fillMaxWidth().padding(top = 8.dp),
                verticalAlignment = Alignment.CenterVertically) {
                Button(onClick = {
                    cm.runRecipe(r.id); onOpenChat()
                }) { Text(stringResource(R.string.start_session)) }
            }
            if (r.description.isNotBlank()) {
                Text(r.description, style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.outline,
                    modifier = Modifier.padding(vertical = 8.dp))
            }

            SettingsSection("Schedule") {
                val currentCron = r.cron ?: job?.cron ?: ""
                var cron by remember(r.id, currentCron) { mutableStateOf(currentCron) }
                // Cron stays the stored format -- it is what goose takes and what every other
                // client shows. This only chooses one.
                CronEditor(cron) { cron = it }
                Text(
                    if (cron.isBlank()) "Not scheduled" else cronInEnglish(cron),
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.padding(top = 8.dp))
                Text(cron.ifBlank { "—" }, style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.outline)
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                    if (currentCron.isNotBlank()) {
                        TextButton(onClick = { cm.setRecipeCron(r.id, null); cron = "" }) {
                            Text(stringResource(R.string.unschedule))
                        }
                    }
                    TextButton(enabled = cron.isNotBlank() && cron.trim() != currentCron,
                        onClick = { cm.setRecipeCron(r.id, cron.trim()) }) { Text(stringResource(R.string.save_schedule)) }
                }
                SettingCaption("Server local time.")
                job?.let { j ->
                    SettingsSwitchRow("Enabled", !j.paused) { on ->
                        cm.setSchedulePaused(j.id, !on)
                    }
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        TextButton(enabled = !j.running, onClick = { cm.runScheduleNow(j.id) }) {
                            Text(stringResource(R.string.run_now))
                        }
                        if (j.running) {
                            CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
                            Spacer(Modifier.width(8.dp))
                            Text("running", style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.outline)
                        }
                    }
                    j.lastRun?.let {
                        SettingCaption("Last run ${it.take(16).replace('T', ' ')} · job ${j.id}")
                    }
                }
            }

            SettingsSection("Model") {
                var model by remember(r.id, r.model) { mutableStateOf(r.model.orEmpty()) }
                var provider by remember(r.id, r.provider) { mutableStateOf(r.provider.orEmpty()) }
                OutlinedTextField(model, { model = it }, singleLine = true,
                    label = { Text(stringResource(R.string.model_title)) },
                    placeholder = { Text(stringResource(R.string.blank_server_default)) },
                    modifier = Modifier.fillMaxWidth())
                var open by remember { mutableStateOf(false) }
                Box {
                    SettingsNavRow("Provider", provider.ifBlank { "(server default)" }) { open = true }
                    DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
                        // Source from the live server inventory (configured providers) instead of
                        // a hardcoded set that silently omitted server-configured providers. The
                        // leading blank entry means "use the server default". When the inventory
                        // hasn't loaded this yields just the blank + current choice, so the
                        // field degrades to free pick instead of offering a stale catalog.
                        val choices = listOf("") + cm.providerChoices(provider)
                        choices.forEach { p ->
                            DropdownMenuItem(
                                text = { Text(when (p) {
                                    "" -> "(server default)"
                                    "openai" -> "openai (local)"
                                    else -> p
                                }) },
                                onClick = { provider = p; open = false })
                        }
                    }
                }
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                    TextButton(
                        enabled = model != r.model.orEmpty() || provider != r.provider.orEmpty(),
                        onClick = {
                            // Two writes, model first: each save replaces the whole recipe, so
                            // they have to be applied in sequence off the same base or the second
                            // reverts the first.
                            var dto = cm.recipeWithSetting(r, "goose_model", model.trim())
                            dto = cm.recipeWithSetting(
                                r.copy(raw = dto), "goose_provider", provider.trim())
                            cm.saveRecipe(r.id, dto)
                        }) { Text(stringResource(R.string.save_model)) }
                }
                SettingCaption("A recipe's own pin wins over the server default. A cloud model " +
                    "under provider \"openai\" is sent to LocalAI and 404s — move both together.")
            }

            SettingsSection("Prompt") {
                var prompt by remember(r.id, r.prompt) { mutableStateOf(r.prompt.orEmpty()) }
                OutlinedTextField(prompt, { prompt = it }, minLines = 2, maxLines = 8,
                    label = { Text(stringResource(R.string.prompt)) },
                    supportingText = { Text(stringResource(R.string.prompt_hint)) },
                    modifier = Modifier.fillMaxWidth())
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                    TextButton(enabled = prompt != r.prompt.orEmpty(), onClick = {
                        cm.saveRecipe(r.id, cm.recipeWith(r, "prompt", prompt))
                    }) { Text(stringResource(R.string.save_prompt)) }
                }
            }

            SettingsSection("Instructions") {
                var instr by remember(r.id, r.instructions) {
                    mutableStateOf(r.instructions.orEmpty())
                }
                OutlinedTextField(instr, { instr = it }, minLines = 4, maxLines = 20,
                    label = { Text(stringResource(R.string.instructions)) },
                    supportingText = { Text(stringResource(R.string.instructions_hint)) },
                    modifier = Modifier.fillMaxWidth())
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                    TextButton(enabled = instr != r.instructions.orEmpty(), onClick = {
                        cm.saveRecipe(r.id, cm.recipeWith(r, "instructions", instr))
                    }) { Text(stringResource(R.string.save_instructions)) }
                }
            }

            // Read-only from here down. These are structural -- a sub-recipe's delegate wiring and
            // an extension's tool allowlist are what make a briefing read-only and scoped, and a
            // phone-sized editor for them would be a good way to break a job silently. They are
            // shown because "what does this actually run" is the question the screen exists to
            // answer.
            if (r.extensions.isNotEmpty()) {
                SettingsSection("Extensions") {
                    r.extensions.forEach { Text("· $it", Modifier.padding(vertical = 2.dp)) }
                    SettingCaption("The tools this recipe can reach. Edit in the recipe file.")
                }
            }

            if (r.subRecipes.isNotEmpty()) {
                SettingsSection("Sub-recipes") {
                    r.subRecipes.forEach { Text("· $it", Modifier.padding(vertical = 2.dp)) }
                    SettingCaption("Delegated checks, each with its own small context. Their " +
                        "tools come from THIS recipe's extensions, not their own files.")
                }
            }

            if (r.parameters.isNotEmpty()) {
                SettingsSection("Parameters") {
                    r.parameters.forEach { p ->
                        Text("· ${p.key}" + if (p.requirement == "optional") " (optional)" else "",
                            Modifier.padding(top = 4.dp))
                        if (p.description.isNotBlank()) {
                            Text(p.description, style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.outline)
                        }
                    }
                    SettingCaption("A schedule freezes parameter values when it is created, so " +
                        "anything that changes per run has to be a tool call, not a parameter.")
                }
            }

            SettingsSection("Danger") {
                TextButton(onClick = { confirmDelete = true }) { Text(stringResource(R.string.delete_recipe)) }
                SettingCaption(r.filePath)
            }
            Spacer(Modifier.height(24.dp))
        }
    }

    if (confirmDelete) {
        AlertDialog(
            onDismissRequest = { confirmDelete = false },
            title = { Text("Delete ${r?.title}?") },
            text = { Text(stringResource(R.string.recipe_delete_body)) },
            confirmButton = {
                TextButton(onClick = {
                    confirmDelete = false
                    r?.let { rec -> cm.deleteRecipe(rec.id) }
                    nav.popBackStack()
                }) { Text(stringResource(R.string.delete)) }
            },
            dismissButton = {
                TextButton(onClick = { confirmDelete = false }) { Text(stringResource(R.string.cancel)) }
            },
        )
    }
}


/** One model consumer, rendered the same way for every one of them: an optional on/off, a
 *  provider, and a model.
 *
 *  Every feature here picks a model, and before this they each did it differently -- chat had
 *  two config keys, vision had a container environment variable, speech had app-local strings,
 *  and the fast model had nothing at all. Same shape for all of them, so "which model does X
 *  use" has one answer in one place.
 *
 *  `onEnabled(false)` is what OFF means, and it is per-row rather than a blanket rule: for the
 *  fast model it clears the key so goose uses the chat model, for vision it stops the proxy
 *  describing images. Nothing is left half-set.
 *
 *  `incompatible` disables the row outright and says why -- a toggle that can be switched on
 *  into a configuration that cannot work is worse than one that is greyed out. */
@Composable
internal fun ModelRow(
    title: String,
    caption: String,
    enabled: Boolean,
    onEnabled: ((Boolean) -> Unit)?,          // null = not optional, always on
    provider: String,
    onProvider: (String) -> Unit,
    model: String,
    onModel: (String) -> Unit,
    providers: List<String>,
    modelChoices: List<String> = emptyList(),
    incompatible: String? = null,
    showModel: Boolean = true,
) {
    // `enabled` is derived from the server value being set, so switching ON could never
    // stick: onEnabled(true) has nothing to write (the model is still blank), the derived
    // value stayed false, and the row snapped shut — the fields you needed in order to set
    // a model were exactly what the switch was supposed to reveal. Fast and Vision could
    // therefore be turned off and never back on. Local state opens the row; the server is
    // written when a model is actually saved.
    var enabledLocal by remember(enabled) { mutableStateOf(enabled) }
    val on = enabledLocal && incompatible == null
    SettingsSection(title) {
        if (onEnabled != null) {
            Row(Modifier.fillMaxWidth().padding(vertical = 6.dp),
                verticalAlignment = Alignment.CenterVertically) {
                Text(if (incompatible != null) "Unavailable" else "Enabled", Modifier.weight(1f),
                    color = if (incompatible != null) MaterialTheme.colorScheme.outline
                            else MaterialTheme.colorScheme.onSurface)
                Switch(checked = on, enabled = incompatible == null,
                    onCheckedChange = { enabledLocal = it; onEnabled(it) })
            }
        }
        incompatible?.let { SettingCaption(it) }
        if (on) {
            var pOpen by remember { mutableStateOf(false) }
            Box {
                SettingsNavRow("Provider", provider.ifBlank { "—" }) { pOpen = true }
                DropdownMenu(expanded = pOpen, onDismissRequest = { pOpen = false }) {
                    providers.forEach { pv ->
                        DropdownMenuItem(
                            text = { Text(if (pv == "openai") "openai (local)" else pv) },
                            onClick = { onProvider(pv); pOpen = false })
                    }
                }
            }
            var mOpen by remember { mutableStateOf(false) }
            var draft by remember(model) { mutableStateOf(model) }
            if (showModel) Box {
                OutlinedTextField(draft, { draft = it }, singleLine = true,
                    label = { Text(stringResource(R.string.model_title)) },
                    trailingIcon = {
                        Row {
                            if (modelChoices.isNotEmpty())
                                IconButton(onClick = { mOpen = true }) {
                                    Icon(Icons.Filled.ArrowDropDown, contentDescription = "choose")
                                }
                            if (draft.isNotBlank() && draft != model)
                                TextButton(onClick = { onModel(draft.trim()) }) { Text(stringResource(R.string.save)) }
                        }
                    },
                    modifier = Modifier.fillMaxWidth().padding(top = 4.dp))
                DropdownMenu(expanded = mOpen, onDismissRequest = { mOpen = false }) {
                    modelChoices.sorted().forEach { mm ->
                        DropdownMenuItem(text = { Text(mm) },
                            onClick = { draft = mm; onModel(mm); mOpen = false })
                    }
                }
            }
        }
        SettingCaption(caption)
    }
}

// ---- Settings subpages ------------------------------------------------------
//
// Settings used to be one long scroll where Connection, Voice, Models and Images sat next
// to Appearance and Security. These are the same sections, grouped by what they configure:
// the SERVER you talk to, the MODELS it runs, the TOOLS it exposes, and the ASSISTANT.
// Device-local preferences (notifications, theme, biometric lock) stay on the main page,
// because they are not settings of the goose at all.
