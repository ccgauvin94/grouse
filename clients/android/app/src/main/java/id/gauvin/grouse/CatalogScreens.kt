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


/** Every saved recipe, whether or not anything runs it on a timer.
 *
 *  Split out of the Scheduler screen, which listed recipes underneath the cron table and made
 *  an unscheduled recipe look like a broken schedule. They are separate things: a recipe is
 *  what to run, a schedule is when. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun RecipesScreen(cm: ConnectionManager, nav: NavController, onOpenChat: () -> Unit) {
    LaunchedEffect(cm.online.value) { if (cm.online.value) cm.refreshSchedules() }
    var note by remember { mutableStateOf<String?>(null) }
    Scaffold(topBar = {
        TopAppBar(
            title = { Text(stringResource(R.string.recipes)) },
            navigationIcon = {
                IconButton(onClick = { nav.popBackStack() }) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "back")
                }
            },
            actions = {
                IconButton(onClick = { cm.refreshSchedules() }) {
                    Icon(Icons.Filled.Refresh, contentDescription = "refresh")
                }
            },
        )
    }) { pad ->
        Column(Modifier.padding(pad).padding(horizontal = 16.dp).fillMaxSize()
            .verticalScroll(rememberScrollState())) {

            note?.let {
                Text(it, style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.padding(vertical = 8.dp))
            }
            if (cm.recipes.value.isEmpty()) SettingCaption("No saved recipes on the server.")

            cm.recipes.value.forEach { r ->
                val job = cm.schedules.value.firstOrNull { it.source == r.filePath }
                Row(Modifier.fillMaxWidth().padding(vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f).clickable { nav.navigate("recipe/" + r.id) }) {
                        Text(r.title)
                        Text(
                            buildString {
                                append(if (r.cron.isNullOrBlank()) "not scheduled"
                                       else cronInEnglish(r.cron))
                                if (job?.running == true) append("  ·  running now")
                                else if (job?.paused == true) append("  ·  paused")
                                job?.lastRun?.let { append("  ·  last ${it.take(16).replace('T', ' ')}") }
                            },
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.outline,
                        )
                    }
                    when {
                        job?.running == true ->
                            CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                        job != null ->
                            Switch(checked = !job.paused,
                                onCheckedChange = { cm.setSchedulePaused(job.id, !it) })
                    }
                }
                Row {
                    // Running a recipe = starting a session FROM it. goose applies its
                    // extensions, settings and instructions and titles the session after it, so
                    // this is the same object the scheduler runs, driven by hand.
                    TextButton(onClick = { cm.runRecipe(r.id); onOpenChat() }) { Text(stringResource(R.string.start_session)) }
                }
                if (job != null) {
                    Row {
                        TextButton(enabled = !job.running, onClick = {
                            cm.runScheduleNow(job.id)
                            // run-now blocks server-side for the whole run, so its reply is the
                            // finish rather than the start. Say what will happen, not "started".
                            note = "Running ${r.title} — a briefing takes a few minutes and " +
                                "notifies if it has something."
                        }) { Text(stringResource(R.string.run_now)) }
                    }
                }
                HorizontalDivider()
            }

            // There is no separate scheduler list any more. A schedule was never a thing on its
            // own -- it is a property of a recipe, and showing the two as separate screens meant
            // the same job appeared twice with different affordances on each.
            SettingCaption("A recipe is what runs; its schedule is one of its settings. Tap one " +
                "to change when it runs, which model it uses, or what it says.")
            Spacer(Modifier.height(28.dp))
        }
    }
}

/** Skills: the per-domain notes goose pulls in with load_skill only when they are relevant.
 *
 *  Worth being able to read from the phone precisely because they are invisible in normal use —
 *  unlike .goosehints, which sits in every prompt, a skill costs nothing until something loads
 *  it, so a wrong one can sit there being wrong for weeks. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SkillsScreen(cm: ConnectionManager, nav: NavController) {
    LaunchedEffect(cm.online.value) { if (cm.online.value) cm.refreshSkills() }
    Scaffold(topBar = {
        TopAppBar(
            title = { Text(stringResource(R.string.skills)) },
            navigationIcon = {
                IconButton(onClick = { nav.popBackStack() }) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "back")
                }
            },
            actions = {
                IconButton(onClick = { cm.refreshSkills() }) {
                    Icon(Icons.Filled.Refresh, contentDescription = "refresh")
                }
            },
        )
    }) { pad ->
        Column(Modifier.padding(pad).padding(horizontal = 16.dp).fillMaxSize()
            .verticalScroll(rememberScrollState())) {
            if (cm.skills.value.isEmpty()) SettingCaption("No skills installed.")
            cm.skills.value.forEach { sk ->
                SettingsNavRow(sk.name, sk.description.take(90)) {
                    nav.navigate("skill/" + Uri.encode(sk.name))
                }
            }
            SettingCaption("Listed one line each to the model; the body is only read when it " +
                "calls load_skill. That is why detailed tool procedure belongs here rather " +
                "than in the hints, which are in context every single turn.")
            Spacer(Modifier.height(28.dp))
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SkillScreen(cm: ConnectionManager, nav: NavController, name: String) {
    LaunchedEffect(cm.online.value) { if (cm.online.value) cm.refreshSkills() }
    val sk = cm.skills.value.firstOrNull { it.name == name }
    Scaffold(topBar = {
        TopAppBar(
            title = { Text(sk?.name ?: "Skill") },
            navigationIcon = {
                IconButton(onClick = { nav.popBackStack() }) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "back")
                }
            },
        )
    }) { pad ->
        if (sk == null) {
            Box(Modifier.padding(pad).fillMaxSize(), contentAlignment = Alignment.Center) {
                Text(stringResource(R.string.skill_not_found), color = MaterialTheme.colorScheme.outline)
            }
            return@Scaffold
        }
        Column(Modifier.padding(pad).padding(horizontal = 16.dp).fillMaxSize()
            .verticalScroll(rememberScrollState())) {
            Text(sk.description, style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.outline,
                modifier = Modifier.padding(vertical = 8.dp))
            var body by remember(sk.path, sk.content) { mutableStateOf(sk.content) }
            OutlinedTextField(body, { body = it }, minLines = 10, maxLines = 40,
                enabled = sk.writable,
                label = { Text(if (sk.writable) "SKILL.md" else "SKILL.md (read-only)") },
                modifier = Modifier.fillMaxWidth())
            if (sk.writable) {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                    TextButton(enabled = body != sk.content,
                        onClick = { cm.saveSkill(sk, body) }) { Text(stringResource(R.string.save)) }
                }
            } else {
                // Bundled skills live inside goose's own directory; a save would fail, so the
                // field is disabled rather than offering an edit that cannot land.
                SettingCaption("Bundled with goose, so it cannot be edited here.")
            }
            SettingCaption(sk.path)
            Spacer(Modifier.height(28.dp))
        }
    }
}




// A Code screen and a server-side directory picker stood here and are GONE from this branch.
// Both depended on _goose/unstable/fs/list_directory, which is a fork method rather than
// something upstream goose answers, and on knowing this one server's layout. Sessions here are
// filed by goose's own project id and run wherever the configured working directory points.
