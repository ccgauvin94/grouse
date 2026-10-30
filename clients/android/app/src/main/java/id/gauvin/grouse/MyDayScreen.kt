// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Menu
import androidx.compose.material.icons.filled.Psychology
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
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
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import androidx.navigation.NavController
import id.gauvin.grouse.ui.theme.statusColors
import java.time.Instant

/**
 * My Day — the Assistant tab, with the always-on thread at the top and the assistant's
 * own working state under it.
 *
 * The lists are assistant-state's `item` rows (open loops, waiting-ons, obligations)
 * fetched over a deterministic `tools/call`, so opening this tab costs milliseconds and
 * no model turn (CONTRACT §3.3). The server has no push for them — briefings change the
 * state in their own sessions — so freshness is pull-shaped by design: on open, on the
 * refresh affordance, and after every action the core re-reads and re-emits.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MyDayScreen(cm: ConnectionManager, nav: NavController, onOpenDrawer: () -> Unit) {
    var draft by rememberSaveable { mutableStateOf("") }
    // The item being closed, and which way: `done` (it happened) or `dropped` (it does
    // not matter). The note is REQUIRED by the workflow even though the tool accepts an
    // empty one: a closed item with no line is what makes the next briefing re-raise it.
    var closing by remember { mutableStateOf<TaskItem?>(null) }
    var closeState by remember { mutableStateOf("done") }
    var closeNote by rememberSaveable { mutableStateOf("") }

    LaunchedEffect(Unit) { if (cm.online.value) cm.refreshTasks() }

    // Every way into the conversation lands here. `openAssistant()` only SELECTS the thread in
    // the core — My Day is a separate route, so without this the tap looked dead while the chat
    // behind it had already switched, which is exactly how the card's "Open" behaved.
    val toChat: () -> Unit = {
        nav.navigate("chat") { launchSingleTop = true; popUpTo("chat") { inclusive = true } }
    }

    Scaffold(topBar = {
        TopAppBar(
            title = { Text("My Day") },
            navigationIcon = {
                IconButton(onClick = onOpenDrawer) { Icon(Icons.Filled.Menu, contentDescription = "menu") }
            },
            actions = {
                if (cm.tasksLoading.value) {
                    CircularProgressIndicator(Modifier.size(20.dp).padding(end = 4.dp), strokeWidth = 2.dp)
                } else {
                    IconButton(onClick = { cm.refreshTasks() }) {
                        Icon(Icons.Filled.Refresh, contentDescription = "refresh")
                    }
                }
            }
        )
    }) { pad ->
        LazyColumn(
            Modifier.padding(pad).fillMaxSize(),
            contentPadding = PaddingValues(horizontal = 16.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            // The always-on thread first: this tab replaces the drawer's Assistant button,
            // so the way into the conversation has to be the first thing on it.
            item {
                AssistantCard(
                    cm,
                    draft,
                    onChange = { draft = it },
                    onOpen = { cm.openAssistant(); toChat() },
                    onSend = { text -> cm.askAssistant(text); draft = ""; toChat() },
                )
            }

            cm.tasksError.value?.let { msg ->
                item {
                    Card(shape = RoundedCornerShape(12.dp)) {
                        Row(Modifier.fillMaxWidth().padding(12.dp), verticalAlignment = Alignment.CenterVertically) {
                            Text(
                                msg,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.error,
                                modifier = Modifier.weight(1f),
                            )
                            TextButton(onClick = { cm.refreshTasks() }) { Text("Retry") }
                        }
                    }
                }
            }

            // Handed-over work first, above the loops: it is the only thing on this
            // screen the assistant is DOING rather than reporting, and a row in
            // `review` is a person's turn to act.
            val handed = cm.handedTasks.value
            if (handed.isNotEmpty()) {
                item { SectionHeader("Handed over", handed.size) }
                items(handed, key = { "task-${it.id}" }) { t ->
                    HandedRow(
                        t,
                        onOpen = {
                            // Tap through to the session that did the work: that is
                            // where the discussion belongs, and it has the history.
                            if (t.sessionId.isNotBlank()) {
                                cm.openSession(t.sessionId)
                                toChat()
                            }
                        },
                    )
                }
            }

            val all = cm.openTasks.value
            // Snoozed rows arrive in the SAME section as awake ones (server-side a snoozed
            // item is still open, it is only quiet), so a partition here is what makes the
            // Snooze button observable at all. Read the clock once per composition: a row
            // whose snooze has just expired reads as open, which is exactly what the
            // server does on its next rank.
            val nowMs = System.currentTimeMillis()
            val snoozed = all.filter { it.isSnoozedAt(nowMs) }
            val open = all.filterNot { it.isSnoozedAt(nowMs) }
            item { SectionHeader("Open", open.size) }
            if (open.isEmpty() && cm.tasksError.value == null) {
                item { EmptyRow(if (cm.online.value) "Nothing open. The assistant adds here when a briefing finds something." else "Not connected — the lists load once the server is reachable.") }
            }
            items(open, key = { it.id }) { t ->
                TaskRow(
                    t,
                    onDone = { closing = t; closeState = "done"; closeNote = "" },
                    onDrop = { closing = t; closeState = "dropped"; closeNote = "" },
                    onSnooze = {
                        // Snooze to tomorrow morning, not "+24h": the point is to be quiet
                        // overnight and be back before the 06:00 digest, and "+24h" from an
                        // evening tap would skip that briefing entirely.
                        cm.snoozeTask(t.id, nextMorning().toString(), "snoozed from My Day")
                    },
                    onOpen = {
                        // Discuss in the ASSISTANT thread, not a per-task one: assistant-state's
                        // beliefs and attention are global, so a task thread would talk to a
                        // model with no context about any of it. The item id rides along in the
                        // opening line — that is what lets the assistant call `state_items` /
                        // `state_close` on THIS item instead of guessing which one you mean.
                        cm.pendingDraft.value = "About item ${t.id} (${t.title.take(80)}): "
                        cm.openAssistant()
                        toChat()
                    },
                )
            }

            if (snoozed.isNotEmpty()) {
                item { SectionHeader("Snoozed", snoozed.size) }
                items(snoozed, key = { "sn-${it.id}" }) { t ->
                    TaskRow(
                        t,
                        snoozed = true,
                        onDone = { closing = t; closeState = "done"; closeNote = "" },
                        onDrop = { closing = t; closeState = "dropped"; closeNote = "" },
                        onSnooze = {
                            // "Wake" is a snooze into the past: the server's own wake pass
                            // (rank/maintain) flips it back to open, and until that runs the
                            // row already reads as open here because its until has passed.
                            cm.snoozeTask(t.id, Instant.now().toString(), "brought back from My Day")
                        },
                        onOpen = {
                            cm.pendingDraft.value = "About item ${t.id} (${t.title.take(80)}): "
                            cm.openAssistant()
                            toChat()
                        },
                    )
                }
            }

            val done = cm.doneTasks.value
            item { SectionHeader("Recently done", done.size) }
            items(done, key = { "done-${it.id}" }) { t -> DoneRow(t) }
            item { Spacer(Modifier.height(24.dp)) }
        }
    }

    closing?.let { t ->
        val isDone = closeState == "done"
        AlertDialog(
            onDismissRequest = { closing = null },
            title = { Text(if (isDone) "Done: ${t.title.take(60)}" else "Not real: ${t.title.take(60)}") },
            text = {
                Column {
                    Text(
                        if (isDone) "One line on what happened. The assistant records it as the outcome, " +
                            "which is what keeps the next briefing from raising this again."
                        else "One line on why this was never a real task. The assistant learns from it and " +
                            "won't raise it again.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.outline,
                    )
                    Spacer(Modifier.height(12.dp))
                    OutlinedTextField(
                        value = closeNote,
                        onValueChange = { closeNote = it },
                        label = { Text("Outcome") },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
            },
            confirmButton = {
                TextButton(
                    enabled = closeNote.isNotBlank(),
                    onClick = {
                        cm.closeTask(t.id, closeState, closeNote.trim())
                        closing = null
                    },
                ) { Text(if (isDone) "Close it" else "Drop it") }
            },
            dismissButton = { TextButton(onClick = { closing = null }) { Text("Cancel") } },
        )
    }
}

/** The assistant thread's entry point: who it is, how fresh its last briefing is, and a
 *  one-line composer so a thought does not need a navigation to be written down. */
@Composable
private fun AssistantCard(
    cm: ConnectionManager,
    draft: String,
    onChange: (String) -> Unit,
    onOpen: () -> Unit,
    onSend: (String) -> Unit,
) {
    val thread = cm.sessions.value.firstOrNull { it.sessionId == cm.assistantSessionId() }
    // Briefing health, same rule as the chat header: green while the hourly job is keeping
    // up, amber when it is late, red when it has stopped. Overnight the gap is expected.
    //
    // The push stamp (`lastBriefingAt`) only exists once a briefing push has ARRIVED — on a
    // device that has never had one it reads 0, which painted a red dot and "briefed never"
    // over a perfectly healthy assistant. The thread's own `updatedAt` is the honest
    // fallback (briefings append to it, and nothing else moves it while you are idle), so
    // take whichever is newer and say NOTHING when neither is known.
    val threadStamp = thread?.updatedEpoch() ?: 0L
    val lastBriefing = maxOf(cm.store.lastBriefingAt, threadStamp)
    val ageMin = if (lastBriefing > 0) (System.currentTimeMillis() - lastBriefing) / 60000 else Long.MAX_VALUE
    val activeHrs = java.time.ZonedDateTime.now().hour in 7..22
    val dot = when {
        lastBriefing <= 0 -> null
        ageMin <= (if (activeHrs) 90 else 12 * 60) -> MaterialTheme.statusColors.online
        ageMin <= (if (activeHrs) 180 else 15 * 60) -> MaterialTheme.statusColors.connecting
        else -> MaterialTheme.colorScheme.error
    }
    val lastSeen = if (lastBriefing > 0) relativeTime(Instant.ofEpochMilli(lastBriefing).toString()) else ""
    // The thread's own last line, when the session list knows it: the drawer already
    // carries session/list's snippet, so the card needs no extra round trip.
    val snippet = thread?.snippet.orEmpty()

    Card(shape = RoundedCornerShape(16.dp)) {
        Column(Modifier.fillMaxWidth().padding(14.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Box {
                    Icon(Icons.Filled.Psychology, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
                    // No stamp, no dot: an unknown age is not the same as a stale one.
                    dot?.let {
                        Surface(
                            color = it,
                            shape = RoundedCornerShape(50),
                            modifier = Modifier.size(8.dp).align(Alignment.TopEnd),
                        ) {}
                    }
                }
                Spacer(Modifier.width(10.dp))
                Column(Modifier.weight(1f)) {
                    Text("Assistant", style = MaterialTheme.typography.titleSmall)
                    Text(
                        if (lastSeen.isBlank()) "always on" else "always on · briefed $lastSeen",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.outline,
                    )
                }
                TextButton(onClick = onOpen) { Text("Open") }
            }
            if (snippet.isNotBlank()) {
                Spacer(Modifier.height(6.dp))
                Text(
                    snippet,
                    style = MaterialTheme.typography.bodySmall,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Spacer(Modifier.height(8.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(
                    value = draft,
                    onValueChange = onChange,
                    placeholder = { Text("Message the assistant") },
                    singleLine = true,
                    modifier = Modifier.weight(1f),
                    shape = RoundedCornerShape(12.dp),
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                    keyboardActions = KeyboardActions(onSend = { if (draft.isNotBlank()) onSend(draft) }),
                )
                Spacer(Modifier.width(6.dp))
                IconButton(onClick = { if (draft.isNotBlank()) onSend(draft) }, enabled = draft.isNotBlank()) {
                    Icon(Icons.AutoMirrored.Filled.Send, contentDescription = "send")
                }
            }
        }
    }
}

@Composable
private fun SectionHeader(title: String, count: Int) {
    Row(Modifier.fillMaxWidth().padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        Text(title, style = MaterialTheme.typography.titleSmall)
        if (count > 0) {
            Spacer(Modifier.width(8.dp))
            Text("$count", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.outline)
        }
    }
}

@Composable
private fun EmptyRow(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.outline,
        modifier = Modifier.padding(vertical = 8.dp),
    )
}

/** One open item. The body taps through to the assistant thread; the three actions are the
 *  whole vocabulary the server acts on (close / snooze / it was never real). A snoozed row
 *  is the same card with its wake time in the meta line and Snooze swapped for Wake —
 *  otherwise the two states are indistinguishable, which is the bug this section exists for. */
@Composable
private fun TaskRow(
    t: TaskItem,
    snoozed: Boolean = false,
    onDone: () -> Unit,
    onDrop: () -> Unit,
    onSnooze: () -> Unit,
    onOpen: () -> Unit,
) {
    val meta = listOfNotNull(
        wakeLabel(t.snoozeUntil).takeIf { snoozed && it.isNotBlank() },
        threadName(t.thread).takeIf { it.isNotBlank() },
        dueLabel(t.dueAt).takeIf { it.isNotBlank() },
        t.kind.takeIf { it.isNotBlank() && it != "commitment" },
    ).joinToString(" · ")
    Card(
        shape = RoundedCornerShape(12.dp),
        onClick = onOpen,
        // Asleep reads as asleep: a snoozed row sits in its own section AND is muted, so the
        // tap that put it there is visible without reading the button label back.
        colors = if (snoozed) {
            CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.35f))
        } else {
            CardDefaults.cardColors()
        },
    ) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 10.dp)) {
            Text(t.title, style = MaterialTheme.typography.bodyMedium, maxLines = 3, overflow = TextOverflow.Ellipsis)
            if (t.detail.isNotBlank()) {
                Spacer(Modifier.height(2.dp))
                Text(
                    t.detail,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.outline,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            if (meta.isNotBlank()) {
                Spacer(Modifier.height(4.dp))
                Text(meta, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.outline)
            }
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                TextButton(onClick = onDone, contentPadding = PaddingValues(horizontal = 8.dp)) { Text("Done") }
                TextButton(onClick = onSnooze, contentPadding = PaddingValues(horizontal = 8.dp)) {
                    Text(if (snoozed) "Wake" else "Snooze")
                }
                TextButton(onClick = onDrop, contentPadding = PaddingValues(horizontal = 8.dp)) { Text("Not real") }
            }
        }
    }
}

/** One handed-over task. Read-only on purpose: the state moves from the server
 *  (a turn updating it), and the action a person takes is to go and look at what
 *  came back — so the whole card taps through to the session that did the work
 *  rather than offering buttons that would need an intent the core does not have. */
@Composable
private fun HandedRow(t: AgentTask, onOpen: () -> Unit) {
    val reviewing = t.state == "review"
    val meta = listOfNotNull(
        if (reviewing) "ready for review" else "working",
        t.thread.takeIf { it.isNotBlank() }?.let(::threadName),
        artifactsLabel(t.artifacts),
    ).joinToString(" · ")
    Card(
        shape = RoundedCornerShape(12.dp),
        onClick = onOpen,
        colors = if (reviewing) {
            CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.primaryContainer)
        } else {
            CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.35f))
        },
    ) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 10.dp)) {
            Text(t.title, style = MaterialTheme.typography.bodyMedium, maxLines = 2, overflow = TextOverflow.Ellipsis)
            // The line it came back with is the point of the row once it is done;
            // while it runs, what was asked for is the only honest thing to show.
            val body = if (t.result.isNotBlank()) t.result else t.request
            if (body.isNotBlank()) {
                Spacer(Modifier.height(2.dp))
                Text(
                    body,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Spacer(Modifier.height(4.dp))
            Text(meta, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.outline)
        }
    }
}

/** "2 files · deck.pdf" — a count plus the first name, because a task's value is
 *  usually a file and the full path list belongs in the thread, not in a meta line. */
private fun artifactsLabel(paths: List<String>): String {
    if (paths.isEmpty()) return ""
    val first = paths.first().substringAfterLast('/')
    return if (paths.size == 1) first else "${paths.size} files · $first"
}

/** Whether this row is asleep right now. A snooze whose `until` has passed IS open — the
 *  server wakes it on its next rank pass, and until then it is still quiet only if `until`
 *  is in the future. An unparseable stamp reads as snoozed: keep it quiet rather than
 *  shout about something the user asked not to be shown. */
private fun TaskItem.isSnoozedAt(nowMs: Long): Boolean {
    if (state != "snoozed") return false
    val until = runCatching { Instant.parse(snoozeUntil) }.getOrNull() ?: return true
    return until.toEpochMilli() > nowMs
}

/** The thread label as a reader wants it. The server's labels are normalised slugs
 *  (`chami-billing`), so hyphens become spaces and the first letter is capitalised — there is
 *  nothing to look up, because a label is all a thread is. An item with no label shows none
 *  rather than an empty segment in the meta line. */
private fun threadName(label: String): String {
    val words = label.trim().replace('-', ' ').replace('_', ' ').trim()
    if (words.isEmpty()) return ""
    return words.replaceFirstChar { it.titlecase() }
}

/** When a snoozed row comes back: "wakes in 6h" / "wakes tomorrow 05:00". Short on purpose —
 *  it shares a line with the thread and the due date. */
private fun wakeLabel(iso: String): String {
    if (iso.isBlank()) return ""
    val until = runCatching { Instant.parse(iso) }.getOrNull() ?: return ""
    val zone = java.time.ZoneId.systemDefault()
    val mins = java.time.Duration.between(Instant.now(), until).toMinutes()
    if (mins <= 0) return "wakes now"
    val local = until.atZone(zone)
    return when {
        mins < 90 -> "wakes in ${mins}m"
        mins < 12 * 60 -> "wakes in ${mins / 60}h"
        java.time.LocalDate.now(zone) == local.toLocalDate() -> "wakes ${local.hour}:%02d".format(local.minute)
        else -> "wakes tomorrow ${local.hour}:%02d".format(local.minute)
    }
}

/** The next 05:00 local, which is the point of a snooze from this tab: quiet overnight and
 *  back before the 06:00 morning digest. A flat "+24h" from an evening tap would land AFTER
 *  that digest and silently skip a whole day of the assistant's attention. */
private fun nextMorning(): Instant {
    val zone = java.time.ZoneId.systemDefault()
    val now = java.time.ZonedDateTime.now(zone)
    var wake = now.toLocalDate().atTime(5, 0).atZone(zone)
    if (wake.isBefore(now.plusHours(1))) wake = wake.plusDays(1)
    return wake.toInstant()
}

/** `due_at` arrives as a bare date ("2026-07-05") or a naive datetime ("2026-10-15T15:30").
 *  `relativeTime` parses neither, so it printed the raw string; what a reader needs is
 *  whether it is overdue and by how much. Past days dominate this list, hence "overdue". */
internal fun dueLabel(iso: String): String {
    if (iso.isBlank()) return ""
    val date = runCatching {
        runCatching { java.time.LocalDate.parse(iso.take(10)) }
            .recoverCatching { java.time.OffsetDateTime.parse(iso).toLocalDate() }
            .recoverCatching { java.time.LocalDateTime.parse(iso).toLocalDate() }
            .getOrThrow()
    }.getOrNull() ?: return "due ${iso.take(10)}"
    // `DAYS.between(a, b)` is b - a, so NOW must come first for a future due date to be
    // positive. With the arguments the other way round every branch inverted: an item due in
    // a week read "overdue 7d" and the flu shot, 15 days past, read "due in 15d".
    val days = java.time.temporal.ChronoUnit.DAYS.between(java.time.LocalDate.now(), date)
    return when {
        days <= -14 -> "overdue ${-days / 7}w"
        days < 0 -> "overdue ${-days}d"
        days == 0L -> "due today"
        days == 1L -> "due tomorrow"
        days <= 30 -> "due in ${days}d"
        else -> "due ${date.month.getDisplayName(java.time.format.TextStyle.SHORT, java.util.Locale.getDefault())} ${date.dayOfMonth}"
    }
}

/** A closed item: struck through, with the resolution line that closed it. Read-only — the
 *  history is the answer to "did I actually deal with that", so it is not there to be edited. */
@Composable
private fun DoneRow(t: TaskItem) {
    Column(Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 4.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(
                Icons.Filled.Check,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.outline,
                modifier = Modifier.size(14.dp),
            )
            Spacer(Modifier.width(6.dp))
            Text(
                t.title,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.outline,
                textDecoration = TextDecoration.LineThrough,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        if (t.resolution.isNotBlank()) {
            Text(
                t.resolution,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.outline,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.padding(start = 20.dp),
            )
        }
    }
}
