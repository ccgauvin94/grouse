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


/** Model picker: a READ-ONLY dropdown. Pick from goose's featured models + ones seen before for the
 *  CURRENT provider (provider-scoped, so LocalAI and OpenRouter never mix, and now also live-fetched
 *  from the provider's actual backend -- see ConnectionManager/onSupportedModels), or
 *  pick "Custom model…" to type any model id in a dialog. Free text is needed for OpenRouter slugs
 *  goose doesn't "feature" (e.g. poolside/laguna-s-2.1) and any local model id not yet in the list. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ModelDropdown(opt: ConfigOption, knownModels: Set<String>, onPick: (String, String) -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    var showCustomDialog by remember { mutableStateOf(false) }
    val featured = opt.choices.map { it.value }
    val entries = (featured + knownModels.filter { it !in featured }).distinct()
    fun labelFor(v: String) = if (v == "current") "Provider default"
        else opt.choices.firstOrNull { it.value == v }?.label ?: v
    fun commit(v: String) { expanded = false; onPick("model", v) }
    val currentLabel = if (opt.currentValue.isBlank()) "Provider default" else labelFor(opt.currentValue)
    ExposedDropdownMenuBox(
        expanded = expanded, onExpandedChange = { expanded = it },
        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp)
    ) {
        OutlinedTextField(
            value = currentLabel,
            onValueChange = {},
            readOnly = true,
            singleLine = true,
            label = { Text(stringResource(R.string.model)) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded) },
            modifier = Modifier.menuAnchor().fillMaxWidth()
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            // NO "Provider default" entry. It committed the literal string "current", which
            // goose forwards verbatim to the backend, and LocalAI answers
            //   404  model "current" not found. To see available models, call GET /v1/models
            // so selecting it killed the chat until another model was picked. It was also
            // conceptually empty: setConfigOption("model", X) makes goose WRITE X into its
            // config.yaml, so whatever this picker last chose IS the provider default.
            entries.forEach { v ->
                DropdownMenuItem(text = { Text(labelFor(v)) }, onClick = { commit(v) })
            }
            DropdownMenuItem(
                text = { Text(stringResource(R.string.custom_model_menu)) },
                onClick = { expanded = false; showCustomDialog = true },
            )
        }
    }
    if (showCustomDialog) {
        CustomModelDialog(
            initial = opt.currentValue,
            onConfirm = { v -> showCustomDialog = false; if (v.isNotBlank()) commit(v) },
            onDismiss = { showCustomDialog = false },
        )
    }
}

@Composable
private fun CustomModelDialog(initial: String, onConfirm: (String) -> Unit, onDismiss: () -> Unit) {
    var text by rememberSaveable { mutableStateOf(if (initial == "current") "" else initial) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.custom_model_title)) },
        text = {
            OutlinedTextField(
                value = text, onValueChange = { text = it }, singleLine = true,
                label = { Text(stringResource(R.string.model_id)) },
                placeholder = { Text(stringResource(R.string.model_id_placeholder)) },
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
                keyboardActions = KeyboardActions(onDone = { onConfirm(text.trim()) }),
                modifier = Modifier.fillMaxWidth(),
            )
        },
        confirmButton = { TextButton(onClick = { onConfirm(text.trim()) }, enabled = text.isNotBlank()) { Text(stringResource(R.string.set)) } },
        dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.cancel)) } },
    )
}

/** goose reports which values a knob accepts, and it is model-dependent -- do NOT second-guess it
 *  with a hardcoded list. `thinking_effort` offers only ["off"] on a model without extended
 *  thinking (Qwen3.6-35B-A3B on LocalAI) and ["off","low","medium","high","max"] on one that has it
 *  (z-ai/glm-5.2, deepseek-r1). A fallback list lived here briefly on the mistaken belief that goose
 *  sent no options at all -- that came from reading the wrong JSON key (`choices`; goose sends
 *  `options`) and it also omitted "max". Showing only "off" is correct, not a bug. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConfigDropdown(opt: ConfigOption, onPick: (String, String) -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    val label = opt.choices.firstOrNull { it.value == opt.currentValue }?.label ?: opt.currentValue
    ExposedDropdownMenuBox(
        expanded = expanded, onExpandedChange = { expanded = it },
        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp)
    ) {
        OutlinedTextField(
            value = label, onValueChange = {}, readOnly = true, singleLine = true,
            label = { Text(opt.name) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded) },
            modifier = Modifier.menuAnchor().fillMaxWidth()
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            opt.choices.forEach { c ->
                DropdownMenuItem(text = { Text(c.label) },
                    onClick = { expanded = false; onPick(opt.id, c.value) })
            }
        }
    }
}

/** Tool-approval mode as a compact dropdown (the + sheet's knob): shows the current
 *  mode, menu lists all modes with their one-line explanations. Same options as the
 *  old stacked blocks — just not a full-width column each. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ModeDropdown(opt: ConfigOption?, onPick: (String, String) -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    val current = opt?.currentValue.orEmpty()
    ExposedDropdownMenuBox(
        expanded = expanded, onExpandedChange = { expanded = it },
        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp)
    ) {
        OutlinedTextField(
            value = prettyMode(current),
            onValueChange = {}, readOnly = true, singleLine = true,
            label = { Text(stringResource(R.string.tool_mode)) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded) },
            modifier = Modifier.menuAnchor().fillMaxWidth()
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            opt?.choices?.forEach { c ->
                DropdownMenuItem(
                    text = {
                        Column {
                            Text(prettyMode(c.value))
                            Text(modeBlurb(c.value),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    },
                    onClick = { expanded = false; onPick("mode", c.value) },
                )
            }
        }
    }
}

// ---- Message bubbles --------------------------------------------------------
