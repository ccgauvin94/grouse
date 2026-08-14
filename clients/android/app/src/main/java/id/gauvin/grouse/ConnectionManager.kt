package id.gauvin.grouse

import android.content.Context
import android.content.Intent
import android.os.Handler
import android.os.Looper
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put
import uniffi.grouse_core.ConfigChoice
import uniffi.grouse_core.ConfigOption as CoreConfigOption
import uniffi.grouse_core.ConnectionStatus
import uniffi.grouse_core.Core
import uniffi.grouse_core.CoreListener
import uniffi.grouse_core.GrouseUnstable
import uniffi.grouse_core.GrouseUnstableListener
import uniffi.grouse_core.Message
import uniffi.grouse_core.PermissionOutcome
import uniffi.grouse_core.PermissionRequest
import uniffi.grouse_core.PermissionOption
import uniffi.grouse_core.ProjectSummary
import uniffi.grouse_core.Prompt
import uniffi.grouse_core.PromptBlock
import uniffi.grouse_core.SendExpect
import uniffi.grouse_core.ServerConfig
import uniffi.grouse_core.SessionSummary
import uniffi.grouse_core.StreamEvent
import uniffi.grouse_core.ToolCallKind
import uniffi.grouse_core.TranscriptEvent
import uniffi.grouse_roam_core.cardFingerprint
import uniffi.grouse_roam_core.identityGenerate
import uniffi.grouse_roam_core.identityPublicKey

/** The three drawer session categories. See ConnectionManager.sessionKind(). */
enum class SessionKind { ASSISTANT, CHAT, CODE }

/**
 * Process-scoped owner of the grouse-core connection + chat state. A singleton (not a
 * ViewModel) so it survives navigation/config changes and can later be shared with a
 * background service, share intents, tiles, etc. Compose observes its snapshot state directly.
 *
 * THE MODEL: the Rust core owns the connection, the session list, the active session's
 * transcript, caches, reconnect/backoff, and remote-change resync (CONTRACT). This controller
 * mirrors the core's events into the app's state holders and translates its records into the
 * app DTOs (Dtos.kt). It never reimplements client logic.
 *
 * Threading: the uniffi listeners fire on the core's worker thread; every callback marshals
 * onto the main looper before touching Compose state.
 */
class ConnectionManager private constructor(context: Context) {
    val store = SecureStore(context)
    private val appContext = context.applicationContext
    private val notifier = Notifier(context)
    private var appForeground = true
    private var serviceRunning = false
    // Sends that must wait for (re)connect — a queue, not one slot, so a second reply while
    // still connecting can't clobber the first. The user bubble is added when queued (in send()).
    private data class PendingSend(val text: String, val images: List<ImageBlock>)
    private val pendingSends = ArrayDeque<PendingSend>()

    /** How many prompts are waiting behind the running turn. Drives the "N queued" chip -- without
     *  it a queued message is indistinguishable from a dropped one, since its bubble looks exactly
     *  like a sent one. Mutate the deque ONLY through enqueue/dequeue/clearQueue so this can't drift. */
    val queuedCount = mutableStateOf(0)
    private fun enqueue(p: PendingSend) { pendingSends.add(p); queuedCount.value = pendingSends.size }
    private fun dequeue(): PendingSend? =
        (if (pendingSends.isEmpty()) null else pendingSends.removeFirst()).also { queuedCount.value = pendingSends.size }
    private fun clearQueue() { pendingSends.clear(); queuedCount.value = 0 }
    // True between sendPrompt and RunEnded. `busy` is UI state and is also set while merely
    // queued, so it cannot answer "is the wire busy" -- this can.
    private var turnInFlight = false

    // --- grouse-core (the wire is the core's; these are the only handles we hold) ----------
    // The core owns the socket, reconnect/backoff, the transcript, and the caches. `unstable`
    // is the _goose/unstable/* shim (recipes, schedules, skills, extensions, tools, config).
    private val core = Core(object : CoreListener {
        override fun onStatus(status: ConnectionStatus) { main.post { onCoreStatus(status) } }
        override fun onSessions(sessions: List<SessionSummary>) { main.post { onCoreSessions(sessions) } }
        override fun onTranscript(event: TranscriptEvent) { main.post { onCoreTranscript(event) } }
        override fun onStream(event: StreamEvent) { main.post { onCoreStream(event) } }
        override fun onConfig(options: List<CoreConfigOption>) { main.post { onCoreConfig(options) } }
        override fun onPermissionRequest(request: PermissionRequest) { main.post { onCorePermission(request) } }
        override fun onSessionTouched(sessionId: String, title: String, updatedAt: String) {
            main.post { onCoreSessionTouched(sessionId, title, updatedAt) }
        }
        override fun onProjects(projects: List<ProjectSummary>) { main.post { onCoreProjects(projects) } }
        override fun onRoamPeerStatus(label: String, status: String) { main.post { onRoamPeerStatus(label, status) } }
        override fun onRoamSessions(label: String, sessions: List<SessionSummary>) {
            main.post { onRoamSessions(label, sessions) }
        }
        override fun onActiveRun(sessionId: String, runId: String) {
            main.post { onCoreActiveRun(sessionId, runId) }
        }
        override fun onCommands(commands: List<String>) {
            main.post { this@ConnectionManager.commands.value = commands }
        }
    })
    private val unstable = GrouseUnstable(object : GrouseUnstableListener {
        override fun onExport(data: String) { main.post { exportData.value = data } }
        override fun onRecipeParams(parameters: String) { main.post { onRecipeParams(parameters) } }
        override fun onElicitation(schema: String) { main.post { onElicitation(schema) } }
        override fun onCompactionStatus(message: String) { main.post { onCompactionStatus(message) } }
        override fun onMessageUsage(outputTokens: ULong, elapsedMs: ULong, timeToFirstTokenMs: ULong, cost: Double) {
            main.post { onMessageUsage(outputTokens, elapsedMs, timeToFirstTokenMs, cost) }
        }
        override fun onAppResource(key: String, html: String) { main.post { onAppResource(key, html) } }
        override fun onRecipes(recipes: String) { main.post { onRecipes(recipes) } }
        override fun onSchedules(schedules: String) { main.post { onSchedules(schedules) } }
        override fun onProjects(projects: String) { main.post { onUnstableProjects(projects) } }
        override fun onSkills(skills: String) { main.post { onSkills(skills) } }
        override fun onTools(sessionId: String, tools: String) { main.post { onTools(sessionId, tools) } }
        override fun onExtensions(extensions: String) { main.post { onExtensions(extensions) } }
        override fun onSessionExtensions(sessionId: String, extensions: String) {
            main.post { onSessionExtensions(sessionId, extensions) }
        }
        override fun onConfigValue(key: String, value: String) { main.post { onConfigValue(key, value) } }
        override fun onSupportedModels(provider: String, models: String) {
            main.post { onSupportedModels(provider, models) }
        }
        override fun onSessionProbe(sessionId: String, updatedAt: String, messageCount: Long) {
            // The core owns resync now (probe → in-place replay); the app never probes.
        }
        override fun onToolResult(text: String, isError: Boolean) { main.post { onToolResult(text, isError) } }
        override fun onError(method: String, message: String) { main.post { onUnstableError(method, message) } }
    })

    val messages = mutableStateListOf<ChatMessage>()
    val status = mutableStateOf("not connected")
    val online = mutableStateOf(false)   // true between Ready and disconnect — for a UI status pill
    val config = mutableStateOf<List<ConfigOption>>(emptyList())
    val sessions = mutableStateOf<List<SessionInfo>>(emptyList())

    /** Goose projects, refreshed alongside the session list. A project is a named source with an
     *  id, not a directory -- so filing a chat no longer decides where its tools run, and the
     *  same project is one entry from every client instead of one per cwd spelling. */
    val projects = mutableStateOf<List<ProjectInfo>>(emptyList())

    /** The server's cron table, and the recipe library the jobs run from. Both are server state
     *  with no local mirror -- every mutation re-lists rather than patching, because paused and
     *  running move without the client asking. */
    val schedules = mutableStateOf<List<ScheduleInfo>>(emptyList())
    val recipes = mutableStateOf<List<RecipeInfo>>(emptyList())

    /** The recipe a job runs, matched by file path -- schedules/list gives a path and
     *  recipes/list gives a path, and nothing gives an id linking them. Null for a job created
     *  with an inline recipe, which the library never saw. */
    fun recipeFor(job: ScheduleInfo): RecipeInfo? =
        recipes.value.firstOrNull { it.filePath.isNotEmpty() && it.filePath == job.source }

    fun refreshSchedules() {
        io { unstable.schedulesList(); unstable.recipesList() }
    }

    fun setSchedulePaused(id: String, paused: Boolean) {
        io { if (paused) unstable.schedulesPause(id) else unstable.schedulesUnpause(id) }
    }

    fun runScheduleNow(id: String) { io { unstable.schedulesRunNow(id) } }

    fun deleteSchedule(id: String) { io { unstable.schedulesDelete(id) } }

    fun setScheduleCron(id: String, cron: String) { io { unstable.schedulesUpdate(id, cron) } }

    fun setRecipeCron(recipeId: String, cron: String?) { io { unstable.recipesSchedule(recipeId, cron) } }

    fun deleteRecipe(recipeId: String) { io { unstable.recipesDelete(recipeId) } }

    /** Save an edited recipe. The caller hands back a full DTO derived from RecipeInfo.raw. */
    fun saveRecipe(recipeId: String, dto: JsonObject) { io { unstable.recipesSave(recipeId, dto.toString()) } }

    /** Replace one top-level string field, dropping it when blank. */
    fun recipeWith(r: RecipeInfo, field: String, value: String): JsonObject =
        JsonObject(r.raw.toMutableMap().apply {
            if (value.isBlank()) remove(field) else put(field, JsonPrimitive(value))
        })

    /** Replace one `settings:` key, creating or pruning the settings block as needed. An empty
     *  settings object is removed rather than left behind: goose treats a present-but-empty
     *  block differently from an absent one in some paths. */
    fun recipeWithSetting(r: RecipeInfo, key: String, value: String): JsonObject {
        val settings = ((r.raw["settings"] as? JsonObject)?.toMutableMap() ?: mutableMapOf())
        if (value.isBlank()) settings.remove(key) else settings[key] = JsonPrimitive(value)
        return JsonObject(r.raw.toMutableMap().apply {
            if (settings.isEmpty()) remove("settings") else put("settings", JsonObject(settings))
        })
    }

    /** Skills: the tool-usage guides goose pulls in with load_skill. Server state, listed on
     *  demand -- they change rarely and there is no notification when they do. */
    val skills = mutableStateOf<List<SkillInfo>>(emptyList())

    fun refreshSkills() { io { unstable.sourcesList("skill") } }

    fun saveSkill(s: SkillInfo, content: String) {
        io { unstable.sourcesUpdate("skill", s.path, s.name, s.description, content) }
    }

    fun deleteSkill(path: String) { io { unstable.sourcesDelete("skill", path) } }

    fun sessionsByProject(): List<Pair<String, List<SessionInfo>>> {
        val byName = projects.value.associate { it.id to it.name }
        return sessions.value
            .groupBy { it.projectId ?: "" }
            .entries
            .sortedWith(compareBy({ it.key.isEmpty() }, { -(it.value.maxOfOrNull { s -> s.updatedAt } ?: "").hashCode() }))
            .map { (id, list) ->
                val label = if (id.isEmpty()) "Unfiled" else (byName[id] ?: id)
                label to list.sortedByDescending { it.updatedAt }
            }
    }

    fun refreshProjects() { io { unstable.sourcesList("project") } }

    /** Start a chat already filed under [projectId].
     *
     *  Filing happens once the server hands back a session id -- session/new has no
     *  projectId parameter. The core decides the new session's cwd (the connect-time cwd). */
    fun newChatInProject(projectId: String, cwd: String? = null) {
        pendingProjectFiling = projectId
        newSession(cwd = cwd ?: store.workingDir, kind = SessionKind.CHAT)
    }

    /** Set while a new-chat-in-project is in flight; consumed when Ready delivers the id. */
    private var pendingProjectFiling: String? = null
    fun fileSession(sessionId: String, projectId: String?) {
        io { unstable.sessionProject(sessionId, projectId) }
        sessions.value = sessions.value.map {
            if (it.sessionId == sessionId) it.copy(projectId = projectId) else it
        }
    }
    // recentProjects (a locally-remembered list of typed project names) was REMOVED 2026-08-01.
    // It padded the drawer with names the server had never heard of, so a project deleted
    // months ago still rendered -- "Media" outlived its sessions, its directory and its server
    // entry purely because this list remembered it. cm.projects is now the only source.
    val currentSession = mutableStateOf<String?>(null)   // id of the session on screen (for the Assistant binding)
    val busy = mutableStateOf(false)
    val usage = mutableStateOf<AcpEvent.Usage?>(null)   // context window used/size + cost
    // True while compaction (manual /compact or server-triggered auto-compact) is running. The
    // protocol only ever sends discrete text status lines, never a numeric percentage, so this
    // drives an INDETERMINATE indicator, not a real progress fraction.
    val compacting = mutableStateOf(false)
    val commands = mutableStateOf<List<String>>(emptyList())
    val permissions = mutableStateListOf<AcpEvent.Permission>()   // pending approvals, oldest first
    val elicitations = mutableStateListOf<AcpEvent.Elicitation>() // pending input forms, oldest first
    // Last background RPC failure (sidebar refresh, config read...). Shown as a toast by the
    // UI and cleared; never a transcript bubble.
    val backgroundNotice = mutableStateOf<String?>(null)
    // A session/export result waiting for the UI to hand to the Android share sheet.
    val exportData = mutableStateOf<String?>(null)
    // Handed in by OS entry points (share sheet, shortcut, tile), consumed by the UI.
    val pendingShareText = mutableStateOf<String?>(null)
    val pendingShareImages = mutableStateListOf<ImageBlock>()
    val pendingNewChat = mutableStateOf(false)
    // A notification tap that should open a specific session (finished-turn alert). Consumed in MainActivity.
    val pendingOpenSession = mutableStateOf<String?>(null)
    // Draft attachments live here (process-scoped) so a rotation/recreation doesn't drop picked
    // images -- and base64 payloads stay out of the saved-state Bundle (TransactionTooLarge).
    val draftAttachments = mutableStateListOf<ImageBlock>()
    val dynamicColor = mutableStateOf(store.dynamicColor)
    val showAllProviders = mutableStateOf(store.showAllProviders)
    // Live model list for the CURRENT provider, from the server, in memory only -- never
    // persisted.
    val knownModels = mutableStateOf(emptySet<String>())
    // Guards supportedModels() to fire once per provider per connection, not on every Config
    // event (which fires on every option change, not just provider switches).
    private var liveModelsFetchedFor: String? = null
    val extensions = mutableStateOf<List<ExtInfo>>(emptyList())
    val extensionsBusy = mutableStateOf(false)
    // Names of the CURRENT session's enabled extensions -- drives the in-chat "N tools" indicator
    // and its management sheet. Refreshed on every session open (Ready); optimistically updated by
    // toggleSessionExtension since add/remove replies are empty (no server re-list to react to).
    val sessionExtensionNames = mutableStateOf<List<String>>(emptyList())
    // Full extension objects for the CURRENT session (same reply as the names above).
    val sessionExtensionInfos = mutableStateOf<List<ExtInfo>>(emptyList())
    // Peer extensions toggled OFF this session, kept so their row (and DTO) survives to be
    // toggled back on. Cleared on session open; local sessions never need it because their
    // rows come from the local global catalog.
    val detachedPeerExts = mutableStateOf<List<ExtInfo>>(emptyList())
    // Tools ACTIVE in the current session, grouped extension -> tool names (the `ext__tool` prefix
    // goose uses, stripped). Reflects available_tools filtering, so it is the "checked" set.
    val sessionTools = mutableStateOf<Map<String, List<String>>>(emptyMap())
    // Full tool catalogue per extension, i.e. what you'd get with no allowlist. Not obtainable
    // directly -- goose has no per-extension tools endpoint -- so it is discovered on demand by
    // discoverTools() and cached here for the process lifetime. Absent = not discovered yet.
    val toolCatalog = mutableStateOf<Map<String, List<String>>>(emptyMap())
    // Extension whose full catalogue is being discovered; its tools/list reply is the catalogue,
    // not the live set, so the Tools handler must not treat it as sessionTools.
    private var discovering: ExtInfo? = null

    /** toolCatalog key. A peer's extension can share a name with a local one while exposing a
     *  different tool set, so peer-sourced entries are namespaced by the owning peer. */
    fun catKey(e: ExtInfo): String =
        if (e.fromPeer) "peer:${roamPeer(currentSession.value)}:${e.name}" else e.name

    /** Discovered full tool set for this extension, or null if not discovered yet. */
    fun catalogOf(e: ExtInfo): List<String>? = toolCatalog.value[catKey(e)]

    /** True when a session-scoped tool operation with this ExtInfo would be unsound: the
     *  current session lives on a peer but the DTO is local (or vice versa). Callers must
     *  no-op rather than push it. */
    private fun wrongNode(e: ExtInfo): Boolean =
        (roamPeer(currentSession.value) != null) != e.fromPeer
    // Per-message generation stats (tok/s, cost) for the most recently finished assistant reply.
    // Cleared when a new turn starts so stale numbers don't linger under the next streaming bubble.
    val lastMessageUsage = mutableStateOf<AcpEvent.MessageUsage?>(null)

    /** Fetch the extension list (agent-global). Reply lands as onExtensions.
     *  goose ≥1.42 dropped goosed's REST /config/extensions; this uses the ACP method instead. */
    fun loadExtensions() {
        if (!live) { extensions.value = emptyList(); extensionsBusy.value = false; return }
        extensionsBusy.value = true
        io { unstable.listGlobalExtensions() }
    }

    /** Group `ext__tool` names into ext -> [tool]. ONLY mcp-type extensions namespace their tools
     *  this way: developer's are bare (`shell`, `edit`, `tree`), summon's is `delegate`, skills' is
     *  `load_skill`. So an extension with no matching prefix is not "zero tools", it is "cannot be
     *  attributed" -- see toolsAttributable(). Grouping bare names under a bucket and showing that
     *  as a count is what made every builtin read 0. */
    private fun group(names: List<String>): Map<String, List<String>> =
        names.filter { it.contains("__") }
            .groupBy({ it.substringBefore("__") }, { it.substringAfter("__") })

    /** Whether per-tool control can be offered for this extension at all. Only mcp-backed ones
     *  namespace their tools, and only namespaced tools can be mapped back to an owner. */
    fun toolsAttributable(e: ExtInfo): Boolean = e.type == "mcp"

    /** Ask the server for this session's active tools; lands as onTools. The unstable surface
     *  routes to the MAIN connection only, so peer-owned sessions are skipped (see the roam note). */
    fun refreshTools() {
        discovering = null
        val sid = core.activeSessionId() ?: return
        if (roamPeer(currentSession.value) != null) return
        io { unstable.listTools(sid) }
    }

    /** Everything the in-chat tool sheet displays, refreshed together on open. */
    fun refreshSessionSheet() { refreshTools(); sessionExtensionsList() }

    private fun sessionExtensionsList() {
        val sid = core.activeSessionId() ?: return
        if (roamPeer(currentSession.value) != null) return
        io { unstable.sessionExtensionsList(sid) }
    }

    /** Discover an extension's FULL tool set. goose only reports ALLOWED tools, so the only way to
     *  see what an allowlist is hiding is to briefly run the extension unfiltered: re-add it
     *  session-scoped with an empty available_tools, list, then put the real setting back. Entirely
     *  session-local -- config.yaml is untouched -- and self-healing, since the restore re-applies
     *  whatever the session should have. */
    fun discoverTools(ext: ExtInfo) {
        // Catalogue is cached for the process lifetime, but sessionTools is NOT reliably fresh:
        // MCP extensions attach asynchronously after Ready, and the two listTools polls (0s/2.5s)
        // can both miss a slow one. Expanding a cached row used to early-return without any
        // refresh, so the sheet rendered the PREVIOUS session's tool state after a global-default
        // change -- "the session tools list isn't accurate". Refresh cheaply instead.
        if (toolCatalog.value.containsKey(catKey(ext))) { refreshTools(); return }
        if (wrongNode(ext)) { refreshTools(); return }
        val sid = core.activeSessionId() ?: return
        if (roamPeer(currentSession.value) != null) { refreshTools(); return }
        val unfiltered = JsonObject(ext.raw.toMutableMap().apply {
            put("available_tools", JsonArray(emptyList()))
        })
        discovering = ext
        io {
            unstable.sessionExtensionsRemove(sid, ext.name)
            unstable.sessionExtensionsAdd(sid, toExtensionDto(unfiltered).toString())
        }
        // the add re-lists tools + session extensions (core side) -- see onTools
    }

    /** Restrict `ext` to `allowed` for THIS session only (no config.yaml write). Empty = all. */
    fun setSessionTools(ext: ExtInfo, allowed: Set<String>) {
        if (wrongNode(ext)) return
        val sid = core.activeSessionId() ?: return
        val full = catalogOf(ext).orEmpty()
        // An allowlist equal to the whole catalogue is the same as no allowlist, and storing []
        // keeps it that way if the extension later gains tools.
        val list = if (allowed.size >= full.size && full.isNotEmpty()) emptyList() else allowed.toList()
        val scoped = JsonObject(ext.raw.toMutableMap().apply {
            put("available_tools", JsonArray(list.map { JsonPrimitive(it) }))
        })
        discovering = null
        io {
            unstable.sessionExtensionsRemove(sid, ext.name)
            unstable.sessionExtensionsAdd(sid, toExtensionDto(scoped).toString())
        }
    }

    /** Save `allowed` as the GLOBAL default for `ext` (config.yaml; applies to new chats). */
    fun setDefaultTools(ext: ExtInfo, allowed: Set<String>) {
        val full = toolCatalog.value[ext.name].orEmpty()
        val list = if (allowed.size >= full.size && full.isNotEmpty()) emptyList() else allowed.toList()
        val updated = JsonObject(ext.raw.toMutableMap().apply {
            put("available_tools", JsonArray(list.map { JsonPrimitive(it) }))
        })
        extensionsBusy.value = true
        io { unstable.addExtension(toExtensionDto(updated).toString(), ext.enabled) }
    }

    /** Enable/disable an extension globally (affects new chats); the reply refreshes the list. */
    fun toggleExtension(e: ExtInfo, enabled: Boolean) {
        extensionsBusy.value = true
        // The core's set-enabled takes the extension NAME (its wire param is `name`, not the
        // config.yaml key the old client sent).
        io { unstable.setExtensionEnabled(e.name, enabled) }
    }

    /** Enable/disable one extension for just THIS session (session-scoped API — never touches
     *  config.yaml or any other open session). Optimistic: add/remove replies are empty, so
     *  sessionExtensionNames is updated immediately rather than waiting on a re-list. */
    fun toggleSessionExtension(e: ExtInfo, enabled: Boolean) {
        if (wrongNode(e)) return
        val sid = core.activeSessionId() ?: return
        if (enabled) {
            io { unstable.sessionExtensionsAdd(sid, toExtensionDto(e.raw).toString()) }
            sessionExtensionNames.value = sessionExtensionNames.value + e.name
            detachedPeerExts.value = detachedPeerExts.value.filterNot { it.name == e.name }
        } else {
            io { unstable.sessionExtensionsRemove(sid, e.name) }
            sessionExtensionNames.value = sessionExtensionNames.value - e.name
            // Keep the peer DTO so the row survives to be re-enabled; the peer's global
            // catalog can't be listed, so a dropped row would be gone until reopen.
            if (e.fromPeer) detachedPeerExts.value = detachedPeerExts.value + e
        }
    }

    // Providers actually set up on this goose (config.yaml `providers:` with configured:true).
    // Unconfigured catalog entries are hidden unless showAllProviders is on.
    val configuredProviders = setOf("openai", "openrouter")

    fun setDynamicColor(v: Boolean) { store.dynamicColor = v; dynamicColor.value = v }
    fun setShowAllProviders(v: Boolean) { store.showAllProviders = v; showAllProviders.value = v }

    private val main = Handler(Looper.getMainLooper())
    // The unstable shim's intents are SYNCHRONOUS RPCs (round-trip the server on the
    // calling thread) — unlike the core's own async intents. Calling them on the main
    // thread freezes the UI for the whole call (the drawer's refreshSidebar used to do
    // exactly that: opening the menu hung until session/list + sources/list returned, so
    // tapping Settings "did nothing"). Every blocking intent goes through this thread.
    private val intents = java.util.concurrent.Executors.newSingleThreadExecutor { r ->
        Thread(r, "grouse-intents").apply { isDaemon = true }
    }
    private fun io(block: () -> Unit) { intents.execute(block) }

    // MCP-App template cache: "$extension|$uri" -> HTML. Templates are static per server
    // version and shared across tools/messages/sessions, so one fetch serves everything —
    // including transcript replays, which re-emit every historical tool_call.
    private val appHtmlCache = java.util.concurrent.ConcurrentHashMap<String, String>()
    private val appFetchInFlight = java.util.concurrent.ConcurrentHashMap.newKeySet<String>()
    private var live = false
    private var connecting = false
    private var lastSessionId: String? = null
    // True when a Clear wiped `messages` while prompts were still queued (their bubbles are
    // not in the server history); the Ready handler re-adds them.
    private var replayWiped = false
    // Replay scroll-pinning: while the core rebuilds the transcript (Clear → chunks), the UI
    // pins unconditionally; the tick fires one final snap when Ready completes the rebuild.
    val replayActive = mutableStateOf(false)
    val replayDoneTick = mutableStateOf(0)
    // Replayed source messages counted so far — feeds the "Loading… N" title while a big
    // history streams in. Without it a long replay sat on a static "Connecting…" and looked
    // hung; the count proves it is advancing.
    val replayProgress = mutableStateOf(0)

    private val optionIds = listOf("provider", "model", "mode", "thinking_effort")

    /** The ServerConfig the app last handed the core (mirrors the core's own last_config, so
     *  this process knows whether a fresh `connect()` is needed before new/open intents). */
    private var lastConfig: ServerConfig? = null
    /** Set when the first `connect()` of the process (or a config change) created a transient
     *  session; the Ready handler then opens the real target. */
    private var pendingResumeAfterConnect: String? = null
    /** Saved provider/model/mode picks re-applied once per connection (the core does not
     *  re-apply them; the old client's applyDesired did). */
    private var desiredApplied = false

    val configured: Boolean get() = store.hasKey()

    /** Connect using the already-saved host/port/key (post-unlock auto-connect). */
    fun connectSaved() { if (store.hasKey()) open(resume = null) }

    // --- Roam (parallel iroh peers) -------------------------------------------
    // The CORE owns the peer registry (CONTRACT §6): `roam_connect(card, label)` dials a peer
    // in browse mode (sessions arrive as onRoamSessions), `roam_open_session(label, id)` makes
    // it the chat owner, `roam_disconnect(label)` closes it. The app only mirrors which peer
    // owns the chat (for the UI) and stores the pasted cards (the dialing identity is the
    // core's own, generated + persisted by the core).
    data class RoamPeer(val name: String, val card: String, val fingerprint: String)

    val roamPeers = mutableStateListOf<RoamPeer>()
    /** Name of the peer that owns the current chat, or null (main connection). */
    @Volatile var currentRoamPeer: String? = null
        private set

    fun loadRoamPeers() {
        roamPeers.clear()
        store.roamPeers.forEach { (n, c) ->
            roamPeers.add(RoamPeer(n, c, runCatching { cardFingerprint(c) }.getOrDefault("")))
        }
    }

    /** The device's iroh secret key, created once and held in SecureStore.
     *  DISPLAY ONLY: the core generates + persists its OWN roam identity on first use (its
     *  data dir is not reachable from the app), so the key a host sees in `peers list` at dial
     *  time is the core's, not this one. Kept so the pairing screen has a key to show. */
    fun roamIdentity(): String = store.roamIdentity
        ?: identityGenerate().also { store.roamIdentity = it }

    /** Hex public key — what a host sees in `peers list` before accepting. */
    val roamPublicKey: String
        get() = runCatching { identityPublicKey(roamIdentity()) }.getOrDefault("")

    /** Add a peer from its card; returns null on success, else the error text. */
    fun addRoamPeer(name: String, card: String): String? {
        val fp = try { cardFingerprint(card) } catch (t: Throwable) { return t.message ?: "invalid card" }
        store.roamPeers = store.roamPeers + (name to card)
        roamPeers.add(RoamPeer(name, card, fp))
        return null
    }

    fun removeRoamPeer(name: String) {
        store.roamPeers = store.roamPeers - name
        roamPeers.removeAll { it.name == name }
        if (currentRoamPeer == name) disconnectRoam()
    }

    fun disconnectRoam() {
        val peer = currentRoamPeer ?: return
        currentRoamPeer = null
        core.roamDisconnect(peer)
        sessions.value = sessions.value.filterNot { roamPeer(it.sessionId) == peer }
        if (roamPeer(currentSession.value) == peer) {
            messages.clear(); currentSession.value = null
        }
    }

    /** Dial a peer and bind the chat to it. `resume` is the peer-side session to open (last
     *  session on that peer, or the one the user just picked); null on first connect leaves the
     *  app sessionless until the user picks -- session/new would litter the host's session list.
     *  `createSession` (a new chat ON the peer) has no core intent -- the peer registry only
     *  opens EXISTING peer sessions (CONTRACT §6) -- so it degrades to browse: the peer's
     *  sessions are listed and the user picks one. */
    fun connectRoam(name: String, resume: String? = null, createSession: Boolean = false) {
        val peer = roamPeers.firstOrNull { it.name == name } ?: return
        currentRoamPeer = name
        // The core generates + persists the device identity itself; browse-mode dial.
        core.roamConnect(peer.card, name)
        if (resume != null) {
            messages.clear(); currentSession.value = resume
            core.roamOpenSession(name, resume)
        } else {
            messages.clear(); currentSession.value = null
            status.value = "connecting to ${peer.name}…"
            // its sessions arrive via on_roam_sessions
        }
    }

    /** Reconnect whatever is current: the roam peer (resuming the open session) or the WS host.
     *  Every reconnect call site routes through here so roam mode can't accidentally dial the WS
     *  path with a peer session id. */
    private fun reconnectToCurrent() {
        val peer = currentRoamPeer
        if (peer != null) connectRoam(peer, resume = lastSessionId)
        else open(resume = lastSessionId ?: store.lastSessionId)
    }

    // connectHome() re-enters composition every time the lock screen (or any recreation) swaps
    // AppRoot back in; only the FIRST call per process should land on the Assistant. Later calls
    // resume whatever session was open instead.
    private var homeOpened = false

    /** Fresh start: land directly on the privileged Assistant thread (its home). Uses the cached
     *  id to resume it with no churn; on first run (no cache) connects fresh and opens it once the
     *  list arrives. After the first call this only re-establishes the connection. */
    fun connectHome() {
        if (!store.hasKey()) return
        if (homeOpened) { ensureConnected(); return }
        homeOpened = true
        // Master switch off: connect to the last regular session instead of the Assistant.
        if (!store.assistantEnabled) { ensureConnected(); return }
        val a = store.assistantSessionId
        if (a != null) openSession(a, knownKind = SessionKind.ASSISTANT)
        else { pendingOpenAssistant = true; open(resume = null) }
    }

    /** Save new credentials and connect fresh (from the Connect screen). */
    fun connect(host: String, port: String, key: String, workingDir: String) {
        val (h, p) = normalizeHostPort(host, port)
        store.host = h; store.port = p; store.secretKey = key
        store.workingDir = workingDir
        lastSessionId = null; config.value = emptyList()
        open(resume = null)
    }

    /** Accept a host pasted in any shape and return a bare (host, port), because the socket URL is
     *  built as `wss://host:port/acp` with plain interpolation — a leftover `https://` or trailing
     *  `/acp` produces `wss://https://…` and silently never connects (this bit a real setup).
     *  Strips any scheme and path; if the host carried its own `:port` that wins over the field. */
    private fun normalizeHostPort(rawHost: String, rawPort: String): Pair<String, String> {
        var h = rawHost.trim()
            .replace(Regex("^[A-Za-z][A-Za-z0-9+.-]*://"), "")   // strip scheme (http/https/ws/wss)
            .substringBefore('/')                                 // drop any path
            .trim()
        var p = rawPort.trim()
        val colon = h.lastIndexOf(':')                            // host:port -> split (IPv4/host only)
        if (colon > 0) {
            val tail = h.substring(colon + 1)
            if (tail.isNotEmpty() && tail.all { it.isDigit() }) { p = tail; h = h.substring(0, colon) }
        }
        return h to p
    }

    /** Reconnect silently after Android drops the socket in the background. The core reconnects
     *  on its own (backoff, resume) — this only nudges the give-up case or the first connect. */
    fun ensureConnected() {
        if (!store.hasKey() || live || connecting) return
        val sid = lastSessionId ?: store.lastSessionId
        if (sid != null && lastConfig != null) core.openSession(sid)
        else if (sid != null) open(resume = sid)
        else open(resume = null)
    }

    /** Refresh sessions AND the projects that label them. Kept as one call so the two can never
     *  drift -- a session list newer than the project list renders groups labelled by raw id. */
    fun refreshSidebar() { core.listSessions(); io { unstable.sourcesList("project") } }

    /** Archive a session: history stays on disk, it just leaves the list. The soft option --
     *  deleteSession is the permanent one. */
    fun archiveSession(sessionId: String) {
        core.archiveSession(sessionId)
        sessions.value = sessions.value.filterNot { it.sessionId == sessionId }   // optimistic
        if (sessionId == store.assistantSessionId) store.assistantSessionId = null
    }

    /** Serialize a session server-side; the reply lands in [exportData] and the UI opens the
     *  Android share sheet with it. */
    fun exportSession(sessionId: String) { io { unstable.exportSession(sessionId) } }

    /** Publish this device's UnifiedPush endpoint into the server's config (GROUSE_PUSH_ENDPOINT),
     *  so the server's senders always POST to the current token. Best-effort. */
    fun publishPushEndpoint(url: String) {
        if (url.isNotBlank()) io { unstable.configUpsert("GROUSE_PUSH_ENDPOINT", url) }
    }

    /** Answer a pending elicitation form and drop it from the queue. */
    fun answerElicitation(e: AcpEvent.Elicitation, values: Map<String, JsonPrimitive>?, cancelled: Boolean = false) {
        io {
            if (e.recipeParams) {
                // Recipe-params answers ride the same UI but a different wire shape:
                // {action:"submit"|"cancel", values:{key:string}} — values must be STRINGS
                // (RecipeParamsResponse is HashMap<String,String> server-side).
                if (cancelled || values == null) unstable.respondRecipeParams("cancel", "{}")
                else unstable.respondRecipeParams(
                    "submit",
                    buildJsonObject { values.forEach { (k, v) -> put(k, v.content) } }.toString())
            } else {
                when {
                    cancelled -> unstable.respondElicitation("cancel", "")
                    values == null -> unstable.respondElicitation("decline", "")
                    else -> unstable.respondElicitation(
                        "accept",
                        buildJsonObject { values.forEach { (k, v) -> put(k, v) } }.toString())
                }
            }
        }
        elicitations.remove(e)
    }

    /** Delete a session outright (history gone server-side). Archive remains the soft option. */
    fun deleteSession(sessionId: String) {
        core.deleteSession(sessionId)
        sessions.value = sessions.value.filterNot { it.sessionId == sessionId }   // optimistic
        if (sessionId == store.assistantSessionId) store.assistantSessionId = null
    }

    /** Move a chat into a project (or back to /state): the sanctioned working_dir rewrite.
     *  Also the in-app repair for sessions stranded by a renamed project directory. */
    fun moveSession(sessionId: String, cwd: String) {
        io { unstable.workingDirUpdate(sessionId, cwd) }
        sessions.value = sessions.value.map {          // optimistic
            if (it.sessionId == sessionId) it.copy(cwd = cwd) else it
        }
        store.rememberSessionCwds(listOf(sessionId to cwd))
    }

    /** Set a session's title (the reply re-lists). */
    fun renameSession(sessionId: String, title: String) {
        val t = title.trim()
        if (t.isEmpty()) return
        core.renameSession(sessionId, t)
        sessions.value = sessions.value.map {          // optimistic
            if (it.sessionId == sessionId) it.copy(title = t) else it
        }
    }

    fun openSession(sessionId: String, knownKind: SessionKind? = null) {
        // Cancel any deferred "open the assistant thread" -- the user has since picked a specific
        // session and that choice wins.
        pendingOpenAssistant = false
        pendingAssistantRename = false
        messages.clear(); lastSessionId = sessionId; currentSession.value = sessionId
        // Roam: the session lives on the peer -- the core routes the chat to it.
        val peer = roamPeer(sessionId)
        if (peer != null) {
            currentRoamPeer = peer
            core.roamOpenSession(peer, sessionId)
            return
        }
        currentRoamPeer = null
        open(resume = sessionId, kind = knownKind)
    }

    fun newSession(
        cwd: String = "",
        kind: SessionKind = SessionKind.CHAT,
        recipeId: String? = null,
    ) {
        pendingOpenAssistant = false      // same as openSession: an explicit choice cancels it
        pendingAssistantRename = false
        messages.clear(); lastSessionId = null; currentSession.value = null; config.value = emptyList()
        // A new chat always leaves any peer-owned session (the core clears its own routing too).
        currentRoamPeer = null
        pendingRecipeId = recipeId
        open(resume = null, kind = kind)
    }

    /** Carried to the next session/new. Consumed by open() once handed to the core, so a plain
     *  chat started afterwards does not inherit the recipe. */
    @Volatile private var pendingRecipeId: String? = null

    /** Run a recipe: start a session from it, optionally in [cwd].
     *
     *  This is what "running" a recipe means interactively -- goose applies the recipe's
     *  extensions, settings and instructions to a real session you then talk to, rather than
     *  executing it once and discarding it. The scheduled jobs use the same recipes through the
     *  scheduler; this is the same object driven by hand. */
    fun runRecipe(recipeId: String, cwd: String? = null) =
        newSession(cwd = cwd ?: store.workingDir, kind = SessionKind.CHAT, recipeId = recipeId)

    /** Run one shell command server-side via a DIRECT tool call and report (error, output).
     *
     *  No model is involved: the core invokes developer__shell through `tools/call` on the
     *  bound session -- deterministic, exact output, near-instant. Because the session never
     *  receives a prompt it gains no messages (the old throwaway-session variant was needed
     *  only because the hand-rolled client had no direct-call path on the live wire). */
    private fun runUtilityTool(command: String, timeoutMs: Long = 30_000, onDone: (String?, String) -> Unit) =
        runToolDirect("shell", kotlinx.serialization.json.buildJsonObject {
            put("command", kotlinx.serialization.json.JsonPrimitive(command))
        }, timeoutMs, onDone)

    /** Invoke ONE tool directly and hand back its text, with no model turn.
     *  Generalised out of runUtilityTool, which was the shell-only special case. */
    private fun runToolDirect(
        tool: String,
        args: kotlinx.serialization.json.JsonObject,
        timeoutMs: Long = 30_000,
        onDone: (String?, String) -> Unit,
    ) {
        val sid = core.activeSessionId()
        if (sid == null || !core.ready()) { onDone("not ready — no session", ""); return }
        var done = false
        lateinit var watchdog: Runnable
        lateinit var entry: PendingToolCall
        fun finish(err: String?, out: String) {
            if (done) return
            done = true
            main.removeCallbacks(watchdog)
            toolCallQueue.removeAll { it === entry }
            onDone(err, out)
        }
        watchdog = Runnable { finish("Timed out talking to the server.", "") }
        main.postDelayed(watchdog, timeoutMs)
        entry = PendingToolCall(::finish)
        toolCallQueue.addLast(entry)
        io { unstable.toolsCall(sid, tool, args.toString()) }
    }

    private data class PendingToolCall(val onDone: (String?, String) -> Unit)
    // Direct tool replies arrive on one listener slot; the core executes tools_call requests
    // sequentially per connection, so a FIFO matches replies to callers exactly.
    private val toolCallQueue = ArrayDeque<PendingToolCall>()

    private fun cleanProjectName(raw: String): String? {
        val name = raw.trim().trim('/').removePrefix("projects/")
            .removePrefix("workspace/").trim('/')
        if (name.isEmpty() || name.contains("..") || name.contains('/') ||
            name.any { it.isWhitespace() } || name.contains('\'') || name.contains('"')) return null
        return name
    }

    /** Create a project on the server.
     *
     *  Was `mkdir -p /projects/<name>` over a shell tool, because a project WAS a directory.
     *  Now it is a named source (sources/create) and nothing is created on disk, so a project
     *  can be renamed, cannot be broken by a moved mount, and means the same thing to every
     *  client.
     *
     *  Validated here rather than round-tripping: goose applies validate_skill_name -- lowercase
     *  ASCII, digits and hyphens, no leading/trailing hyphen, <=64 chars -- and returns a raw
     *  -32602 for anything else. "Cooking" is rejected, which is surprising enough to be worth
     *  saying plainly in the dialog. */
    fun createProject(rawName: String, onResult: (String?) -> Unit) {
        val name = rawName.trim()
        val bad = when {
            name.isEmpty() -> "Name can't be empty."
            name.length > 64 -> "Name must be 64 characters or fewer."
            name.startsWith("-") || name.endsWith("-") -> "Name can't start or end with a hyphen."
            !name.all { it in 'a'..'z' || it in '0'..'9' || it == '-' } ->
                "Lowercase letters, digits and hyphens only — goose rejects capitals."
            projects.value.any { it.name == name } -> "A project called \"$name\" already exists."
            else -> null
        }
        if (bad != null) { onResult(bad); return }
        io { unstable.sourcesCreate("project", name, "", "") }
        // The reply dispatch re-lists projects; report success now so the dialog can close.
        onResult(null)
    }

    /** Read a project's .goosehints and local memory (goose's memory extension stores its
     *  local scope at <cwd>/.goose/memory). Direct shell call -- exact file contents. */
    fun fetchProjectInfo(project: String, onResult: (String?, String) -> Unit) {
        val name = cleanProjectName(project) ?: run { onResult("bad project name", ""); return }
        runUtilityTool(
            "echo '=== .goosehints ==='; cat '/workspace/$name/.goosehints' 2>/dev/null || echo '(none)'; " +
                "echo; echo '=== .goose/memory ==='; " +
                "for f in '/workspace/$name/.goose/memory'/*; do [ -f \"\$f\" ] || continue; " +
                "echo \"-- \$(basename \"\$f\")\"; cat \"\$f\"; done 2>/dev/null; " +
                "[ -d '/workspace/$name/.goose/memory' ] || echo '(none)'"
        ) { err, text -> onResult(err, text) }
    }

    /** Delete a project and unfile its chats.
     *
     *  Chats are UNFILED rather than archived: a project is only a label now, so deleting it
     *  should not hide conversations. Removing the label leaves them in Unfiled, where they can
     *  be found and re-filed. Archiving them would make deleting a project destructive in a way
     *  its name does not suggest. */
    fun deleteProject(project: String, onResult: (String) -> Unit) {
        val proj = projects.value.firstOrNull { it.id == project || it.name == project }
            ?: run { onResult("No such project."); return }
        val affected = sessions.value.filter { it.projectId == proj.id }
        affected.forEach { fileSession(it.sessionId, null) }
        io { unstable.sourcesDelete("project", proj.path) }
        projects.value = projects.value.filterNot { it.id == proj.id }   // optimistic; reply re-lists
        onResult(
            if (affected.isEmpty()) "Project deleted."
            else "Project deleted. ${affected.size} chat${if (affected.size == 1) "" else "s"} moved to Unfiled."
        )
    }

    /** The assistant thread's id.
     *
     *  The CACHED id wins over the title lookup, not the other way round (see the note in the
     *  Sessions handler). The cached id is the one this app actually created and renamed, so it
     *  is the authoritative answer; the title lookup is the fallback for a fresh install. */
    fun assistantSessionId(): String? =
        store.assistantSessionId
            ?: sessions.value.firstOrNull { it.title == ASSISTANT_TITLE }?.sessionId

    /** True when the on-screen conversation IS the privileged assistant thread. */
    val onAssistant: Boolean get() = currentSession.value != null && currentSession.value == assistantSessionId()

    @Volatile private var pendingOpenAssistant = false

    /** Open the privileged assistant thread; if the session list isn't loaded yet, refresh it and
     *  open as soon as it arrives (see the Sessions handler). */
    fun openAssistant() {
        val id = assistantSessionId()
        if (id != null) { openSession(id, knownKind = SessionKind.ASSISTANT); return }
        // No id yet: defer until a session list arrives -- but only if one can actually arrive.
        // With no live connection, refreshSidebar() does nothing and the flag would sit armed
        // indefinitely.
        if (live) { pendingOpenAssistant = true; refreshSidebar() }
        else beginAssistantThread()
    }

    // --- Assistant-thread reset / (re)create ---
    // The reset is IN-PLACE: /clear keeps the thread's session id (deliver.sh compacts the
    // same way), so the id never changes and nothing needs re-pointing. The rename-to-Assistant
    // path only fires when a NEW thread was created (no thread existed); an explicit
    // newSession/openSession after beginAssistantThread supersedes the pending rename, so a
    // stray Ready can never title the wrong chat.
    @Volatile private var pendingAssistantRename = false

    /** Clear the assistant thread IN PLACE, keeping its session id.
     *
     *  `/clear` is a goose slash command: execute_command intercepts the literal text before any
     *  model turn and replace_conversation swaps in an empty conversation. History is discarded
     *  (unlike /compact, which summarises it), the id survives, and no cache anywhere needs
     *  updating. If the thread does not exist yet there is nothing to clear -- create it. */
    fun resetAssistant() {
        val id = store.assistantSessionId ?: assistantSessionId()
        if (id == null) { beginAssistantThread(); return }
        openSession(id, knownKind = SessionKind.ASSISTANT)
        pendingClearOnReady = id
    }

    /** Set by resetAssistant; consumed in the Ready handler once the session is live, because
     *  /clear has to be sent as a prompt ON that session. */
    private var pendingClearOnReady: String? = null

    /** Open a fresh session that will become the ASSISTANT_TITLE thread once live. */
    private fun beginAssistantThread() {
        pendingAssistantRename = true
        newSession(kind = SessionKind.ASSISTANT)
    }

    // --- Server-side goose config (global config.yaml, edited over ACP) ---
    /** Current server values, populated by loadServerConfig(); empty until read. */
    val serverContextLimit = mutableStateOf("")
    val serverFastModel = mutableStateOf("")
    /** Generic mirror of config/read replies, keyed by config key — the phone-writable
     *  server settings channel (schedule times, etc.). */
    val serverConfig = mutableStateMapOf<String, String>()

    /** Observable master switch (mirrors SecureStore.assistantEnabled for the drawer). */
    val assistantEnabled = mutableStateOf(store.assistantEnabled)
    fun setAssistantEnabled(on: Boolean) {
        store.assistantEnabled = on
        assistantEnabled.value = on
        writeServerConfig("ASSISTANT_ENABLED", if (on) "true" else "false")
    }

    /** Trigger a server-side test run of a delivery pipeline ("morning" | "briefing"):
     *  a direct tool call touches a /state drop file that a host systemd path unit watches;
     *  the unit runs deliver.sh with TEST_RUN=1 (all gates and stamps bypassed, briefings
     *  forced to produce a visible push). */
    fun testAssistantJob(kind: String, onResult: (String?) -> Unit) {
        runUtilityTool("touch /state/.assistant-test-$kind") { err, _ -> onResult(err) }
    }

    /** Read + write server-side goose config (the deliver.sh schedule keys live there). */
    fun readServerConfig(vararg keys: String) { io { keys.forEach { unstable.configRead(it) } } }
    fun writeServerConfig(key: String, value: String) {
        io { unstable.configUpsert(key, value) }
        serverConfig[key] = value   // optimistic
    }

    /** Read the app-editable global goose settings so Settings can show current values. */
    fun loadServerConfig() {
        io {
            unstable.configRead("GOOSE_CONTEXT_LIMIT")
            unstable.configRead("GOOSE_FAST_MODEL")
        }
    }

    /** Upsert a global goose setting (takes effect for NEW sessions/tasks), then re-read to confirm. */
    fun setServerConfig(key: String, value: String) {
        io {
            unstable.configUpsert(key, value)
            unstable.configRead(key)
        }
    }

    fun setOption(configId: String, value: String) {
        store.saveOption(configId, value)
        core.setConfigOption(configId, value)
    }

    fun send(text: String, images: List<ImageBlock> = emptyList()) {
        // Keep the images ON the message so the bubble renders the actual thumbnail(s), not a
        // "[📎 N]" placeholder. (Live-session only — a session reloaded from the server replays
        // text; images aren't reconstructed from the replayed content blocks.)
        messages.add(ChatMessage("user", text, images)); busy.value = true
        lastMessageUsage.value = null   // stale stats from the previous turn shouldn't linger
        startService()   // keep the socket alive if the user backgrounds mid-turn
        if (images.isNotEmpty() && store.describeImages) { describeThenSend(text, images); return }
        dispatch(text, images)
    }

    /** Turn the attached images into text, then send a text-only prompt.
     *
     *  WHY, given goose has a `read_image` tool: read_image is not a proxy. It loads the file and
     *  returns image content, so the image still lands in the token stream -- the same bytes, one
     *  layer over, still unreadable to a model that cannot see. Nothing in goose transcodes an
     *  image to text, so the conversion has to happen before the prompt is built.
     *
     *  The user's typed text is passed as the question, so the answer is about what they actually
     *  asked rather than a generic caption -- the difference between "a screenshot of a terminal"
     *  and the error message they wanted read.
     *
     *  The bubble is already on screen with its thumbnail; only what goes to the model changes.
     *  On failure the send still happens, with the failure written into the prompt: a silent
     *  fallback to sending the raw image would put us back where we started, and dropping the
     *  message would lose what the user typed. */
    private fun describeThenSend(text: String, images: List<ImageBlock>) {
        status.value = if (images.size == 1) "reading the image…" else "reading ${images.size} images…"
        val parts = arrayOfNulls<String>(images.size)
        var remaining = images.size
        images.forEachIndexed { i, img ->
            runToolDirect("vision__describe_image", kotlinx.serialization.json.buildJsonObject {
                put("image", kotlinx.serialization.json.JsonPrimitive("data:${img.mimeType};base64,${img.dataB64}"))
                put("question", kotlinx.serialization.json.JsonPrimitive(
                    text.ifBlank { "Describe this image in detail." }))
            }, timeoutMs = 120_000) { err, out ->
                parts[i] = err?.let { "(could not read this image: $it)" } ?: out
                if (--remaining == 0) {
                    val described = images.indices.joinToString("\n\n") { n ->
                        val label = if (images.size == 1) "Image" else "Image ${n + 1}"
                        "[$label, described by a vision model because I can't see images: " +
                            "${parts[n].orEmpty().trim()}]"
                    }
                    status.value = ""
                    dispatch(if (text.isBlank()) described else "$text\n\n$described", emptyList())
                }
            }
        }
    }

    private fun dispatch(text: String, images: List<ImageBlock>) {
        // `ready` gate: between reconnect and replay-complete the socket is live but has no
        // bound session — a send in that window used to ERROR ("not ready — no session")
        // instead of queueing, which was the visible "app must reconnect" failure. Queue;
        // the Ready handler flushes.
        if (live && core.ready() && !turnInFlight) {
            turnInFlight = true
            lastSessionId?.let { store.pendingPushSessionId = it }
            sendPromptBlocks(text, images, expect = currentSession.value)
        } else if (live) {
            // A turn is already running. Steer into it when the core surfaced
            // the run id (the server validates expected_run_id, so a run that
            // ended between typing and sending fails loudly instead of
            // starting a stray turn); otherwise queue for RunEnded — a second
            // sendPrompt would interleave in the transcript and the reply
            // would be attributed to the wrong question.
            val run = activeRunId
            if (run != null) {
                turnInFlight = true
                io { unstable.steer(text, run) }
            } else {
                enqueue(PendingSend(text, images))
            }
        } else {
            // Not connected yet (initial connect / silent reconnect window): queue and let the
            // core's own reconnect deliver the flush. The resume's replay wipes the local
            // transcript (including this just-added bubble); Ready re-adds the queued bubbles on
            // top of the rebuilt history.
            enqueue(PendingSend(text, images))
            if (!connecting) reconnectToCurrent()
        }
    }

    /** Build a Prompt and hand it to the core. The core rejects a send whose `expect` session
     *  mismatches its bound session — silently, so the mismatch is surfaced here instead. */
    private fun sendPromptBlocks(text: String, images: List<ImageBlock>, expect: String?) {
        val bound = core.activeSessionId()
        if (expect != null && expect != bound) {
            messages.add(ChatMessage("error",
                "not sent — this chat isn't loaded yet (showing $expect, socket on $bound). Try again."))
            busy.value = false; turnInFlight = false
            return
        }
        val blocks = mutableListOf<PromptBlock>()
        if (text.isNotBlank()) blocks.add(PromptBlock.Text(text))
        images.forEach { img -> blocks.add(PromptBlock.Image(img.mimeType, img.dataB64)) }
        if (blocks.isEmpty()) { busy.value = false; turnInFlight = false; return }
        core.sendPrompt(Prompt(blocks), expect?.let { SendExpect(it) })
    }

    /** Send once connected — used by notification replies, which may arrive disconnected. */
    fun sendWhenReady(text: String) = send(text)

    fun setForeground(fg: Boolean) {
        appForeground = fg
        if (fg) {
            notifier.cancelAlert()
            // The core owns reconnect + remote-change resync, so nothing to nudge beyond a
            // live re-ask of the server's model list (models renamed/added server-side stay
            // invisible until then — cheap, one /v1/models round trip through goose).
            if (live) {
                config.value.firstOrNull { it.id == "provider" }?.currentValue
                    ?.takeIf { it.isNotBlank() }
                    ?.let { model -> io { unstable.supportedModels(model) } }
            }
            if (!store.persistentConnection && !busy.value) stopService()
        } else {
            if (store.persistentConnection) startService()
        }
    }

    fun setPersistent(on: Boolean) {
        store.persistentConnection = on
        if (on) startService() else if (!busy.value && appForeground) stopService()
    }

    val persistent: Boolean get() = store.persistentConnection

    /** Whether the app is currently in the foreground (for push dedup). */
    val isForeground: Boolean get() = appForeground

    private fun startService() {
        if (serviceRunning) return
        serviceRunning = true
        appContext.startForegroundService(Intent(appContext, ConnectionService::class.java))
    }

    private fun stopService() {
        if (!serviceRunning) return
        serviceRunning = false
        appContext.stopService(Intent(appContext, ConnectionService::class.java))
    }

    private fun lastAssistantText(): String =
        messages.lastOrNull { it.role == "assistant" }?.text ?: "Turn finished."

    /** Stop the running turn. The core sends the ACP cancel; the server ends the turn and the
     *  queue drains on RunEnded. Stop means stop: queued prompts are dropped, not deferred. */
    fun cancel() {
        core.cancel()
        busy.value = false
        turnInFlight = false   // wire is free again; without this the queue never drains
        clearQueue()           // Stop means stop: don't let queued prompts fire after a cancel
    }

    /** Compact the conversation history to reclaim context (goose /compact command). */
    fun compact() {
        busy.value = true
        compacting.value = true   // client-initiated: don't wait for the server's own status echo
        sendPromptBlocks("/compact", emptyList(), expect = currentSession.value)
    }

    /** Answer the given approval request; null optionId denies (cancelled). */
    fun answerPermission(p: AcpEvent.Permission, optionId: String?) {
        core.respondPermission(p.toolCallId,
            if (optionId != null) PermissionOutcome.Selected(optionId) else PermissionOutcome.Cancelled)
        permissions.remove(p)
    }

    // ---------------------------------------------------------------------------
    // Core event translation (all handlers run on the main thread)
    // ---------------------------------------------------------------------------

    private fun onCoreStatus(st: ConnectionStatus) {
        when (st) {
            ConnectionStatus.Ready -> onCoreReady()
            ConnectionStatus.Connecting -> {
                connecting = true
                status.value = "connecting…"
            }
            ConnectionStatus.Syncing -> status.value = "loading session…"
            ConnectionStatus.Disconnected -> {
                live = false; connecting = false; online.value = false
                replayActive.value = false
                status.value = "not connected"
            }
            is ConnectionStatus.Error -> {
                live = false; connecting = false; online.value = false
                replayActive.value = false
                status.value = st.message
            }
        }
    }

    private fun onCoreReady() {
        live = true; connecting = false; online.value = true
        val sid = core.activeSessionId()
        if (sid == null) return
        // A first-process connect() (or a config change) created a transient session first;
        // the real target opens now. Skip the transient's bookkeeping entirely.
        val deferred = pendingResumeAfterConnect
        if (deferred != null) {
            pendingResumeAfterConnect = null
            if (pendingRecipeId != null) core.newSession(pendingRecipeId.also { pendingRecipeId = null })
            else core.openSession(deferred)
            return
        }
        // A roam peer owns the chat: Ready here is the MAIN connection's — don't repoint the
        // on-screen session at it.
        if (currentRoamPeer == null) {
            lastSessionId = sid
            store.lastSessionId = sid
            currentSession.value = sid
            // A chat started from inside a project gets filed the moment it has an id --
            // session/new takes no projectId, so membership is a second call.
            pendingProjectFiling?.let { pid ->
                pendingProjectFiling = null
                fileSession(sid, pid)
            }
            pendingClearOnReady?.let { target ->
                pendingClearOnReady = null
                if (target == sid) {
                    messages.clear()          // the replay we just took is about to be void
                    core.sendPrompt(Prompt(listOf(PromptBlock.Text("/clear"))), SendExpect(sid))
                }
            }
            if (pendingAssistantRename) {
                pendingAssistantRename = false
                core.renameSession(sid, ASSISTANT_TITLE)
                store.assistantSessionId = sid
            }
            // Populate the in-chat "N tools" indicator and the per-extension tool lists.
            // MCP-backed extensions come up asynchronously AFTER the session is ready: a
            // tools/list fired here returns only the builtins (measured). Ask again shortly
            // for the full picture rather than caching a half-built one. (Peer-owned sessions
            // are skipped: the unstable surface routes to the main connection only.)
            detachedPeerExts.value = emptyList()
            io { unstable.listTools(sid) }
            main.postDelayed({ if (live && core.activeSessionId() == sid) io { unstable.listTools(sid) } }, 2500)
            io { unstable.sessionExtensionsList(sid) }
            core.listSessions()   // so the Assistant thread can be resolved by title
        }
        // A rebuild (replay or cache path) just finished: unpin the list and snap to bottom
        // when content actually streamed in.
        if (replayActive.value) {
            replayActive.value = false
            replayDoneTick.value++   // ChatScreen: new content -> snap to bottom
        }
        // Prompts still waiting in the queue aren't in the server history (they haven't
        // been sent). Re-add their bubbles on top, in queue order, so the user's unsent
        // messages don't vanish from the screen (the rebuild wiped them).
        if (replayWiped) {
            replayWiped = false
            pendingSends.forEach { messages.add(ChatMessage("user", it.text, it.images)) }
        }
        // Send ONE queued prompt (bubbles were already added when queued); RunEnded drains
        // the rest. Firing the whole deque at once would interleave the prompts in the
        // transcript and misattribute each reply — the exact failure the queue exists to prevent.
        dequeue()?.let { p ->
            store.pendingPushSessionId = sid
            turnInFlight = true
            busy.value = true
            sendPromptBlocks(p.text, p.images, expect = currentSession.value)
        }
    }

    private fun onCoreSessions(list: List<SessionSummary>) {
        sessions.value = list.map { it.toInfo() }
        // Only seed the cache when empty. It used to be written on every session list,
        // which let an OLD session sharing the title clobber the id of the thread this app
        // had just created. Newest title match wins, in case stale duplicates linger.
        if (store.assistantSessionId == null)
            sessions.value.filter { it.title == ASSISTANT_TITLE }
                .maxByOrNull { it.updatedAt }?.let { store.assistantSessionId = it.sessionId }
        if (pendingOpenAssistant) {
            pendingOpenAssistant = false
            val id = assistantSessionId()
            // If no assistant thread exists (e.g. an interrupted reset, or a fresh box),
            // recreate one instead of silently no-oping so openAssistant() can never dead-end.
            if (id != null) openSession(id, knownKind = SessionKind.ASSISTANT) else beginAssistantThread()
        }
    }

    /** Mirror the core's transcript into `messages`. Clear rebuilds from the core's snapshot
     *  (a fresh cached transcript arrives as Clear + nothing else; a live replay arrives as
     *  Clear + chunks). The app's bubbles are per-message ids allocated here; the core's
     *  message ids correlate Updates to the right bubble. */
    private fun onCoreTranscript(event: TranscriptEvent) {
        when (event) {
            is TranscriptEvent.Clear -> {
                messages.clear()
                coreKeyToAppId.clear()
                pendingToolStash.clear()
                if (pendingSends.isNotEmpty()) replayWiped = true
                val snap = core.transcript()
                snap.forEach { appendFromMessage(it) }
                // Empty snapshot = a rebuild is about to stream (show "Loading… N"); a
                // non-empty one is the instant cache paint (no loading state).
                replayActive.value = snap.isEmpty()
                replayProgress.value = 0
            }
            is TranscriptEvent.Append -> appendFromMessage(event.message)
            is TranscriptEvent.Update -> updateFromMessage(event.message)
        }
    }

    /** Core message id ("" for live bubbles, tool_call_id for tool rows) -> app bubble id. */
    private val coreKeyToAppId = HashMap<String, Long>()
    /** The on_stream ToolCall for a tool bubble arrives just before its on_transcript Append;
     *  keyed by tool_call_id (== the Append's message id). */
    private data class ToolCallStash(val kind: ToolCallKind?, val detail: String)
    private val pendingToolStash = HashMap<String, ToolCallStash>()

    private fun appendFromMessage(m: Message) {
        val appId = chatMessageSeq.getAndIncrement()
        val bubble = if (m.role == "tool") {
            val stash = pendingToolStash.remove(m.id)
            buildToolBubble(m, stash, appId)
        } else {
            ChatMessage(if (m.role == "agent") "assistant" else m.role, m.content, id = appId)
        }
        messages.add(bubble)
        if (m.id.isNotEmpty()) coreKeyToAppId[m.id] = appId
        if (replayActive.value) replayProgress.value++
    }

    private fun buildToolBubble(m: Message, stash: ToolCallStash?, appId: Long): ChatMessage {
        val kind = stash?.kind
        return when (kind) {
            is ToolCallKind.Chart -> ChatMessage("chart", kind.spec, id = appId)
            is ToolCallKind.McpApp -> ChatMessage(
                "mcpapp", m.content, detail = kind.input, toolCallId = m.id,
                appKey = kind.appKey, appHtml = appHtmlCache[kind.appKey] ?: "",
                id = appId,
            ).also {
                // Fetch the template once per key; the bubble renders as a plain tool row
                // until it lands. Peer-owned sessions can't fetch (unstable routes to the
                // main connection), so they stay plain rows.
                if (kind.appKey.isNotEmpty() && !appHtmlCache.containsKey(kind.appKey) &&
                    appFetchInFlight.add(kind.appKey) && roamPeer(currentSession.value) == null
                ) {
                    core.activeSessionId()?.let { sid -> io { unstable.resourcesRead(sid, kind.uri, kind.extension) } }
                }
            }
            else -> ChatMessage(
                "tool", m.content,
                detail = stash?.detail.orEmpty(),
                toolCallId = m.id,
                // Live calls stream with a lifecycle; a snapshot/rebuild (no stash) is
                // finished history and renders as a plain wrench.
                status = if (stash != null) "in_progress" else "",
                id = appId,
            )
        }
    }

    private fun updateFromMessage(m: Message) {
        val idx = if (m.id.isNotEmpty()) {
            coreKeyToAppId[m.id]?.let { appId -> messages.indexOfFirst { it.id == appId } } ?: -1
        } else {
            // Live text bubble (empty key): the core's stream bubble is always the last one.
            messages.lastIndex
        }
        if (idx < 0 || idx >= messages.size) return
        val cur = messages[idx]
        messages[idx] = cur.copy(
            role = if (m.role == "agent") "assistant" else m.role,
            text = m.content,
        )
    }

    private fun onCoreStream(event: StreamEvent) {
        when (event) {
            is StreamEvent.ToolCall -> pendingToolStash[event.toolCallId] =
                ToolCallStash(event.kind, event.detail)
            is StreamEvent.ToolCallUpdate -> {
                val i = messages.indexOfLast { it.toolCallId == event.id }
                if (i >= 0) messages[i] = messages[i].copy(
                    status = event.status.ifBlank { messages[i].status },
                    // Live shell chunks APPEND (capped — a verbose build log must not grow
                    // a transcript entry without bound); the final completion update still
                    // replaces, so the finished chip shows the tool's real result.
                    output = when {
                        event.live -> (messages[i].output + event.output).takeLast(4000)
                        event.output.isNotBlank() -> event.output
                        else -> messages[i].output
                    },
                )
            }
            is StreamEvent.Usage -> usage.value =
                AcpEvent.Usage(event.used.toInt(), event.size.toInt(), event.cost, event.currency)
            is StreamEvent.RunEnded -> onRunEnded(event.stopReason)
            // Text chunks are mirrored through on_transcript (Append/Update); nothing to do.
            is StreamEvent.AgentChunk, is StreamEvent.UserChunk, is StreamEvent.ThoughtChunk -> {}
        }
    }

    private fun onRunEnded(stopReason: String) {
        compacting.value = false
        turnInFlight = false
        // We got the authoritative completion straight from our own socket -- stop waiting
        // on the Stop-hook push for this turn so a later turn from another client in the
        // same (possibly shared) session doesn't spuriously match the stale flag.
        store.pendingPushSessionId = null
        // Drain one queued prompt, if any: send it and STAY busy, so the UI never flickers
        // idle between a queue and its turn.
        val queued = dequeue()
        busy.value = queued != null
        if (!appForeground) notifier.postReply(lastAssistantText())
        if (queued != null) {
            // Send the queued prompt now that the wire is free. Service stays up (we are
            // still busy), so backgrounding between the two turns is safe.
            turnInFlight = true
            lastSessionId?.let { store.pendingPushSessionId = it }
            sendPromptBlocks(queued.text, queued.images, expect = currentSession.value)
        } else if (!store.persistentConnection) stopService()
    }

    private fun onCoreConfig(options: List<CoreConfigOption>) {
        if (options.isEmpty()) return
        config.value = options.map { it.toApp() }
        // A federated session's options are the PEER's (session/load passes them
        // through). Persisting them would overwrite the sticky local defaults with
        // the peer's model/provider, and fetching "supported models" would ask
        // the LOCAL provider registry about the peer's provider id. Display only.
        if (roamPeer(currentSession.value) != null) return
        // Persist the true current values so re-apply on reconnect can't drift.
        config.value.forEach { if (it.currentValue.isNotBlank()) store.saveOption(it.id, it.currentValue) }
        // Re-apply persisted picks once per connection (provider first for the model cascade).
        if (!desiredApplied) {
            desiredApplied = true
            val saved = store.savedOptions(optionIds)
            if (saved.isNotEmpty()) {
                val current = config.value.associate { it.id to it.currentValue }
                for (id in listOf("provider", "model", "mode", "thinking_effort")) {
                    val want = saved[id] ?: continue
                    if (want.isNotBlank() && want != current[id]) core.setConfigOption(id, want)
                }
            }
        }
        // Models come from the SERVER, live, and are never persisted. Ask the server,
        // show the answer, keep nothing.
        val provider = config.value.firstOrNull { it.id == "provider" }?.currentValue ?: ""
        if (provider != liveModelsFetchedFor) {
            // Provider changed (or first Config): drop the previous provider's list
            // immediately so its slugs cannot be shown under the new one, even briefly.
            knownModels.value = emptySet()
            liveModelsFetchedFor = provider
            if (provider.isNotBlank()) io { unstable.supportedModels(provider) }
        }
    }

    private fun onCorePermission(request: PermissionRequest) {
        permissions.add(AcpEvent.Permission(
            request.toolCallId, request.title, request.detail,
            request.options.map { PermOption(it.optionId, it.name, it.kind) }))
        if (!appForeground) notifier.postApprovalNeeded(request.title)
    }

    private fun onCoreSessionTouched(sessionId: String, title: String, updatedAt: String) {
        // Live title/updatedAt sync (auto-naming after the first turn, renames from any
        // client) — previously only visible after a full session re-list.
        sessions.value = sessions.value.map {
            if (it.sessionId == sessionId) it.copy(
                title = if (title.isNotBlank()) title else it.title,
                updatedAt = if (updatedAt.isNotBlank()) updatedAt else it.updatedAt,
            ) else it
        }
    }

    /** The stable on_projects path is never emitted by the core (projects are an unstable
     *  sources/list feature); kept for completeness. */
    private fun onCoreProjects(projects: List<ProjectSummary>) {
        this.projects.value = projects.map { it.toInfo() }
    }

    private fun onRoamPeerStatus(label: String, st: String) {
        when {
            st == "ready" -> {
                // Peer connected; its sessions arrive via on_roam_sessions.
            }
            st == "disconnected" -> {
                if (currentRoamPeer == label) {
                    currentRoamPeer = null
                    if (roamPeer(currentSession.value) == label) {
                        messages.clear(); currentSession.value = null
                    }
                }
                sessions.value = sessions.value.filterNot { roamPeer(it.sessionId) == label }
                if (currentRoamPeer == null) status.value = "not connected"
            }
            st.startsWith("error") -> {
                if (currentRoamPeer == label) currentRoamPeer = null
                status.value = "roam: $label — ${st.removePrefix("error:")}"
            }
            else -> {}
        }
    }

    private fun onRoamSessions(label: String, sessions: List<SessionSummary>) {
        // Merge the peer's sessions into the drawer (their ids are `roam:<label>:<id>`
        // prefixed, which the drawer partitions on).
        val infos = sessions.map { it.toInfo() }
        this.sessions.value =
            this.sessions.value.filterNot { roamPeer(it.sessionId) == label } + infos
    }

    /** The running turn's run id (steer key), or null when no turn is live. */
    @Volatile private var activeRunId: String? = null

    private fun onCoreActiveRun(sessionId: String, runId: String) {
        // Steer only targets the session on screen; a peer's or background
        // session's run is irrelevant here. Empty runId = the run ended.
        if (sessionId == currentSession.value) {
            activeRunId = runId.ifEmpty { null }
        }
    }

    // ---------------------------------------------------------------------------
    // Unstable event translation
    // ---------------------------------------------------------------------------

    private fun onUnstableError(method: String, message: String) {
        // A failed sidebar/config refresh is not the conversation's problem: no
        // transcript bubble. But the flags those calls set MUST clear, or the failure
        // sticks: a dead tools/list left `discovering` armed and the NEXT tools reply
        // triggered a spurious allowlist write; a dead extensions/list left the sheet
        // spinning.
        if (method.startsWith("_goose/unstable/tools/list")) discovering = null
        if (method.startsWith("_goose/unstable/config/extensions/list")) extensionsBusy.value = false
        // Automatic probes (fired at connect / on screen open) are background traffic —
        // the old client rendered their failures as subtle background errors. Snackbar
        // only user-initiated calls; a dead probe (e.g. supported-models against a
        // provider the box can't reach) must not greet the user with an alarm at login.
        val quiet = listOf(
            "_goose/unstable/tools/list",
            "_goose/unstable/session/extensions/list",
            "_goose/unstable/providers/supported-models/list",
            "_goose/unstable/sources/list",
            "_goose/unstable/session/info",
            "_goose/unstable/session/update",
        ).any { method.startsWith(it) }
        if (quiet) { android.util.Log.w("Grouse", "quiet probe error: $method: $message"); return }
        backgroundNotice.value = "$method: $message"
        android.util.Log.w("Grouse", "background rpc error: $method: $message")
    }

    private fun onRecipes(json: String) {
        recipes.value = parseRecipes(json)
    }

    private fun onSchedules(json: String) {
        schedules.value = parseSchedules(json)
    }

    private fun onUnstableProjects(json: String) {
        projects.value = parseProjects(json)
    }

    private fun onSkills(json: String) {
        skills.value = parseSkills(json)
    }

    private fun onTools(sessionId: String, toolsJson: String) {
        val g = group(parseToolNames(toolsJson))
        val target = discovering
        if (target != null) {
            // Catalogue read: record the full set, then restore the session's real
            // setting by round-tripping the SAME ExtInfo the discovery ran with.
            toolCatalog.value = toolCatalog.value + (catKey(target) to g[target.name].orEmpty())
            discovering = null
            val allowed = (target.raw["available_tools"] as? JsonArray)
                ?.mapNotNull { it.jsonPrimitive.contentOrNull }?.toSet().orEmpty()
            setSessionTools(target, if (allowed.isEmpty()) g[target.name].orEmpty().toSet() else allowed)
        } else if (sessionId == core.activeSessionId() || sessionId == currentSession.value) {
            sessionTools.value = g
        }
    }

    private fun onExtensions(json: String) {
        extensions.value = parseGlobalExtensions(json)
        extensionsBusy.value = false
    }

    private fun onSessionExtensions(sessionId: String, json: String) {
        // Only the session the UI is showing (stale replies from a switched session must
        // not clobber the current sheet).
        if (sessionId != currentSession.value) return
        val infos = parseSessionExtensions(json)
        sessionExtensionNames.value = infos.map { it.name }
        sessionExtensionInfos.value = infos
        // A re-listed name is attached again; its detached-row copy is stale.
        detachedPeerExts.value = detachedPeerExts.value.filterNot { it.name in sessionExtensionNames.value.toSet() }
    }

    private fun onConfigValue(key: String, value: String) {
        when (key) {
            "GOOSE_CONTEXT_LIMIT" -> serverContextLimit.value = value
            "GOOSE_FAST_MODEL" -> serverFastModel.value = value
        }
        serverConfig[key] = value   // generic mirror for settings UIs
    }

    private fun onSupportedModels(provider: String, modelsJson: String) {
        // Straight replace, in memory only. Ignore a reply for a provider we have since
        // switched away from, so a slow response can't repopulate the wrong list.
        val currentProvider = config.value.firstOrNull { it.id == "provider" }?.currentValue
        if (provider == currentProvider) knownModels.value = parseModelNames(modelsJson)
    }

    private fun onCompactionStatus(message: String) {
        // Substring match on goose's own status copy: both start and end lines are type
        // Notice, so "notice vs progress" can't gate the indicator -- the text itself is
        // the only stable signal. If a future goose upgrade rewords these, this just stops
        // firing (fails safe: no spinner, never a wrong one).
        val m = message.lowercase()
        if (m.contains("compact")) compacting.value = true
        if (m.contains("complete") || m.contains("error")) compacting.value = false
    }

    private fun onMessageUsage(outputTokens: ULong, elapsedMs: ULong, timeToFirstTokenMs: ULong, cost: Double) {
        val elapsed = elapsedMs.toLong()
        if (elapsed <= 0) return   // can't derive a tok/s rate from a zero/missing duration
        val ev = AcpEvent.MessageUsage(
            outputTokens.toInt(), elapsed, timeToFirstTokenMs.toLong(),
            cost.takeIf { it > 0 })
        lastMessageUsage.value = ev
        // Stamp the reply this belongs to. Usage arrives at the end of a turn, so the
        // most recent assistant message is the one it describes.
        val idx = messages.indexOfLast { it.role == "assistant" }
        if (idx >= 0) messages[idx] = messages[idx].copy(usage = ev)
    }

    private fun onAppResource(key: String, html: String) {
        appFetchInFlight.remove(key)
        if (html.isNotBlank()) {
            appHtmlCache[key] = html
            for (i in messages.indices)
                if (messages[i].role == "mcpapp" && messages[i].appKey == key && messages[i].appHtml.isEmpty())
                    messages[i] = messages[i].copy(appHtml = html)
        }
    }

    private val elicitSeq = java.util.concurrent.atomic.AtomicInteger(1)

    /** A tool/extension requested structured input (elicitation/create, form mode). */
    private fun onElicitation(schemaJson: String) {
        val schema = try {
            Json.parseToJsonElement(schemaJson).jsonObject
        } catch (e: Exception) { null } ?: return
        val required = (schema["required"] as? JsonArray).orEmpty()
            .mapNotNull { it.jsonPrimitive.contentOrNull }.toSet()
        val fields = (schema["properties"] as? JsonObject).orEmpty().entries.map { (name, raw) ->
            val o = raw as? JsonObject ?: JsonObject(emptyMap())
            // Single-select comes as either untagged `enum` values or titled `oneOf`
            // [{const, title}] options; both collapse to Choice(value, label).
            val options =
                (o["oneOf"] as? JsonArray).orEmpty().mapNotNull { el ->
                    val eo = el as? JsonObject ?: return@mapNotNull null
                    val v = eo["const"]?.jsonPrimitive?.contentOrNull ?: return@mapNotNull null
                    Choice(v, eo["title"]?.jsonPrimitive?.contentOrNull ?: v)
                }.ifEmpty {
                    (o["enum"] as? JsonArray).orEmpty().mapNotNull { el ->
                        el.jsonPrimitive.contentOrNull?.let { Choice(it, it) }
                    }
                }
            AcpEvent.ElicitField(
                name = name,
                type = o["type"]?.jsonPrimitive?.contentOrNull ?: "string",
                title = o["title"]?.jsonPrimitive?.contentOrNull ?: name,
                description = o["description"]?.jsonPrimitive?.contentOrNull ?: "",
                options = options,
                required = name in required,
            )
        }
        elicitations.add(AcpEvent.Elicitation(
            "elicit-" + elicitSeq.getAndIncrement(),
            "Input requested",
            schema["title"]?.jsonPrimitive?.contentOrNull ?: "",
            fields))
    }

    /** A recipe with declared parameters, started via _meta.recipeId. Rendered through the
     *  SAME form UI as elicitations; the answer is routed to respond_recipe_params by the
     *  `recipeParams` flag, because the response shapes differ. */
    private fun onRecipeParams(parametersJson: String) {
        val params = try {
            Json.parseToJsonElement(parametersJson) as? JsonArray
        } catch (e: Exception) { null } ?: return
        val fields = params.mapNotNull { el ->
            val o = el as? JsonObject ?: return@mapNotNull null
            val key = o["key"]?.jsonPrimitive?.contentOrNull ?: return@mapNotNull null
            val default = o["default"]?.jsonPrimitive?.contentOrNull
            val desc = o["description"]?.jsonPrimitive?.contentOrNull ?: ""
            AcpEvent.ElicitField(
                name = key,
                type = o["input_type"]?.jsonPrimitive?.contentOrNull ?: "string",
                title = key,
                // No `default` slot in the form model — surface it in the description so
                // the value is at least visible and copyable.
                description = if (default != null) "$desc (default: $default)" else desc,
                options = (o["options"] as? JsonArray).orEmpty().mapNotNull {
                    it.jsonPrimitive.contentOrNull?.let { v -> Choice(v, v) }
                },
                required = o["requirement"]?.jsonPrimitive?.contentOrNull == "required",
            )
        }
        elicitations.add(AcpEvent.Elicitation(
            "recipeparams-" + elicitSeq.getAndIncrement(),
            "This recipe needs parameters", "Recipe parameters", fields,
            recipeParams = true))
    }

    /** Reply to a DIRECT tool invocation. The core executes tools_call requests sequentially,
     *  so the FIFO matches replies to callers in order. */
    private fun onToolResult(text: String, isError: Boolean) {
        val entry = toolCallQueue.removeFirstOrNull() ?: return
        entry.onDone(if (isError) text.ifBlank { "tool call failed" } else null, text)
    }

    // ---------------------------------------------------------------------------
    // The core connect entry point (all session opens funnel through here)
    // ---------------------------------------------------------------------------

    private fun open(resume: String?, cwd: String? = null, kind: SessionKind? = null) {
        // If a reset/create-assistant is pending but THIS open isn't the one that scheduled
        // it, abandon it so a later unrelated Ready can't complete a stale rename. (Explicit
        // newSession/openSession already clear the flag; this is the belt-and-suspenders.)
        if (pendingAssistantRename && resume != null) pendingAssistantRename = false
        // A new client can't receive the old turn's RunEnded, so clear turn state here.
        // Otherwise a hung/dropped turn leaves busy=true and every new chat + reconnect
        // inherits a stuck "goose is thinking…" with nothing sent. Same logic for compacting.
        busy.value = false; compacting.value = false
        turnInFlight = false
        live = false; connecting = true; online.value = false
        liveModelsFetchedFor = null   // re-fetch supported models fresh on every new connection
        desiredApplied = false
        replayActive.value = false; replayProgress.value = 0; replayWiped = false
        val base = currentServerConfig()
        // A fresh config (first connect of the process, or host/port/key/cwd changed) needs a
        // real `connect()` — the core's new/open intents reuse the LAST config only. connect()
        // is the one blocking core intent (bounded ≤15s), so it runs on a worker thread.
        if (lastConfig != base) {
            lastConfig = base
            pendingResumeAfterConnect = resume
            // A recipe pending on a cold start rides the connect's session/new
            // (gap 4: the core's connect() takes initial_recipe_id), so the
            // transient session IS the recipe session — one session, no waste.
            // Consumed here; the Ready handler's newSession branch then no-ops.
            val cfg = base.copy(initialRecipeId = pendingRecipeId)
            pendingRecipeId = null
            Thread({
                core.connect(cfg)
                // connect() returned: either Ready arrived (fine) or the bounded wait timed
                // out / the handshake failed (the core emits no error status for the
                // never-connected case — surface it here so the UI doesn't hang on
                // "Connecting…" forever; a LATE Ready still overrides via live).
                main.post {
                    if (!live && connecting) {
                        connecting = false
                        online.value = false
                        status.value = "connection failed — check host, port and key"
                    }
                }
            }, "grouse-core-connect").apply { isDaemon = true; start() }
            return
        }
        if (resume != null) {
            // An explicit open supersedes any deferred first-connect resume.
            pendingResumeAfterConnect = null
            core.openSession(resume)
        }
        else core.newSession(pendingRecipeId.also { pendingRecipeId = null })
    }

    private fun currentServerConfig(): ServerConfig = ServerConfig(
        host = store.host,
        // Empty port = the wss default (443). The old client let OkHttp resolve an empty
        // port to 443; goosed on goose.gauvin.id only listens on 443 (3284 is LAN-only),
        // so defaulting to 3284 here broke the "host + key, no port" setup that worked.
        port = store.port.toUShortOrNull() ?: 443u,
        secretKey = store.secretKey,
        // The app has always spoken wss to goosed (trust-all TLS); no TLS toggle exists.
        useTls = true,
        cwd = store.workingDir,
        autoConnect = true,
        clientId = "grouse",
        initialRecipeId = null,
    )
    // ---------------------------------------------------------------------------
    // Unstable payload parsers (the core hands the raw JSON reply payloads)
    // ---------------------------------------------------------------------------

    private fun parseRecipes(json: String): List<RecipeInfo> {
        val arr = try { Json.parseToJsonElement(json) as? JsonArray } catch (e: Exception) { null } ?: return emptyList()
        return arr.mapNotNull { el ->
            val e = el as? JsonObject ?: return@mapNotNull null
            val r = e["recipe"] as? JsonObject ?: return@mapNotNull null
            val settings = r["settings"] as? JsonObject
            RecipeInfo(
                id = e["id"]?.jsonPrimitive?.contentOrNull ?: return@mapNotNull null,
                title = r["title"]?.jsonPrimitive?.contentOrNull ?: "(untitled)",
                description = r["description"]?.jsonPrimitive?.contentOrNull ?: "",
                // snake_case, like recipes/schedule's cron_schedule and unlike most of goose's
                // ACP surface.
                cron = e["schedule_cron"]?.jsonPrimitive?.contentOrNull,
                // settings keys are snake_case here, like the recipe YAML they came from
                provider = settings?.get("goose_provider")?.jsonPrimitive?.contentOrNull,
                model = settings?.get("goose_model")?.jsonPrimitive?.contentOrNull,
                prompt = r["prompt"]?.jsonPrimitive?.contentOrNull,
                instructions = r["instructions"]?.jsonPrimitive?.contentOrNull,
                parameters = (r["parameters"] as? JsonArray).orEmpty().mapNotNull { p ->
                    val po = p as? JsonObject ?: return@mapNotNull null
                    RecipeParam(
                        key = po["key"]?.jsonPrimitive?.contentOrNull ?: return@mapNotNull null,
                        requirement = po["requirement"]?.jsonPrimitive?.contentOrNull ?: "required",
                        description = po["description"]?.jsonPrimitive?.contentOrNull ?: "",
                        default = po["default"]?.jsonPrimitive?.contentOrNull,
                    )
                },
                subRecipes = (r["sub_recipes"] as? JsonArray).orEmpty().mapNotNull {
                    (it as? JsonObject)?.get("name")?.jsonPrimitive?.contentOrNull
                },
                extensions = (r["extensions"] as? JsonArray).orEmpty().mapNotNull {
                    (it as? JsonObject)?.get("name")?.jsonPrimitive?.contentOrNull
                },
                filePath = e["file_path"]?.jsonPrimitive?.contentOrNull ?: "",
                raw = r,
            )
        }.sortedBy { it.title.lowercase() }
    }

    private fun parseSchedules(json: String): List<ScheduleInfo> {
        val arr = try { Json.parseToJsonElement(json) as? JsonArray } catch (e: Exception) { null } ?: return emptyList()
        return arr.mapNotNull { el ->
            val o = el as? JsonObject ?: return@mapNotNull null
            ScheduleInfo(
                id = o["id"]?.jsonPrimitive?.contentOrNull ?: return@mapNotNull null,
                cron = o["cron"]?.jsonPrimitive?.contentOrNull ?: "",
                source = o["source"]?.jsonPrimitive?.contentOrNull ?: "",
                paused = o["paused"]?.jsonPrimitive?.booleanOrNull ?: false,
                running = o["currentlyRunning"]?.jsonPrimitive?.booleanOrNull ?: false,
                lastRun = o["lastRun"]?.jsonPrimitive?.contentOrNull,
                currentSessionId = o["currentSessionId"]?.jsonPrimitive?.contentOrNull,
            )
        }.sortedBy { it.id.lowercase() }
    }

    private fun parseProjects(json: String): List<ProjectInfo> {
        val arr = try { Json.parseToJsonElement(json) as? JsonArray } catch (e: Exception) { null } ?: return emptyList()
        return arr.mapNotNull { el ->
            val o = el as? JsonObject ?: return@mapNotNull null
            val path = o["path"]?.jsonPrimitive?.contentOrNull ?: return@mapNotNull null
            val name = o["name"]?.jsonPrimitive?.contentOrNull ?: return@mapNotNull null
            val slug = path.substringAfterLast('/').removeSuffix(".md")
            val content = o["content"]?.jsonPrimitive?.contentOrNull ?: ""
            ProjectInfo(
                id = slug.ifEmpty { name },
                name = name,
                description = o["description"]?.jsonPrimitive?.contentOrNull ?: "",
                path = path,
                root = content.lineSequence()
                    .firstOrNull { it.trim().startsWith("root:") }
                    ?.substringAfter("root:")?.trim().orEmpty(),
            )
        }.sortedBy { it.name.lowercase() }
    }

    private fun parseSkills(json: String): List<SkillInfo> {
        val arr = try { Json.parseToJsonElement(json) as? JsonArray } catch (e: Exception) { null } ?: return emptyList()
        return arr.mapNotNull { el ->
            val o = el as? JsonObject ?: return@mapNotNull null
            SkillInfo(
                name = o["name"]?.jsonPrimitive?.contentOrNull ?: return@mapNotNull null,
                description = o["description"]?.jsonPrimitive?.contentOrNull ?: "",
                content = o["content"]?.jsonPrimitive?.contentOrNull ?: "",
                path = o["path"]?.jsonPrimitive?.contentOrNull ?: "",
                global = o["global"]?.jsonPrimitive?.booleanOrNull ?: false,
                writable = o["writable"]?.jsonPrimitive?.booleanOrNull ?: false,
            )
        }.sortedBy { it.name.lowercase() }
    }

    private fun parseGlobalExtensions(json: String): List<ExtInfo> {
        val arr = try { Json.parseToJsonElement(json) as? JsonArray } catch (e: Exception) { null } ?: return emptyList()
        return arr.mapNotNull { el ->
            val o = el as? JsonObject ?: return@mapNotNull null
            val ext = o["extension"] as? JsonObject ?: return@mapNotNull null
            // type=mcp extensions (nextcloud, kagi, fastmail, Fetch -- anything backed by an
            // actual MCP server) carry their name NESTED at extension.server.name, not the
            // top-level extension.name that builtin/platform types use.
            val name = ext["name"]?.jsonPrimitive?.contentOrNull
                ?: (ext["server"] as? JsonObject)?.get("name")?.jsonPrimitive?.contentOrNull
                ?: return@mapNotNull null
            ExtInfo(
                name = name,
                enabled = o["enabled"]?.jsonPrimitive?.booleanOrNull ?: false,
                type = ext["type"]?.jsonPrimitive?.contentOrNull ?: "",
                description = ext["description"]?.jsonPrimitive?.contentOrNull ?: "",
                configKey = o["configKey"]?.jsonPrimitive?.contentOrNull ?: name,
                bundled = ext["bundled"]?.jsonPrimitive?.booleanOrNull ?: false,
                raw = ext,
            )
        }
    }

    /** Session-scoped extensions: the array elements ARE the extension objects (goose's tagged
     *  union carries `name` at the top level), unlike config/extensions/list's wrap. */
    private fun parseSessionExtensions(json: String): List<ExtInfo> {
        val arr = try { Json.parseToJsonElement(json) as? JsonArray } catch (e: Exception) { null } ?: return emptyList()
        return arr.mapNotNull { el ->
            val ext = el as? JsonObject ?: return@mapNotNull null
            val name = ext["name"]?.jsonPrimitive?.contentOrNull
                ?: (ext["server"] as? JsonObject)?.get("name")?.jsonPrimitive?.contentOrNull
                ?: return@mapNotNull null
            ExtInfo(
                name = name,
                enabled = true,   // attached to the session by definition
                type = ext["type"]?.jsonPrimitive?.contentOrNull ?: "",
                description = ext["description"]?.jsonPrimitive?.contentOrNull ?: "",
                configKey = name,
                bundled = ext["bundled"]?.jsonPrimitive?.booleanOrNull ?: false,
                raw = ext,
                fromPeer = roamPeer(currentSession.value) != null,
            )
        }
    }

    private fun parseToolNames(json: String): List<String> {
        val arr = try { Json.parseToJsonElement(json) as? JsonArray } catch (e: Exception) { null } ?: return emptyList()
        return arr.mapNotNull { (it as? JsonObject)?.get("name")?.jsonPrimitive?.contentOrNull }
    }

    private fun parseModelNames(json: String): Set<String> {
        val arr = try { Json.parseToJsonElement(json) as? JsonArray } catch (e: Exception) { null } ?: return emptySet()
        return arr.mapNotNull { it.jsonPrimitive.contentOrNull }.toSet()
    }

    // ---------------------------------------------------------------------------
    // Record translation (core -> app DTO)
    // ---------------------------------------------------------------------------

    private fun SessionSummary.toInfo(): SessionInfo = SessionInfo(
        sessionId = id,
        title = title,
        updatedAt = updatedAt,
        messageCount = messageCount.toInt(),
        model = model,
        snippet = lastMessageSnippet.orEmpty(),
        hasRecipe = hasRecipe,
        projectId = projectId,
    )

    private fun CoreConfigOption.toApp(): ConfigOption = ConfigOption(
        id = id,
        name = name,
        currentValue = value,
        choices = choices.map { Choice(it.value, it.name) },
    )

    private fun ProjectSummary.toInfo(): ProjectInfo = ProjectInfo(
        id = path.substringAfterLast('/').removeSuffix(".md").ifEmpty { name },
        name = name,
        description = description.orEmpty(),
        path = path,
        root = "",
    )

    companion object {
        /** Server-side name of the persistent assistant thread (see docker/llm/goose-recipes).
         *  Renamed from "goose-assistant" 2026-07-28 -- coordinated with sessions.db and
         *  deliver.sh's SESSION_NAME, since resolution on all sides is an exact title match. */
        const val ASSISTANT_TITLE = "Assistant"
        /** Sessions are filed by goose's own project id, never by inspecting cwd -- a path is
         *  this server's layout and means nothing on anyone else's. Only the Assistant thread
         *  is special, and it is identified by title. */
        fun sessionKind(s: SessionInfo): SessionKind =
            if (s.title == ASSISTANT_TITLE) SessionKind.ASSISTANT else SessionKind.CHAT

        /** A session living on a roam peer arrives as `roam:<peer>:<remote id>` (the core's
         *  peer namespace). Local ids never carry the prefix, so it doubles as the
         *  client-side "this session is remote" signal. Returns the peer nickname, or null
         *  for a local session. */
        fun roamPeer(sessionId: String?): String? =
            sessionId?.takeIf { it.startsWith("roam:") }
                ?.removePrefix("roam:")?.substringBefore(':')?.ifBlank { null }

        @Volatile private var instance: ConnectionManager? = null
        fun get(context: Context): ConnectionManager =
            instance ?: synchronized(this) {
                instance ?: ConnectionManager(context.applicationContext).also { instance = it }
            }
    }
}
