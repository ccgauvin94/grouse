// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.navigation.NavController

/** A secret lives in goose's `secrets:` section, not beside ordinary config keys. The two
 *  sections can hold the SAME key with different values, so the app's mirror prefixes secrets
 *  in its own key space; the prefix never goes over the wire to goose. */
internal const val GOOSE_SECRET_KEY_PREFIX = "secret:"
internal const val GOOSE_PLAIN_KEY_PREFIX = "config:"
internal const val GOOSE_MASKED_VALUE = "••••••••"

/** App-side key for one config row. Trim once here so a key typed with trailing spaces cannot
 *  create a second row that writes a different server key. */
internal fun gooseConfigEntryKey(key: String, isSecret: Boolean): String {
    val trimmed = key.trim()
    return if (isSecret) GOOSE_SECRET_KEY_PREFIX + trimmed else trimmed
}

/** Reverse of [gooseConfigEntryKey]. Pass the RESULT to the config-read/write calls. */
internal fun parseGooseConfigEntryKey(entryKey: String): Pair<String, Boolean> =
    if (entryKey.startsWith(GOOSE_SECRET_KEY_PREFIX))
        entryKey.removePrefix(GOOSE_SECRET_KEY_PREFIX) to true
    else entryKey to false

/** The core's explicit config_read_ex response namespaces its key (`config:` or
 *  `secret:`) because the callback has no is_secret flag. read-all replies are
 *  bare and ordinary; a fallback "is this a known secret?" would misclassify an
 *  ordinary key that shares a name with a secret. */
internal fun gooseConfigIncomingEntryKey(rawKey: String, secretKeys: Set<String>): String {
    val raw = rawKey.trim()
    if (raw.startsWith(GOOSE_SECRET_KEY_PREFIX))
        return gooseConfigEntryKey(raw.removePrefix(GOOSE_SECRET_KEY_PREFIX), true)
    if (raw.startsWith(GOOSE_PLAIN_KEY_PREFIX))
        return gooseConfigEntryKey(raw.removePrefix(GOOSE_PLAIN_KEY_PREFIX), false)
    return gooseConfigEntryKey(raw, false)
}

/** The list is a client-side filter over the mirror, sorted so a key is always in the same
 *  place. The query matches the KEY only — values (especially secrets) are never searched. */
internal fun filterGooseConfigEntries(
    entries: Map<String, String>,
    query: String,
): List<Pair<String, String>> {
    val q = query.trim()
    return entries.toList()
        .filter { (entryKey, _) ->
            val (key, _) = parseGooseConfigEntryKey(entryKey)
            q.isEmpty() || key.contains(q, ignoreCase = true)
        }
        .sortedBy { (entryKey, _) -> parseGooseConfigEntryKey(entryKey).first.lowercase() }
}

/** A secret's stored value is NEVER put in an editor or shown in the list: both show this
 *  marker instead. The editor starts empty and is a replace operation. */
internal fun gooseConfigDisplayValue(entryKey: String, value: String): String {
    val (_, isSecret) = parseGooseConfigEntryKey(entryKey)
    return if (isSecret) GOOSE_MASKED_VALUE else value
}

/** Route segment for a detail screen. Percent-encode the key as a single path segment so `:`
 *  and `/` inside a config key cannot split the route. Pure (no android.net.Uri in unit tests);
 *  MainActivity decodes the argument with Uri.decode, which understands the same escapes. */
internal fun gooseConfigDetailRoute(entryKey: String): String {
    val (key, isSecret) = parseGooseConfigEntryKey(entryKey)
    return "goose_config_detail/${encodeConfigPathSegment(key)}/${if (isSecret) 1 else 0}"
}

private fun encodeConfigPathSegment(key: String): String = buildString {
    for (byte in key.toByteArray(Charsets.UTF_8)) {
        val c = byte.toInt() and 0xff
        val unreserved = c in 'A'.code..'Z'.code || c in 'a'.code..'z'.code ||
            c in '0'.code..'9'.code || c in "-._~".toCharArray().map { it.code }
        if (unreserved) append(c.toChar())
        else append('%').append(String.format("%02X", c))
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun GooseConfigScreen(cm: ConnectionManager, nav: NavController) {
    var query by rememberSaveable { mutableStateOf("") }
    var adding by remember { mutableStateOf(false) }
    LaunchedEffect(Unit) { cm.refreshGooseConfig() }
    val entries = filterGooseConfigEntries(cm.gooseConfig, query)

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.goose_config)) },
                navigationIcon = {
                    IconButton(onClick = { nav.popBackStack() }) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "back")
                    }
                },
                actions = {
                    IconButton(onClick = { cm.refreshGooseConfig() }) {
                        Icon(Icons.Filled.Refresh, contentDescription = "refresh config")
                    }
                },
            )
        },
        floatingActionButton = {
            androidx.compose.material3.FloatingActionButton(onClick = { adding = true }) {
                Icon(Icons.Filled.Add, contentDescription = "add config key")
            }
        },
    ) { pad ->
        Column(Modifier.padding(pad).fillMaxSize()) {
            OutlinedTextField(
                value = query,
                onValueChange = { query = it },
                label = { Text(stringResource(R.string.goose_config_search)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
            )
            if (cm.configBusy.value && entries.isEmpty()) {
                Row(Modifier.padding(horizontal = 16.dp), verticalAlignment = Alignment.CenterVertically) {
                    CircularProgressIndicator(Modifier.width(16.dp).height(16.dp), strokeWidth = 2.dp)
                    Spacer(Modifier.width(8.dp))
                    Text(stringResource(R.string.loading), style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.outline)
                }
            }
            LazyColumn(Modifier.fillMaxSize().padding(horizontal = 16.dp)) {
                items(entries, key = { it.first }) { (entryKey, value) ->
                    val (key, _) = parseGooseConfigEntryKey(entryKey)
                    Row(
                        Modifier.fillMaxWidth().clickable { nav.navigate(gooseConfigDetailRoute(entryKey)) }
                            .padding(vertical = 12.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Column(Modifier.weight(1f).padding(end = 12.dp)) {
                            Text(key, style = MaterialTheme.typography.bodyLarge,
                                fontFamily = FontFamily.Monospace, maxLines = 1,
                                overflow = TextOverflow.Ellipsis)
                            Text(
                                if (entryKey.startsWith(GOOSE_SECRET_KEY_PREFIX))
                                    stringResource(R.string.goose_config_secret_label)
                                else gooseConfigDisplayValue(entryKey, value),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.outline,
                                maxLines = 1, overflow = TextOverflow.Ellipsis,
                            )
                        }
                    }
                    HorizontalDivider()
                }
                if (entries.isEmpty() && !cm.configBusy.value) {
                    item {
                        Text(stringResource(R.string.goose_config_empty),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.outline,
                            modifier = Modifier.padding(vertical = 16.dp))
                    }
                }
            }
        }
    }

    if (adding) {
        var newKey by remember { mutableStateOf("") }
        var newSecret by remember { mutableStateOf(false) }
        AlertDialog(
            onDismissRequest = { adding = false },
            title = { Text(stringResource(R.string.goose_config_add_key)) },
            text = {
                Column {
                    OutlinedTextField(
                        newKey, { newKey = it },
                        label = { Text(stringResource(R.string.goose_config_new_key)) },
                        placeholder = { Text(stringResource(R.string.goose_config_new_key_hint)) },
                        singleLine = true,
                    )
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Checkbox(checked = newSecret, onCheckedChange = { newSecret = it })
                        Text(stringResource(R.string.goose_config_secret_checkbox))
                    }
                    SettingCaption(stringResource(R.string.goose_config_add_note))
                }
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        if (newKey.isNotBlank()) {
                            nav.navigate(gooseConfigDetailRoute(gooseConfigEntryKey(newKey, newSecret)))
                            adding = false
                        }
                    },
                    enabled = newKey.isNotBlank(),
                ) { Text(stringResource(R.string.goose_config_add_key)) }
            },
            dismissButton = {
                TextButton(onClick = { adding = false }) { Text(stringResource(R.string.cancel)) }
            },
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun GooseConfigDetailScreen(
    cm: ConnectionManager,
    nav: NavController,
    key: String,
    isSecret: Boolean,
) {
    val entryKey = gooseConfigEntryKey(key, isSecret)
    val current = cm.gooseConfig[entryKey].orEmpty()
    // Secrets are replace-only: NEVER seed the editor with the masked value. Even for a plain
    // key, seed once — a confirming read must not overwrite what the user is typing.
    var value by remember(entryKey) { mutableStateOf(if (isSecret) "" else current) }
    LaunchedEffect(entryKey) { cm.readGooseConfigKey(key, isSecret) }
    LaunchedEffect(current) {
        if (!isSecret && value.isEmpty() && current.isNotEmpty()) value = current
    }
    var confirmDelete by remember { mutableStateOf(false) }
    var saved by remember { mutableStateOf(false) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.goose_config)) },
                navigationIcon = {
                    IconButton(onClick = { nav.popBackStack() }) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "back")
                    }
                },
                actions = {
                    if (cm.configBusy.value)
                        CircularProgressIndicator(Modifier.padding(end = 16.dp).width(18.dp).height(18.dp),
                            strokeWidth = 2.dp)
                },
            )
        },
    ) { pad ->
        Column(Modifier.padding(pad).padding(horizontal = 16.dp).fillMaxSize()
            .verticalScroll(rememberScrollState())) {
            Spacer(Modifier.height(8.dp))
            Text(key, style = MaterialTheme.typography.titleMedium, fontFamily = FontFamily.Monospace)
            Spacer(Modifier.height(4.dp))
            SettingCaption(stringResource(
                if (isSecret) R.string.goose_config_secret_label else R.string.goose_config_plain_label))
            Text(
                stringResource(R.string.goose_config_current) + ": " +
                    if (isSecret) GOOSE_MASKED_VALUE else current.ifEmpty {
                        stringResource(R.string.goose_config_not_set)
                    },
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.outline,
            )
            if (isSecret) SettingCaption(stringResource(R.string.goose_config_secret_replace_note))
            Spacer(Modifier.height(12.dp))
            OutlinedTextField(
                value = value,
                onValueChange = { value = it; saved = false },
                label = { Text(stringResource(R.string.goose_config_value)) },
                placeholder = {
                    if (isSecret) Text(stringResource(R.string.goose_config_secret_value_hint))
                },
                visualTransformation = if (isSecret) PasswordVisualTransformation()
                                       else VisualTransformation.None,
                minLines = if (isSecret) 1 else 3,
                maxLines = 12,
                enabled = !cm.configBusy.value,
                modifier = Modifier.fillMaxWidth(),
            )
            SettingCaption(stringResource(R.string.goose_config_apply_note))
            Spacer(Modifier.height(12.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = {
                        cm.setGooseConfigValue(key, value, isSecret)
                        saved = true
                    },
                    enabled = value.isNotEmpty() && !cm.configBusy.value,
                ) { Text(stringResource(R.string.save)) }
                TextButton(onClick = { confirmDelete = true }, enabled = !cm.configBusy.value) {
                    Text(stringResource(R.string.delete))
                }
            }
            if (saved) {
                Spacer(Modifier.height(8.dp))
                Text(stringResource(R.string.saved), style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.primary)
            }
            Spacer(Modifier.height(24.dp))
        }
    }

    if (confirmDelete) AlertDialog(
        onDismissRequest = { confirmDelete = false },
        title = { Text(stringResource(R.string.goose_config_delete_title, key)) },
        text = { Text(stringResource(R.string.goose_config_delete_body)) },
        confirmButton = {
            TextButton(onClick = {
                confirmDelete = false
                cm.removeGooseConfigValue(key, isSecret)
                nav.popBackStack()
            }) { Text(stringResource(R.string.delete)) }
        },
        dismissButton = {
            TextButton(onClick = { confirmDelete = false }) { Text(stringResource(R.string.cancel)) }
        },
    )
}
