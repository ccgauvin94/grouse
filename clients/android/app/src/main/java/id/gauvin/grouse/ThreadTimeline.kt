// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
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
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.navigation.NavController
import uniffi.grouse_core.TimelineRow
import java.time.Instant

/**
 * An AREA seen as a CASE, not as a chat.
 *
 * One vertical timeline: the card that opened the area, the conversation that followed,
 * the cards that arrived afterwards, and every action taken on them ("snoozed",
 * "completed", a re-observation) as its own row. That ordering is the whole feature — a
 * chat with a board bolted above it would lose which came first, which is the question
 * this screen answers.
 *
 * The rows arrive already merged and ordered from the CORE (`thread_timeline`, CONTRACT
 * §3.8); this file decides only how a card, an action and a message LOOK.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ThreadTimelineScreen(cm: ConnectionManager, nav: NavController, label: String) {
    // The chat half of the timeline IS this area's transcript, so bind it before asking —
    // the core reads the displayed store, and asking first would merge the conversation of
    // whatever chat was open.
    LaunchedEffect(label) { cm.openThreadChat(label) }
    val sessionId = cm.threadSessions.value[label]
    val current = cm.currentSession.value
    LaunchedEffect(sessionId, current) {
        if (sessionId != null && current == sessionId) cm.refreshThreadTimeline(label)
    }

    val rows = cm.threadTimeline.value
    val cards = rows.count { it.kind == "item" }
    var draft by rememberSaveable { mutableStateOf("") }
    var closing by remember { mutableStateOf<TimelineRow?>(null) }
    var note by rememberSaveable { mutableStateOf("") }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text(threadName(label), maxLines = 1, overflow = TextOverflow.Ellipsis)
                        Text(
                            if (cards == 1) "1 card" else "$cards cards",
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                },
                navigationIcon = {
                    IconButton(onClick = { nav.popBackStack() }) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
                actions = {
                    IconButton(onClick = { cm.refreshThreadTimeline(label) }) {
                        Icon(Icons.Filled.Refresh, contentDescription = "Reload this area")
                    }
                    TextButton(onClick = {
                        nav.navigate("myday") { launchSingleTop = true; popUpTo("chat") { inclusive = false } }
                    }) { Text("Board") }
                },
            )
        },
        bottomBar = {
            Surface(color = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.35f)) {
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    OutlinedTextField(
                        value = draft,
                        onValueChange = { draft = it },
                        placeholder = { Text(if (cm.busy.value) "Queue message…" else "Message about this area…") },
                        modifier = Modifier.weight(1f),
                        maxLines = 4,
                        shape = RoundedCornerShape(22.dp),
                    )
                    Spacer(Modifier.width(6.dp))
                    FilledIconButton(
                        onClick = {
                            val text = draft.trim()
                            if (text.isNotEmpty()) {
                                cm.send(text)
                                draft = ""
                            }
                        },
                        enabled = draft.isNotBlank(),
                        modifier = Modifier.size(44.dp),
                    ) {
                        Icon(Icons.AutoMirrored.Filled.Send, contentDescription = "Send")
                    }
                }
            }
        },
    ) { padding ->
        LazyColumn(Modifier.fillMaxSize().padding(padding)) {
            if (rows.isEmpty()) {
                item {
                    Text(
                        "Nothing here yet. This area's cards, the conversation and the actions " +
                            "taken on them appear here in the order they happened.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.outline,
                        modifier = Modifier.padding(20.dp),
                    )
                }
            }
            items(rows, key = { "${it.kind}:${it.id}:${it.at}" }) { row ->
                when (row.kind) {
                    "item" -> CardRow(
                        row = row,
                        onDone = { closing = row; note = "" },
                        onSnooze = {
                            // Tomorrow morning, the same default My Day uses for a swipe:
                            // "not now" should mean the next time you look, not a timer.
                            cm.snoozeTask(row.id, timelineNextMorning().toString(), "snoozed from the timeline")
                            cm.refreshTasks()
                            cm.refreshThreadTimeline(label)
                        },
                    )
                    "chat" -> ChatRow(row)
                    else -> ActionRow(row)
                }
            }
        }
    }

    closing?.let { row ->
        AlertDialog(
            onDismissRequest = { closing = null },
            title = { Text("Close this card?") },
            text = {
                Column {
                    Text(row.text, style = MaterialTheme.typography.bodyMedium)
                    Spacer(Modifier.height(8.dp))
                    // The note is REQUIRED by the workflow even though the tool accepts an
                    // empty one: a card closed with no line is what makes the next briefing
                    // re-raise it.
                    OutlinedTextField(
                        value = note,
                        onValueChange = { note = it },
                        label = { Text("What settled it") },
                        modifier = Modifier.fillMaxWidth(),
                        maxLines = 3,
                    )
                }
            },
            confirmButton = {
                TextButton(
                    enabled = note.isNotBlank(),
                    onClick = {
                        cm.closeTask(row.id, "done", note.trim())
                        cm.refreshTasks()
                        cm.refreshThreadTimeline(label)
                        closing = null
                    },
                ) { Text("Done") }
            },
            dismissButton = { TextButton(onClick = { closing = null }) { Text("Cancel") } },
        )
    }
}

/** A card: the thing the area is about. Carries its own actions, because a case file is
 *  something you ACT on rather than read. */
@Composable
private fun CardRow(row: TimelineRow, onDone: () -> Unit, onSnooze: () -> Unit) {
    Surface(
        shape = RoundedCornerShape(14.dp),
        color = MaterialTheme.colorScheme.secondaryContainer.copy(alpha = 0.45f),
        modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 6.dp),
    ) {
        Column(Modifier.padding(14.dp)) {
            Text(
                row.text.ifBlank { "card" },
                style = MaterialTheme.typography.bodyLarge,
                fontWeight = FontWeight.SemiBold,
            )
            if (row.detail.isNotBlank()) {
                Spacer(Modifier.height(4.dp))
                Text(
                    row.detail,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Spacer(Modifier.height(8.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    if (row.state.isNotBlank()) Chip(row.state)
                    if (row.itemKind.isNotBlank()) Chip(row.itemKind)
                    if (row.dueAt.isNotBlank()) Chip(dueLabel(row.dueAt))
                }
                Spacer(Modifier.weight(1f))
                TextButton(onClick = onSnooze) { Text("Snooze") }
                TextButton(onClick = onDone) { Text("Done") }
            }
        }
    }
}

/** A read-only chip. `AssistChip`'s overloads leave `onClick = {}` ambiguous, and none
 *  of these are buttons: they are the card's own labels. */
@Composable
private fun Chip(text: String) {
    Surface(
        shape = RoundedCornerShape(8.dp),
        color = MaterialTheme.colorScheme.surface,
    ) {
        Text(
            text,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 8.dp, vertical = 3.dp),
        )
    }
}

/** An action: something DONE to a card, or something the assistant decided about it. Dim
 *  and narrow on purpose — it is history, not a thing to act on. */
@Composable
private fun ActionRow(row: TimelineRow) {
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 4.dp),
        verticalAlignment = Alignment.Top,
    ) {
        // The rail: a thin bar is what makes a run of these read as one thread of events
        // rather than as small cards of their own.
        Box(
            Modifier
                .padding(top = 4.dp)
                .width(2.dp)
                .height(16.dp)
                .background(MaterialTheme.colorScheme.outlineVariant)
        )
        Spacer(Modifier.width(10.dp))
        Column {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    actionLabel(row.state),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                if (row.itemTitle.isNotBlank()) {
                    Spacer(Modifier.width(6.dp))
                    Text(
                        row.itemTitle,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.outline,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
            Text(
                row.text,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurface,
            )
        }
    }
}

/** A message in the area's own conversation, placed where it happened. */
@Composable
private fun ChatRow(row: TimelineRow) {
    val mine = row.role == "user"
    Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 3.dp)) {
        if (mine) Spacer(Modifier.weight(1f))
        Surface(
            shape = RoundedCornerShape(14.dp),
            color = if (mine) MaterialTheme.colorScheme.primaryContainer
                    else MaterialTheme.colorScheme.surfaceVariant,
            modifier = Modifier.widthIn(max = 300.dp),
        ) {
            Text(
                row.text,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
            )
        }
        if (!mine) Spacer(Modifier.weight(1f))
    }
}

/** "observation" → "seen again", because the pulse vocabulary is the store's, not a
 *  reader's. An unknown kind shows as itself rather than disappearing. */
private fun actionLabel(kind: String): String = when (kind) {
    "observation" -> "seen again"
    "decision" -> "snoozed"
    "reaction" -> "you acted"
    "resolution" -> "closed"
    "attention" -> "told you"
    else -> kind.ifBlank { "action" }
}

/** Tomorrow at 08:00 local, as an instant: the same "next time you look" default My Day
 *  uses, so a snooze from either surface means the same thing. */
private fun timelineNextMorning(): Instant =
    java.time.LocalDate.now().plusDays(1).atTime(8, 0)
        .atZone(java.time.ZoneId.systemDefault()).toInstant()
