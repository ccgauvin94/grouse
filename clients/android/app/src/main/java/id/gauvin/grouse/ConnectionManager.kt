package id.gauvin.grouse

import android.content.Context
import android.content.Intent
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import androidx.compose.runtime.mutableStateListOf
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive
import androidx.compose.runtime.mutableStateOf
import uniffi.grouse_roam_core.cardFingerprint
import uniffi.grouse_roam_core.identityGenerate
import uniffi.grouse_roam_core.identityPublicKey
import uniffi.grouse_roam_core.roamConnect

/** The three drawer session categories. See ConnectionManager.sessionKind(). */
enum class SessionKind { ASSISTANT, CHAT, CODE }

private val chatMessageSeq = java.util.concurrent.atomic.AtomicLong(0)
/** Stable per-message id so the chat LazyColumn keys on identity, not position. copy() preserves it,
 *  so the streaming message keeps the same id as its text grows → its composition is reused, not rebuilt. */
data class ChatMessage(
    val role: String,
    val text: String,
    val images: List<ImageBlock> = emptyList(),
    // Extra detail for a "tool" message: the tool's rawInput (command/args) — Desktop shows this,
    // Grouse was discarding it and only keeping the title. Unused by other roles.
    val detail: String = "",
    // Tool-role only: goose's toolCallId (correlates tool_call_update notifications),
    // lifecycle status (in_progress/completed/failed), and the tool's OUTPUT text when the
    // completion update carried content. Live sessions only — replays don't reconstruct these.
    val toolCallId: String = "",
    val status: String = "",
    val output: String = "",
    val id: Long = chatMessageSeq.getAndIncrement(),
    // Generation stats, stamped onto the assistant message when the turn ends. Held per message
    // rather than as one "latest" value so long-pressing any reply can show its own numbers;
    // replayed history has none, because the server transcript does not carry them.
    val usage: AcpEvent.MessageUsage? = null,
    // MCP-App ("mcpapp" role) only: the server-hosted template that renders this tool's output,
    // and the cache key it was fetched under. `detail` holds the tool input JSON the template
    // consumes. appHtml is empty while the fetch is in flight — the renderer shows a plain tool
    // row until it lands, and forever if it never does.
    val appKey: String = "",
    val appHtml: String = "",
)

/**
 * Process-scoped owner of the ACP connection + chat state. A singleton (not a ViewModel) so
 * it survives navigation/config changes and can later be shared with a background service,
 * share intents, tiles, etc. Compose observes its snapshot state directly.
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
    // True between sendPrompt and TurnDone. `busy` is UI state and is also set while merely
    // queued, so it cannot answer "is the wire busy" -- this can.
    private var turnInFlight = false
    // messageId of the chunk currently streaming into the open bubble (replay only).
    private var streamMsgId: String? = null

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
        client?.listSchedules()
        client?.listRecipes()
    }

    fun setSchedulePaused(id: String, paused: Boolean) { client?.pauseSchedule(id, paused) }

    fun runScheduleNow(id: String) { client?.runScheduleNow(id) }

    fun deleteSchedule(id: String) { client?.deleteSchedule(id) }

    fun setScheduleCron(id: String, cron: String) { client?.updateScheduleCron(id, cron) }

    fun setRecipeCron(recipeId: String, cron: String?) { client?.scheduleRecipe(recipeId, cron) }

    fun deleteRecipe(recipeId: String) { client?.deleteRecipe(recipeId) }

    /** Save an edited recipe. The caller hands back a full DTO derived from RecipeInfo.raw. */
    fun saveRecipe(recipeId: String, dto: JsonObject) { client?.saveRecipe(recipeId, dto) }

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

    fun refreshSkills() { client?.listSkills() }

    fun saveSkill(s: SkillInfo, content: String) {
        client?.updateSkill(s.path, s.name, s.description, content)
    }

    fun deleteSkill(path: String) { client?.deleteSkill(path) }

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

    fun refreshProjects() { client?.listProjects() }

    /** Start a chat already filed under [projectId].
     *
     *  cwd is the configured working directory regardless: a project no longer decides where tools run, so a chat in
     *  "cooking" and a chat in "hacking" share a working directory and differ only by the field
     *  that actually means membership. Filing happens once the server hands back a session id --
     *  session/new has no projectId parameter. */
    fun newChatInProject(projectId: String, cwd: String? = null) {
        pendingProjectFiling = projectId
        // A rooted project passes the directory the chat should work in; an ordinary one does
        // not, and its chats run at the default cwd exactly as before.
        newSession(cwd = cwd ?: store.workingDir, kind = SessionKind.CHAT)
    }

    /** Set while a new-chat-in-project is in flight; consumed when Ready delivers the id. */
    private var pendingProjectFiling: String? = null
    fun fileSession(sessionId: String, projectId: String?) {
        client?.assignSessionProject(sessionId, projectId)
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
    // UI and cleared; never a transcript bubble. See the AcpEvent.Error handler.
    val backgroundNotice = mutableStateOf<String?>(null)
    // The current session's running turn id (from session_info_update's activeRunId _meta).
    // Non-null while a run is live == steering is possible; cleared on TurnDone.
    private var activeRunId: String? = null
    // Resume-probe correlation: bumped per probe AND per reply, so a stale timeout can't
    // fire after its probe was answered. syncStamp is the last known (updatedAt, count) —
    // null means "no baseline", which a probe records without triggering a replay.
    private var probeToken = 0
    private var syncStamp: Pair<String?, Int>? = null
    // A session/export result waiting for the UI to hand to the Android share sheet.
    val exportData = mutableStateOf<String?>(null)
    // Handed in by OS entry points (share sheet, shortcut, tile), consumed by the UI.
    val pendingShareText = mutableStateOf<String?>(null)
    val pendingShareImages = mutableStateListOf<ImageBlock>()
    val pendingNewChat = mutableStateOf(false)
    // A notification tap that should open a specific session (finished-turn alert). Consumed in MainActivity.
    val pendingOpenSession = mutableStateOf<String?>(null)
    // Draft attachments live here (process-scoped) so a rotation/recreation doesn't drop picked
    // images — and base64 payloads stay out of the saved-state Bundle (TransactionTooLarge).
    val draftAttachments = mutableStateListOf<ImageBlock>()
    val dynamicColor = mutableStateOf(store.dynamicColor)
    val showAllProviders = mutableStateOf(store.showAllProviders)
    // Live model list for the CURRENT provider, from the server, in memory only -- never
    // persisted. See the AcpEvent.Config/SupportedModels handlers for why.
    val knownModels = mutableStateOf(emptySet<String>())
    // Guards listSupportedModels() to fire once per provider per connection, not on every Config
    // event (which fires on every option change, not just provider switches).
    private var liveModelsFetchedFor: String? = null
    val extensions = mutableStateOf<List<ExtInfo>>(emptyList())
    val extensionsBusy = mutableStateOf(false)
    // Names of the CURRENT session's enabled extensions — drives the in-chat "N tools" indicator
    // and its management sheet. Refreshed on every session open (Ready); optimistically updated by
    // toggleSessionExtension since add/remove replies are empty (no server re-list to react to).
    val sessionExtensionNames = mutableStateOf<List<String>>(emptyList())
    // Full extension objects for the CURRENT session (same reply as the names above). On a
    // federated session these are the PEER's DTOs — the only objects that may be written back
    // to it — and the tool sheet's row source, since the peer's global catalog is unreachable.
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
    // not the live set, so the Tools handler must not treat it as sessionTools. Held as the full
    // ExtInfo so the restore step can round-trip the same object it discovered with.
    private var discovering: ExtInfo? = null

    /** toolCatalog key. A peer's extension can share a name with a local one while exposing a
     *  different tool set, so peer-sourced entries are namespaced by the owning peer. */
    fun catKey(e: ExtInfo): String =
        if (e.fromPeer) "peer:${roamPeer(currentSession.value)}:${e.name}" else e.name

    /** Discovered full tool set for this extension, or null if not discovered yet. */
    fun catalogOf(e: ExtInfo): List<String>? = toolCatalog.value[catKey(e)]

    /** True when a session-scoped tool operation with this ExtInfo would be unsound: the
     *  current session lives on a peer but the DTO is local (e.g. the Settings extension page
     *  open while a remote chat is current). Callers must no-op rather than push it. */
    private fun wrongNode(e: ExtInfo): Boolean =
        (roamPeer(currentSession.value) != null) != e.fromPeer
    // Per-message generation stats (tok/s, cost) for the most recently finished assistant reply.
    // Cleared when a new turn starts so stale numbers don't linger under the next streaming bubble.
    val lastMessageUsage = mutableStateOf<AcpEvent.MessageUsage?>(null)

    /** Fetch the extension list over ACP (agent-global). Reply lands as AcpEvent.Extensions.
     *  goose ≥1.42 dropped goosed's REST /config/extensions; this uses the ACP method instead. */
    fun loadExtensions() {
        val c = client ?: run { extensions.value = emptyList(); extensionsBusy.value = false; return }
        extensionsBusy.value = true
        c.listExtensions()
    }

    // Per-session-type extension profiles (Assistant/Chat/Code) were REMOVED 2026-07-25. They were
    // a third layer on top of the two goose actually defines, and the three fought each other: goose
    // seeds a session from config.yaml, the profile then diffed it back to a stored set, and the
    // in-chat sheet edited the result -- so "what tools does this chat have" had three owners and no
    // single answer. Goose's own model is the two below, and it is enough:
    //
    //   config/extensions/set-enabled  -> writes config.yaml, the default every NEW session starts from
    //   session/extensions/{add,remove} -> this session only, never persisted
    //
    // Settings owns the first, the in-chat sheet owns the second. If a whole class of session needs a
    // different tool set, that is what a recipe's `extensions:` block is for -- goose already scopes
    // per-run there, with `available_tools` to trim inside an extension.

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

    /** Ask goose for this session's active tools; lands as AcpEvent.Tools. */
    fun refreshTools() { discovering = null; client?.listTools() }

    /** Everything the in-chat tool sheet displays, refreshed together on open. */
    fun refreshSessionSheet() { refreshTools(); client?.listSessionExtensions() }

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
        val c = client ?: return
        val unfiltered = JsonObject(ext.raw.toMutableMap().apply {
            put("available_tools", JsonArray(emptyList()))
        })
        discovering = ext
        c.removeSessionExtension(ext.name)
        c.addSessionExtension(unfiltered)   // its reply triggers listTools -- see AcpClient
    }

    /** Restrict `ext` to `allowed` for THIS session only (no config.yaml write). Empty = all. */
    fun setSessionTools(ext: ExtInfo, allowed: Set<String>) {
        if (wrongNode(ext)) return
        val c = client ?: return
        val full = catalogOf(ext).orEmpty()
        // An allowlist equal to the whole catalogue is the same as no allowlist, and storing []
        // keeps it that way if the extension later gains tools.
        val list = if (allowed.size >= full.size && full.isNotEmpty()) emptyList() else allowed.toList()
        val scoped = JsonObject(ext.raw.toMutableMap().apply {
            put("available_tools", JsonArray(list.map { JsonPrimitive(it) }))
        })
        discovering = null
        c.removeSessionExtension(ext.name)
        c.addSessionExtension(scoped)       // its reply triggers listTools
    }

    /** Save `allowed` as the GLOBAL default for `ext` (config.yaml; applies to new chats). */
    fun setDefaultTools(ext: ExtInfo, allowed: Set<String>) {
        val c = client ?: return
        val full = toolCatalog.value[ext.name].orEmpty()
        val list = if (allowed.size >= full.size && full.isNotEmpty()) emptyList() else allowed.toList()
        val updated = JsonObject(ext.raw.toMutableMap().apply {
            put("available_tools", JsonArray(list.map { JsonPrimitive(it) }))
        })
        extensionsBusy.value = true
        c.addExtensionConfig(updated, ext.enabled)
    }

    /** Enable/disable an extension globally (affects new chats); the reply refreshes the list. */
    fun toggleExtension(e: ExtInfo, enabled: Boolean) {
        val c = client ?: return
        extensionsBusy.value = true
        c.setExtensionEnabled(e.configKey, enabled)
    }

    /** Enable/disable one extension for just THIS session (session-scoped API — never touches
     *  config.yaml or any other open session). Optimistic: add/remove replies are empty, so
     *  sessionExtensionNames is updated immediately rather than waiting on a re-list. */
    fun toggleSessionExtension(e: ExtInfo, enabled: Boolean) {
        if (wrongNode(e)) return
        if (enabled) {
            client?.addSessionExtension(e.raw)
            sessionExtensionNames.value = sessionExtensionNames.value + e.name
            detachedPeerExts.value = detachedPeerExts.value.filterNot { it.name == e.name }
        } else {
            client?.removeSessionExtension(e.name)
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
    private var client: AcpClient? = null
    private var clientGen = 0   // bumped per open(); drops events from superseded clients
    // MCP-App template cache: "$extension|$uri" -> HTML. Templates are static per server
    // version and shared across tools/messages/sessions, so one fetch serves everything —
    // including transcript replays, which re-emit every historical tool_call.
    private val appHtmlCache = java.util.concurrent.ConcurrentHashMap<String, String>()
    private val appFetchInFlight = java.util.concurrent.ConcurrentHashMap.newKeySet<String>()
    private var streamingRole: String? = null
    private var live = false
    private var connecting = false
    private var lastSessionId: String? = null
    // True between a ReplayStart wiping `messages` and the following Ready, which re-adds the
    // bubbles of any still-queued prompts (they aren't in the server history the replay rebuilt).
    private var replayWiped = false
    // Replay scroll-pinning: the chat list is keyed, so each replayed bubble inserted at index 0
    // can drift the key-anchored viewport off the exact bottom, after which the atBottom-gated
    // autoscroll stops and history lands scrolled mid-list. While a replay is rebuilding, the UI
    // pins unconditionally; the tick fires one final snap when the rebuild completes.
    val replayActive = mutableStateOf(false)
    val replayDoneTick = mutableStateOf(0)
    // Replayed source messages counted so far — feeds the "Loading… N" title while a big
    // history streams in. Without it a long replay sat on a static "Connecting…" and looked
    // hung; the count proves it is advancing.
    val replayProgress = mutableStateOf(0)
    // Replays rebuild into this buffer instead of mutating `messages`; Ready swaps it in only
    // when the content actually differs. The common background-reconnect replay is identical,
    // so the visible list is never touched and the scroll position survives by construction —
    // every capture/restore scheme raced the UI's own collectors and lost.
    private val replayBuffer = mutableListOf<ChatMessage>()
    private fun t(): MutableList<ChatMessage> =
        if (replayActive.value) replayBuffer else messages
    // The cwd resolved for the in-flight open() -- persisted to store.lastSessionCwd once Ready
    // fires (Ready itself carries no cwd; this is the single source of truth for what we asked for).
    private var pendingOpenCwd: String = ""

    private val optionIds = listOf("provider", "model", "mode", "thinking_effort")

    val configured: Boolean get() = store.hasKey()

    /** Connect using the already-saved host/port/key (post-unlock auto-connect). */
    fun connectSaved() { if (store.hasKey()) open(resume = null) }

    // --- Roam (direct iroh pairing) --------------------------------------------
    // A peer is a `serve --roam` (or `roam share`) host reached directly over
    // iroh — no hub, no `roam:` ids: the peer IS a first-class goose. The phone
    // connects with its own iroh identity (SecureStore) and a pasted card; the
    // host must have accepted this device's key (`roam peers accept`). One
    // active roam connection at a time, parallel to the WS host path.
    data class RoamPeer(val name: String, val card: String, val fingerprint: String)

    val roamPeers = mutableStateListOf<RoamPeer>()
    /** Name of the peer this connection is dialed to, or null (WS mode). */
    @Volatile var currentRoamPeer: String? = null
        private set
    // Last session opened ON EACH peer, so a reconnect resumes it instead of
    // dropping the user into a sessionless state. Separate from the WS path's
    // store.lastSessionId — a peer's session id means nothing to the local host.
    private val roamLastSession = mutableMapOf<String, String>()

    fun loadRoamPeers() {
        roamPeers.clear()
        store.roamPeers.forEach { (n, c) ->
            roamPeers.add(RoamPeer(n, c, runCatching { cardFingerprint(c) }.getOrDefault("")))
        }
    }

    /** The device's iroh secret key, created once and held in SecureStore. */
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
        currentRoamPeer = null
        client?.close(); client = null
        live = false; connecting = false; online.value = false
        status.value = ""
        messages.clear(); currentSession.value = null
    }

    /** Dial a peer and bind the ACP session layer over the roam stream. `resume`
     *  is the peer-side session to load (last session on that peer, or the one
     *  the user just picked); null on first connect leaves the app sessionless
     *  until the user picks — session/new would litter the host's session list. */
    fun connectRoam(name: String, resume: String? = null, createSession: Boolean = false) {
        val peer = roamPeers.firstOrNull { it.name == name } ?: return
        currentRoamPeer = name
        client?.close()
        live = false; connecting = true; online.value = false
        busy.value = false; streamingRole = null; compacting.value = false
        turnInFlight = false; activeRunId = null
        replayWiped = false; replayActive.value = false
        resetGen = -1
        pendingOpenCwd = ""   // no local cwd: session/load asks the PEER for the real one
        liveModelsFetchedFor = null
        messages.clear(); currentSession.value = resume
        status.value = "connecting to ${peer.name}…"
        val gen = ++clientGen
        Thread({
            val link = try {
                RoamStreamLink(roamConnect(roamIdentity(), peer.card, "grouse-android"))
            } catch (t: Throwable) {
                main.post {
                    if (gen == clientGen) {
                        status.value = "roam: ${t.message ?: "connect failed"}"
                        connecting = false; currentRoamPeer = null
                    }
                }
                return@Thread
            }
            main.post {
                if (gen != clientGen) { link.close(); return@post }
                val c = AcpClient("roam://${peer.name}", "", roam = link) { ev ->
                    main.post { if (gen == clientGen) onEvent(ev) }
                }
                c.autoNewSession = createSession
                c.resumeSessionId = resume
                c.resumeCwdKnown = false          // ask the peer for the session's real cwd
                c.desiredOptions = emptyMap()     // the peer's own config applies
                c.desiredRecipeId = pendingRecipeId.also { pendingRecipeId = null }
                client = c
                c.connect()
            }
        }, "grouse-roam-dial").apply { isDaemon = true; start() }
    }

    /** Reconnect whatever is current: the roam peer (resuming the open session)
     *  or the WS host. Every reconnect call site routes through here so roam
     *  mode can't accidentally dial the WS path with a peer session id. */
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

    /** Reconnect silently after Android drops the socket in the background. The resume always
     *  replays: the server history is rebuilt into the transcript on every session/load (see
     *  AcpEvent.ReplayStart), which both repopulates after a background process kill and picks up
     *  turns another client (Desktop, deliver.sh) added to this session while we were away. */
    fun ensureConnected() {
        val peer = currentRoamPeer
        if (peer != null) {
            if (live || connecting) return
            connectRoam(peer, resume = lastSessionId)
            return
        }
        if (!store.hasKey() || live || connecting) return
        open(resume = lastSessionId)
    }

    // A turn was streaming when the socket died. goosed streams a turn ONLY to the connection
    // that prompted it (protocol fact, not a bug here), so the remainder is invisible to the
    // replacement socket and the chat looks frozen mid-tool-calls. Best available recovery:
    // after reconnecting, replay the session a few times so the finished turn shows up.
    private var droppedMidTurn = false
    private var resyncTicks = 0
    private fun turnResyncTick() {
        if (resyncTicks <= 0) return
        resyncTicks--
        // The user started something new (or left) — their action wins; stop quietly.
        if (busy.value || turnInFlight || !appForeground) return
        lastSessionId?.let { reconnectToCurrent() }
        if (resyncTicks > 0) main.postDelayed(::turnResyncTick, 8_000)
    }

    /** Refresh sessions AND the projects that label them. Kept as one call so the two can never
     *  drift -- a session list newer than the project list renders groups labelled by raw id. */
    fun refreshSidebar() { client?.listSessions(); client?.listProjects() }

    /** Archive a session: history stays on disk, it just leaves the list. The soft option --
     *  deleteSession is the permanent one (goose ≥1.44; the old "no delete" note is obsolete). */
    fun archiveSession(sessionId: String) {
        client?.archiveSession(sessionId)
        sessions.value = sessions.value.filterNot { it.sessionId == sessionId }   // optimistic
        if (sessionId == store.assistantSessionId) store.assistantSessionId = null
    }

    /** Serialize a session server-side; the reply lands in [exportData] and the UI opens the
     *  Android share sheet with it. */
    fun exportSession(sessionId: String) { client?.exportSession(sessionId) }

    /** Publish this device's UnifiedPush endpoint into the server's config (GROUSE_PUSH_ENDPOINT),
     *  so the server's senders always POST to the current token. This is what makes a reinstall /
     *  endpoint-rotation self-heal instead of silently pushing at a dead token. Best-effort. */
    fun publishPushEndpoint(url: String) {
        if (url.isNotBlank()) client?.upsertConfig("GROUSE_PUSH_ENDPOINT", url)
    }

    /** Answer a pending elicitation form and drop it from the queue. */
    fun answerElicitation(e: AcpEvent.Elicitation, values: Map<String, JsonPrimitive>?, cancelled: Boolean = false) {
        client?.respondElicitation(e.requestKey, values, cancelled)
        elicitations.remove(e)
    }

    /** Delete a session outright (history gone server-side). Archive remains the soft option. */
    fun deleteSession(sessionId: String) {
        client?.deleteSession(sessionId)
        sessions.value = sessions.value.filterNot { it.sessionId == sessionId }   // optimistic
        if (sessionId == store.assistantSessionId) store.assistantSessionId = null
    }

    /** Move a chat into a project (or back to /state): the sanctioned working_dir rewrite.
     *  Also the in-app repair for sessions stranded by a renamed project directory. */
    fun moveSession(sessionId: String, cwd: String) {
        client?.updateWorkingDir(sessionId, cwd)
        sessions.value = sessions.value.map {          // optimistic
            if (it.sessionId == sessionId) it.copy(cwd = cwd) else it
        }
        store.rememberSessionCwds(listOf(sessionId to cwd))
    }

    /** Set a session's title (goose _goose/unstable/session/rename; the reply re-lists). */
    fun renameSession(sessionId: String, title: String) {
        val t = title.trim()
        if (t.isEmpty()) return
        client?.renameSession(sessionId, t)
        sessions.value = sessions.value.map {          // optimistic
            if (it.sessionId == sessionId) it.copy(title = t) else it
        }
    }

    fun openSession(sessionId: String, knownKind: SessionKind? = null) {
        // Cancel any deferred "open the assistant thread" -- the user has since picked a specific
        // session and that choice wins. Without this, a pendingOpenAssistant set while offline (its
        // refreshSidebar() is a no-op with no client) survives until the NEXT Sessions event, which
        // arrives from the refreshSidebar() in this very open()'s Ready handler -- and then reopens
        // the assistant on top of the session just chosen. The chat visibly switches and the next
        // message lands in the assistant thread.
        pendingOpenAssistant = false
        messages.clear(); lastSessionId = sessionId; currentSession.value = sessionId
        // Roam: the session lives on the peer — reconnect the roam link resuming it.
        // (Its cwd is resolved by asking the peer; see connectRoam/resumeCwdKnown.)
        val peer = currentRoamPeer
        if (peer != null) { connectRoam(peer, resume = sessionId); return }
        // A caller resuming a session it already has cached (e.g. connectHome's assistant shortcut)
        // can pass knownKind to skip the sessions.value lookup, which may not be populated yet on a
        // cold start; open() falls back to that lookup (then CHAT) when knownKind is null.
        val kind = knownKind ?: sessions.value.firstOrNull { it.sessionId == sessionId }
            ?.let { ConnectionManager.sessionKind(it) }
        open(resume = sessionId, kind = kind)
    }

    fun newSession(
        cwd: String = "",
        kind: SessionKind = SessionKind.CHAT,
        recipeId: String? = null,
    ) {
        pendingOpenAssistant = false      // same as openSession: an explicit choice cancels it
        messages.clear(); lastSessionId = null; currentSession.value = null; config.value = emptyList()
        // Roam: a new chat is session/new ON THE PEER (autoNewSession=true) — the
        // local cwd is meaningless there, and the peer's own config applies.
        val peer = currentRoamPeer
        if (peer != null) { pendingRecipeId = recipeId; connectRoam(peer, createSession = true); return }
        pendingRecipeId = recipeId
        open(resume = null, cwd = cwd, kind = kind)
    }

    /** Carried to the next session/new. Cleared by open() once handed to the client, so a plain
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
     *  No model is involved: a throwaway AcpClient opens a session at /state purely to get a
     *  sessionId, then invokes developer__shell through goose's _goose/unstable/tools/call --
     *  deterministic, exact output, near-instant. Because the session never receives a prompt
     *  it has ZERO messages, and session/list filters message-less sessions, so it never
     *  appears anywhere -- no archiving dance needed. (The earlier version prompted the fast
     *  model to run commands; it was slow, paraphrased output, and its session flashed into
     *  the list until archived.) */
    private fun runUtilityTool(command: String, timeoutMs: Long = 30_000, onDone: (String?, String) -> Unit) =
        runToolDirect("shell", kotlinx.serialization.json.buildJsonObject {
            put("command", kotlinx.serialization.json.JsonPrimitive(command))
        }, timeoutMs, onDone)

    /** Invoke ONE tool on a throwaway session and hand back its text, with no model turn.
     *  Generalised out of runUtilityTool, which was the shell-only special case. */
    private fun runToolDirect(
        tool: String,
        args: kotlinx.serialization.json.JsonObject,
        timeoutMs: Long = 30_000,
        onDone: (String?, String) -> Unit,
    ) {
        val url = "wss://${store.host}:${store.port}/acp"
        var boot: AcpClient? = null
        var finished = false
        lateinit var watchdog: Runnable
        fun finish(err: String?, out: String) {
            if (finished) return
            finished = true
            main.removeCallbacks(watchdog)
            val b = boot
            main.postDelayed({ b?.close() }, 500)
            boot = null
            onDone(err, out)
        }
        watchdog = Runnable { finish("Timed out talking to the server.", "") }
        main.postDelayed(watchdog, timeoutMs)
        boot = AcpClient(url, store.secretKey) { ev ->
            main.post {
                if (finished) return@post
                when (ev) {
                    is AcpEvent.Ready -> {
                        // Builtin developer tools are UNPREFIXED ("shell", not developer__shell —
                        // matches permission.yaml's bare names). Small delay: extensions attach
                        // async after session/new.
                        val sid = ev.sessionId
                        main.postDelayed({ if (!finished) boot?.callTool(sid, tool, args) }, 800)
                    }
                    is AcpEvent.DirectToolResult ->
                        if (ev.isError) finish(ev.text.ifBlank { "tool call failed" }, ev.text)
                        else finish(null, ev.text)
                    // No UI here — never let a form OR approval request hang the utility call.
                    is AcpEvent.Elicitation -> boot?.respondElicitation(ev.requestKey, null, cancelled = true)
                    is AcpEvent.Permission -> boot?.respondPermission(ev.toolCallId, null)
                    is AcpEvent.Error -> finish(ev.text, "")
                    else -> {}
                }
            }
        }.also {
            it.desiredCwd = store.workingDir
            it.connect()
        }
    }

    private fun cleanProjectName(raw: String): String? {
        val name = raw.trim().trim('/').removePrefix("projects/")
            .removePrefix("workspace/").trim('/')
        if (name.isEmpty() || name.contains("..") || name.contains('/') ||
            name.any { it.isWhitespace() } || name.contains('\'') || name.contains('"')) return null
        return name
    }

    /** Create /projects/<name> on the server (null = success, else error). Deterministic
     *  direct mkdir -- no model. */
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
        client?.createProject(name)
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

    /** Delete a project: archive its chats, drop it from recents, and remove the server
     *  directory ONLY if empty (rmdir, never rm -rf -- a non-empty project keeps its files
     *  and merely disappears from the list). Reports a human-readable outcome note. */
    /** Delete a project and unfile its chats.
     *
     *  This used to `rmdir` a directory and lean on the shell's exit code for its message, which
     *  is why an earlier pass left it calling `true` and reporting success while the project
     *  stayed in the list -- nothing server-side was ever deleted. It now calls sources/delete
     *  with the project's source path.
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
        client?.deleteProject(proj.path)
        projects.value = projects.value.filterNot { it.id == proj.id }   // optimistic; reply re-lists
        onResult(
            if (affected.isEmpty()) "Project deleted."
            else "Project deleted. ${affected.size} chat${if (affected.size == 1) "" else "s"} moved to Unfiled."
        )
    }

    /** The persistent "goose-assistant" thread (briefings/proactive/voice land here), if it exists. */
    /** The assistant thread's id.
     *
     *  The CACHED id wins over the title lookup, not the other way round. Two reasons, both
     *  measured: session/list omits sessions with no content, so a freshly reset thread is
     *  invisible to a title search; and resets had left FOUR sessions titled "goose-assistant" at
     *  once (two of them empty), so a title search is ambiguous as well as blind. The cached id is
     *  the one this app actually created and renamed, so it is the authoritative answer; the title
     *  lookup is the fallback for a fresh install that has no cache yet. */
    fun assistantSessionId(): String? =
        store.assistantSessionId
            ?: sessions.value.firstOrNull { it.title == ASSISTANT_TITLE }?.sessionId

    /** True when the on-screen conversation IS the privileged assistant thread. */
    val onAssistant: Boolean get() = currentSession.value != null && currentSession.value == assistantSessionId()

    @Volatile private var pendingOpenAssistant = false

    /** Open the privileged assistant thread; if the session list isn't loaded yet, refresh it and
     *  open as soon as it arrives (see the Sessions event handler). */
    fun openAssistant() {
        val id = assistantSessionId()
        if (id != null) { openSession(id, knownKind = SessionKind.ASSISTANT); return }
        // No id yet: defer until a session list arrives -- but only if one can actually arrive.
        // With no client, refreshSidebar() does nothing and the flag would sit armed indefinitely.
        if (client != null) { pendingOpenAssistant = true; refreshSidebar() }
        else beginAssistantThread()
    }

    // --- Assistant-thread reset / (re)create ---
    // The reset is bound to the SPECIFIC connection generation it opens (resetGen), so an
    // unrelated Ready — e.g. the user tapping another chat before the fresh session connects —
    // can never be mistaken for the reset's session and wrongly renamed to ASSISTANT_TITLE
    // (which would grant that chat the privileged auto-approve policy). Both renames run on that
    // one live socket in order, so nothing is lost to a close race. If a different open()
    // supersedes the pending reset, it's abandoned cleanly (see open()).
    private var resetGen = -1                 // clientGen of the reset-initiated connect
    // resetOldId (the thread to rename aside once the fresh one went live) is GONE: reset no
    // longer creates a replacement thread, so there is never an old one to archive.

    /** Clear the assistant thread IN PLACE, keeping its session id.
     *
     *  Used to archive the thread under a dated name and stand up a fresh one -- which changed
     *  the id, so every client had to detect the rename and follow it. That is precisely the
     *  mechanism that forked the thread on 2026-07-26 and again on 2026-07-30, and it is why the
     *  nightly rotation was replaced by compaction.
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

    /** Open a fresh session that will become the ASSISTANT_TITLE thread once live. `archiveOld`,
     *  if non-null, is renamed aside first (reset); null just creates a new assistant thread
     *  (recovery when none exists). Binds to the connection generation this open() creates. */
    private fun beginAssistantThread() {
        resetGen = clientGen + 1              // the gen newSession()'s open() is about to create
        newSession(kind = SessionKind.ASSISTANT)
    }


    // --- Server-side goose config (global config.yaml, edited over ACP) ---
    /** Current server values, populated by loadServerConfig(); empty until read. */
    val serverContextLimit = mutableStateOf("")
    val serverFastModel = mutableStateOf("")
    /** Generic mirror of config/read replies, keyed by config key — the phone-writable
     *  server settings channel (schedule times, etc.). */
    val serverConfig = androidx.compose.runtime.mutableStateMapOf<String, String>()

    // loadAssistantExtensions / discoverAssistantTools / setAssistantTools lived here and are
    // GONE (2026-08-01). They edited the Assistant session's extension set from the settings
    // screen, which is the same thing the in-chat tools sheet does for whatever chat you are in
    // -- two code paths for one operation, and only one of them was ever exercised. The thread
    // is a chat; its tools are set in it.

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
    fun readServerConfig(vararg keys: String) { keys.forEach { client?.readConfig(it) } }
    fun writeServerConfig(key: String, value: String) {
        client?.upsertConfig(key, value)
        serverConfig[key] = value   // optimistic
    }

    /** Read the app-editable global goose settings so Settings can show current values. */
    fun loadServerConfig() {
        client?.readConfig("GOOSE_CONTEXT_LIMIT")
        client?.readConfig("GOOSE_FAST_MODEL")
    }

    /** Upsert a global goose setting (takes effect for NEW sessions/tasks), then re-read to confirm. */
    fun setServerConfig(key: String, value: String) {
        client?.upsertConfig(key, value)
        client?.readConfig(key)
    }

    fun setOption(configId: String, value: String) {
        store.saveOption(configId, value)
        client?.setConfigOption(configId, value)
    }

    fun send(text: String, images: List<ImageBlock> = emptyList()) {
        // Keep the images ON the message so the bubble renders the actual thumbnail(s), not a
        // "[📎 N]" placeholder. (Live-session only — a session reloaded from the server replays
        // text; images aren't reconstructed from the replayed content blocks.)
        messages.add(ChatMessage("user", text, images)); streamingRole = null; busy.value = true
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
        if (live && client?.ready == true && !turnInFlight) {
            turnInFlight = true
            lastSessionId?.let { store.pendingPushSessionId = it }
            client?.sendPrompt(text, images, expect = currentSession.value)
        } else if (live && client?.ready == true && activeRunId != null && images.isEmpty()) {
            // A turn is running AND we know its run id: STEER — inject into the live turn
            // instead of waiting for it to end. The server validates the id, so a run that
            // ended between typing and sending fails loudly (foreground error) rather than
            // spawning a stray turn. Images still queue: steering is text-only by design.
            client?.steer(text, activeRunId!!)
        } else if (live) {
            // A turn is already running. Queue rather than firing a second sendPrompt into the
            // same session -- concurrent prompts interleave in the transcript and the second
            // reply is attributed to the wrong question. Flushed on TurnDone.
            enqueue(PendingSend(text, images))
        } else {
            // Not connected yet (initial connect / silent reconnect window): queue and connect,
            // rather than calling sendPrompt against a session-less client (which just errors and
            // loses the message). Flushed in the Ready branch. The resume's replay wipes the local
            // transcript (including this just-added bubble); Ready re-adds the queued bubbles on
            // top of the rebuilt history -- see the ReplayStart/Ready handlers.
            enqueue(PendingSend(text, images))
            if (!connecting) reconnectToCurrent()
        }
    }

    /** Send once connected — used by notification replies, which may arrive disconnected. */
    fun sendWhenReady(text: String) = send(text)

    fun setForeground(fg: Boolean) {
        appForeground = fg
        if (fg) {
            notifier.cancelAlert()
            ensureConnected()
            // This block used to REOPEN THE SESSION UNCONDITIONALLY on every foreground —
            // a 2-second alt-tab paid a full session/load replay of the whole transcript,
            // which is exactly the "whole app reconnects every time I look away" complaint.
            // Now: one cheap session/info probe. Three outcomes:
            //   reply, nothing changed  -> do nothing (the overwhelmingly common case)
            //   reply, updatedAt/count moved -> another client touched the session; replay
            //   no reply in 10s         -> socket died while frozen and OkHttp hasn't
            //                              noticed (`live` is stale) — force a reconnect,
            //                              which ensureConnected can't do (it trusts `live`).
            // No probe (and no watchdog) while a replay is streaming: the chunks themselves
            // prove the socket is alive, a reply would queue behind the stream (a huge replay
            // can outrun the window), and a force-reconnect would restart the whole replay.
            if (live && !busy.value && !turnInFlight && !replayActive.value &&
                !(currentRoamPeer != null && lastSessionId == null)) {
                (lastSessionId ?: store.lastSessionId)?.let { sid ->
                    val tok = ++probeToken
                    client?.probeSession(sid)
                    main.postDelayed({
                        if (tok == probeToken && appForeground && !turnInFlight && !replayActive.value) {
                            live = false
                            reconnectToCurrent()
                        }
                    }, 10_000)
                }
            }
            // Re-ask the server for its model list every time we come back. It was previously
            // fetched ONCE per provider per connection (guarded by liveModelsFetchedFor, which
            // only resets in open()), so a long-lived socket never noticed the set changing --
            // models renamed or added server-side stayed invisible until a full reconnect, and
            // ensureConnected() above can't force one because it early-returns on `live`.
            // One /v1/models round trip through goose; cheap enough to just redo on resume.
            if (live) {
                config.value.firstOrNull { it.id == "provider" }?.currentValue
                    ?.takeIf { it.isNotBlank() }
                    ?.let { client?.listSupportedModels(it) }
            }
            if (!store.persistentConnection && !busy.value) stopService()
        } else {
            // Losing focus is the last reliable moment before a possible process kill —
            // snapshot the transcript now so a cold relaunch can repaint it instantly.
            saveTranscriptCache()
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

    /** Stop the running turn — reliably, even if goose is wedged mid-turn and won't honor the
     *  polite ACP cancel. We send the cancel, free the UI immediately, then reconnect (resume):
     *  reopening bumps the client generation so any late events from the stuck connection are
     *  dropped, and the fresh client comes back idle. The reconnect's replay rebuilds the
     *  transcript to whatever the server persisted of the cancelled turn. */
    fun cancel() {
        client?.cancel()
        streamingRole = null
        busy.value = false
        turnInFlight = false   // wire is free again; without this the queue never drains
        clearQueue()           // Stop means stop: don't let queued prompts fire after a cancel
        if (store.hasKey() || currentRoamPeer != null) reconnectToCurrent()
    }

    /** Compact the conversation history to reclaim context (goose /compact command). */
    fun compact() {
        busy.value = true
        compacting.value = true   // client-initiated: don't wait for the server's own status echo
        client?.sendPrompt("/compact")
    }

    /** Answer the given approval request; null optionId denies (cancelled). */
    fun answerPermission(p: AcpEvent.Permission, optionId: String?) {
        client?.respondPermission(p.toolCallId, optionId)
        permissions.remove(p)
    }

    // --- Transcript snapshot cache ---------------------------------------------------------
    // The replay buffer keeps the transcript on screen through a live reconnect, but it can't
    // help when Android kills the process: a cold start has an empty `messages` and the user
    // stares at the full-screen "Connecting…" until session/load finishes replaying. Snapshot
    // the text bubbles to disk on every focus loss, and seed `messages` from the snapshot when
    // a resume starts with an empty transcript — the replay then swaps in the server truth
    // through the existing buffer without the blank screen ever appearing.
    private fun transcriptCacheFile(sid: String): java.io.File =
        java.io.File(java.io.File(appContext.filesDir, "transcripts").apply { mkdirs() },
            sid.replace(Regex("[^A-Za-z0-9_-]"), "_") + ".json")

    private fun saveTranscriptCache() {
        val sid = currentSession.value ?: lastSessionId ?: return
        // Mid-replay `messages` holds the last fully-shown transcript (pre-replay content or the
        // cold-start paint) — still a valid snapshot, and the only one a process kill during a
        // long load would leave. No replayActive gate: every path keeps messages in step with
        // currentSession (openSession clears before painting; newSession nulls both), and an
        // empty list is caught below. The next background after Ready overwrites this.
        // Text bubbles only: tool cards, MCP-app views and usage stats don't survive a replay
        // either, so caching them would just make the swap-in visibly churn.
        val snap = messages.filter { (it.role == "user" || it.role == "assistant") && it.text.isNotBlank() }
            .takeLast(60)
        if (snap.isEmpty()) return
        runCatching {
            val arr = org.json.JSONArray()
            snap.forEach { m -> arr.put(org.json.JSONObject().put("r", m.role).put("t", m.text)) }
            transcriptCacheFile(sid).writeText(arr.toString())
        }
        // The cache is a cold-start nicety, never user data: cap it so a long-lived install
        // doesn't accumulate one file per session ever opened. Best-effort, like the write.
        pruneTranscriptCache(java.io.File(appContext.filesDir, "transcripts"), keep = 20)
    }

    private fun loadTranscriptCache(sid: String): List<ChatMessage> = runCatching {
        val f = transcriptCacheFile(sid)
        if (!f.exists()) return emptyList()
        val arr = org.json.JSONArray(f.readText())
        (0 until arr.length()).map { i ->
            val o = arr.getJSONObject(i)
            ChatMessage(o.getString("r"), o.getString("t"))
        }
    }.getOrElse { emptyList() }

    private fun open(resume: String?, cwd: String? = null, kind: SessionKind? = null) {
        // Cold start (or session switch): paint the cached transcript immediately so the reader
        // sees content, not a Connecting screen, while the replay rebuilds the real one.
        if (resume != null && messages.isEmpty())
            loadTranscriptCache(resume).takeIf { it.isNotEmpty() }?.let { messages.addAll(it) }
        // If a reset/create-assistant is pending but THIS open() isn't the one it scheduled
        // (clientGen+1 != resetGen), a different navigation superseded it — abandon it so a later
        // unrelated Ready can't complete a stale rename.
        if (resetGen >= 0) {
            if (clientGen + 1 != resetGen) { resetGen = -1 }
        }
        client?.close()
        live = false; connecting = true; online.value = false
        // A new client can't receive the old client's TurnDone, so clear turn state here.
        // Otherwise a hung/dropped turn leaves busy=true and every new chat + reconnect
        // inherits a stuck "goose is thinking…" with nothing sent. Same logic for compacting:
        // it's only cleared by TurnDone/Error/a matched CompactionStatus on the SAME client — if
        // you switch sessions or background mid-compact, the old client gets superseded (dropped
        // by the clientGen guard) before any of those arrive, and compacting (global, not
        // per-session) stays stuck true forever after, permanently hiding the usage line under it
        // (they're if/else-if) on every session including ones that were never compacting at all.
        busy.value = false; streamingRole = null; compacting.value = false
            // The superseded client will never report TurnDone here, so free the wire.
            turnInFlight = false
        activeRunId = null   // a stale run id belongs to the superseded session; steer would target the wrong run
        liveModelsFetchedFor = null   // re-fetch supported models fresh on every new connection
        replayWiped = false
        replayActive.value = false
        val url = "wss://${store.host}:${store.port}/acp"
        status.value = if (resume == null) "connecting to $url" else "loading session…"
        val saved = store.savedOptions(optionIds)
        // Resolve the cwd for this open(): an explicit param wins (new session creation always
        // knows its own target); otherwise, for a resume, prefer the cached SessionInfo's cwd
        // (sessions.value, if already loaded) and fall back to the last-persisted cwd for a cold
        // start before any session/list round-trip has happened. session/load's cwd param SILENTLY
        // REWRITES the session's working_dir if wrong, so this must be right, not just "close enough".
        // There used to be an "assistant hard rule" here pinning that thread to /state BY
        // CONSTRUCTION. It stopped being true on 2026-07-30, when conversational sessions moved
        // under <home>/Projects/ so Goose Desktop would group them as projects -- and
        // because session/load REWRITES working_dir, this line did not merely guess wrong, it
        // actively dragged the Assistant back to /state within seconds of every correction,
        // including edits made directly in the sessions DB. Ask the server instead; after
        // the default cwd was fixed this was the ONE remaining hardcoded /state, and it silently
        // undid that fix.
        //
        // Resolution order for a resume: live cache -> the per-session cwd map -> ASK THE SERVER
        // (null: the client queries _goose/unstable/session/info before session/load). NEVER a
        // guess: session/load rewrites working_dir when handed the wrong cwd, and a global
        // last-used guess re-homed the assistant thread into a project once.
        val resolvedCwd: String? = cwd?.takeIf { it.isNotBlank() } ?: if (resume == null) store.workingDir else
            sessions.value.firstOrNull { it.sessionId == resume }?.cwd?.takeIf { it.isNotBlank() }
                ?: store.sessionCwd(resume)
        pendingOpenCwd = resolvedCwd ?: ""
        // Tag this client's events with a generation; a just-closed client still fires
        // onClosed/onFailure asynchronously and its stale "disconnected" must not flip us offline
        // after the new client is already live.
        val gen = ++clientGen
        client = AcpClient(url, store.secretKey) { ev -> main.post { if (gen == clientGen) onEvent(ev) } }.also {
            it.desiredOptions = if (resume == null) saved else emptyMap()
            it.resumeSessionId = resume
            it.resumeCwd = resolvedCwd ?: store.workingDir
            it.resumeCwdKnown = resolvedCwd != null
            it.desiredCwd = resolvedCwd ?: store.workingDir
            it.desiredRecipeId = pendingRecipeId.also { _ -> pendingRecipeId = null }
            it.connect()
        }
    }

    private fun onEvent(ev: AcpEvent) {
        when (ev) {
            // Direct tool replies only occur on utility clients, which have their own handler.
            is AcpEvent.DirectToolResult -> {}
            is AcpEvent.Elicitation -> elicitations.add(ev)
            is AcpEvent.Status -> {
                status.value = ev.text
                if (ev.text == "ready — pick a session") {
                    // Roam connected with no session bound (first connect to a peer):
                    // the host accepted this device, the drawer lists its sessions,
                    // the user picks one — opening it resumes over the same link.
                    live = true; connecting = false; online.value = true
                }
                if (ev.text == "disconnected") {
                    if (turnInFlight) droppedMidTurn = true   // see turnResyncTick
                    live = false; connecting = false; online.value = false
                }
            }
            is AcpEvent.Error -> {
                if (ev.background) {
                    // A failed sidebar/config refresh is not the conversation's problem: no
                    // transcript bubble, and ABOVE ALL no busy/turn-state reset — that reset
                    // once cancelled a live turn because a schedules poll errored.
                    // But the flags those calls set MUST clear, or the failure sticks: a dead
                    // tools/list left `discovering` armed and the NEXT tools reply triggered a
                    // spurious allowlist write; a dead extensions/list left the sheet spinning.
                    if (ev.text.startsWith("_goose/unstable/tools/list")) discovering = null
                    if (ev.text.startsWith("_goose/unstable/config/extensions/list")) extensionsBusy.value = false
                    backgroundNotice.value = ev.text
                    android.util.Log.w("Grouse", "background rpc error: ${ev.text}")
                } else {
                    messages.add(ChatMessage("error", ev.text)); streamingRole = null; busy.value = false; turnInFlight = false
                    compacting.value = false   // safety net: a dropped/garbled status must never stick
                }
            }
            is AcpEvent.ToolCall -> {
                if (ev.appKey.isNotBlank()) {
                    // Tool declared a server-hosted UI (autovisualiser et al). Add the bubble
                    // immediately — with the template if cached, empty otherwise — and fetch
                    // once per key no matter how many bubbles are waiting on it.
                    t().add(ChatMessage("mcpapp", ev.title, detail = ev.appInput,
                        toolCallId = ev.toolCallId, appKey = ev.appKey,
                        appHtml = appHtmlCache[ev.appKey] ?: ""))
                    if (!appHtmlCache.containsKey(ev.appKey) && appFetchInFlight.add(ev.appKey))
                        client?.readAppResource(ev.appKey, ev.appUri, ev.appExt)
                } else {
                    t().add(ChatMessage("tool", ev.title, detail = ev.detail,
                        toolCallId = ev.toolCallId, status = "in_progress"))
                }
                streamingRole = null
            }
            is AcpEvent.AppResource -> {
                appFetchInFlight.remove(ev.key)
                if (ev.html.isNotBlank()) {
                    appHtmlCache[ev.key] = ev.html
                    val tr = t()
                    for (i in tr.indices)
                        if (tr[i].role == "mcpapp" && tr[i].appKey == ev.key && tr[i].appHtml.isEmpty())
                            tr[i] = tr[i].copy(appHtml = ev.html)
                }
            }
            is AcpEvent.ToolCallUpdate -> {
                if (ev.toolCallId.isNotBlank()) {
                    val i = t().indexOfLast { it.role == "tool" && it.toolCallId == ev.toolCallId }
                    if (i >= 0) t()[i] = t()[i].copy(
                        status = ev.status.ifBlank { t()[i].status },
                        // Live shell chunks APPEND (capped — a verbose build log must not grow
                        // a transcript entry without bound); the final completion update still
                        // replaces, so the finished chip shows the tool's real result.
                        output = when {
                            ev.live -> (t()[i].output + ev.output).takeLast(4000)
                            ev.output.isNotBlank() -> ev.output
                            else -> t()[i].output
                        },
                    )
                }
            }
            is AcpEvent.ActiveRun -> {
                // Only trust run ids for the session on screen — a notification for another
                // session must not arm steering against the wrong run.
                if (ev.sessionId == currentSession.value) activeRunId = ev.runId
            }
            is AcpEvent.Probe -> {
                probeToken++   // cancels the pending dead-socket timeout
                // While a replay streams, the socket is demonstrably alive and the rebuild is
                // already bringing the fresh content — acting on the verdict here would just
                // open() and RESTART the replay (same bug the watchdog guard above fixes).
                // Keep only the baseline refresh; the replay's Ready supersedes any verdict.
                if (!replayActive.value) when {
                    // The probe itself failed: session gone or socket dead — reconnect.
                    ev.messageCount < 0 -> if (!turnInFlight) { reconnectToCurrent() }
                    // Baseline exists and moved: another client changed the session; replay.
                    syncStamp != null && syncStamp != (ev.updatedAt to ev.messageCount) ->
                        if (!busy.value && !turnInFlight) reconnectToCurrent()
                    // else: nothing changed — the 2-second alt-tab costs one info call.
                }
                syncStamp = ev.updatedAt to ev.messageCount
            }
            is AcpEvent.SessionExport -> exportData.value = ev.data
            is AcpEvent.SessionInfoChanged -> {
                // Live title/updatedAt sync (auto-naming after the first turn, renames from any
                // client) — previously only visible after a full session re-list.
                sessions.value = sessions.value.map {
                    if (it.sessionId == ev.sessionId) it.copy(
                        title = ev.title ?: it.title,
                        updatedAt = ev.updatedAt ?: it.updatedAt,
                    ) else it
                }
            }
            is AcpEvent.ModeChanged -> {
                config.value = config.value.map {
                    if (it.id == "mode") it.copy(currentValue = ev.modeId) else it
                }
            }
            is AcpEvent.Usage -> usage.value = ev
            is AcpEvent.CompactionStatus -> {
                // Substring match on goose's own status copy — see agents/agent.rs (aaif-goose/goose,
                // commit 1e03bbb5): Notice "Exceeded auto-compact threshold... Performing
                // auto-compaction...", Progress "goose is compacting the conversation...", Notice
                // "Compaction complete". Both start and end lines are type Notice, so "notice vs
                // progress" can't gate the indicator — the text itself is the only stable signal. If
                // a future goose upgrade rewords these, this just stops firing (fails safe: no
                // spinner, never a wrong one) — re-check against that file after a server bump.
                val m = ev.message.lowercase()
                if (m.contains("compact")) compacting.value = true
                if (m.contains("complete") || m.contains("error")) compacting.value = false
            }
            is AcpEvent.Chart -> { t().add(ChatMessage("chart", ev.spec)); streamingRole = null }
            is AcpEvent.TurnDone -> {
                streamingRole = null; compacting.value = false
                turnInFlight = false
                activeRunId = null   // the run this id named is over; steering it would fail
                syncStamp = null     // our own turn changed the server state; re-baseline on next probe
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
                    client?.sendPrompt(queued.text, queued.images, expect = currentSession.value)
                } else if (!store.persistentConnection) stopService()
            }
            is AcpEvent.ReplayStart -> {
                // A session/load replay is about to stream: the server transcript is ground truth
                // (it may hold turns Desktop or deliver.sh added while this app wasn't looking),
                // so rebuild from scratch — into the side buffer; `messages` stays visible and
                // untouched until Ready decides whether anything actually changed.
                replayBuffer.clear()
                replayProgress.value = 0
                streamingRole = null
                replayWiped = true
                replayActive.value = true
            }
            is AcpEvent.AgentChunk -> {
                // Replay boundary: a NEW messageId means a new source message — break the
                // bubble instead of gluing (consecutive assistant messages, e.g. a briefing
                // relay followed by an appended digest, used to merge into one). The same
                // boundary counts a replayed message for the Loading progress.
                if (ev.messageId != null && ev.messageId != streamMsgId) {
                    streamingRole = null
                    if (replayActive.value) replayProgress.value++
                }
                if (ev.messageId != null) streamMsgId = ev.messageId
                appendStream("assistant", ev.text)
            }
            is AcpEvent.ThoughtChunk -> appendStream("thought", ev.text)
            is AcpEvent.UserChunk -> {
                t().add(ChatMessage("user", ev.text)); streamingRole = null
                if (replayActive.value) replayProgress.value++   // one UserChunk per replayed user message
            }
            is AcpEvent.Config -> if (ev.options.isNotEmpty()) {
                config.value = ev.options
                // A federated session's options are the PEER's (session/load passes them
                // through). Persisting them would overwrite the sticky local defaults with
                // the peer's model/provider, and fetching "supported models" would ask
                // the LOCAL provider registry about the peer's provider id. Display only.
                if (roamPeer(currentSession.value) != null) return
                // Persist the true current values so re-apply on reconnect can't drift
                // (e.g. leave a model selected after switching provider).
                ev.options.forEach { if (it.currentValue.isNotBlank()) store.saveOption(it.id, it.currentValue) }
                // Models come from the SERVER, live, and are never persisted. The old design kept
                // a per-provider set in SharedPreferences that only ever GREW: every slug goose
                // ever reported stayed forever, so a model retired server-side (Qwen3_1.7B), an
                // alias that was renamed (the whole 2026-07-25 rename), and slugs that leaked in
                // from another provider during a switch (z-ai/glm-5.2, gpt-4o under openai) all
                // accumulated with no way to clear them short of wiping app data. Two separate
                // one-time migrations existed in SecureStore purely to repair that cache, and a
                // shape heuristic here tried to stop it being poisoned in the first place.
                //
                // None of that is needed: fetch_supported_models is authoritative and cheap. Ask
                // the server, show the answer, keep nothing.
                val provider = ev.options.firstOrNull { it.id == "provider" }?.currentValue ?: ""
                if (provider != liveModelsFetchedFor) {
                    // Provider changed (or first Config): drop the previous provider's list
                    // immediately so its slugs cannot be shown under the new one, even briefly.
                    knownModels.value = emptySet()
                    liveModelsFetchedFor = provider
                    if (provider.isNotBlank()) client?.listSupportedModels(provider)
                }
            }
            is AcpEvent.SupportedModels -> {
                // Straight replace, in memory only. Ignore a reply for a provider we have since
                // switched away from, so a slow response can't repopulate the wrong list.
                val currentProvider = config.value.firstOrNull { it.id == "provider" }?.currentValue
                if (ev.providerId == currentProvider) knownModels.value = ev.models.toSet()
            }
            is AcpEvent.Ready -> {
                live = true; connecting = false; online.value = true
                lastSessionId = ev.sessionId
                // A roam peer's session id means nothing to the local WS host — keep the
                // WS resume target and each peer's resume target separate.
                if (currentRoamPeer != null) roamLastSession[currentRoamPeer!!] = ev.sessionId
                else store.lastSessionId = ev.sessionId
                // Ready itself carries no cwd -- record what this open resolved (see open()).
                // Blank = the server was asked via session/info; the next session/list merge
                // records the authoritative value instead.
                if (pendingOpenCwd.isNotBlank())
                    store.rememberSessionCwds(listOf(ev.sessionId to pendingOpenCwd))
                // A chat started from inside a project gets filed the moment it has an id --
                // session/new takes no projectId, so membership is a second call.
                pendingProjectFiling?.let { pid ->
                    pendingProjectFiling = null
                    fileSession(ev.sessionId, pid)
                }
                pendingClearOnReady?.let { target ->
                    pendingClearOnReady = null
                    if (target == ev.sessionId) {
                        messages.clear()          // the replay we just took is about to be void
                        client?.sendPrompt("/clear")
                    }
                }
                if (droppedMidTurn) {
                    // The turn we lost is still finishing server-side; poll it back into view.
                    droppedMidTurn = false
                    resyncTicks = 3
                    main.postDelayed(::turnResyncTick, 8_000)
                }
                currentSession.value = ev.sessionId
                // Populate the in-chat "N tools" indicator and the per-extension tool lists.
                // MCP-backed extensions come up asynchronously AFTER the session is ready: a
                // tools/list fired here returns only the builtins (measured -- nextcloud, beeper,
                // kagi and memory were all absent from a list taken immediately). Ask again shortly
                // for the full picture rather than caching a half-built one.
                // Since roam-5 the probes route to the owning peer for federated sessions
                // too (tools/list and session/extensions/list are session-scoped), so they
                // run unconditionally. Stale state from the previous session is cleared by
                // the replies; the detached-row cache is per-session and cleared here.
                detachedPeerExts.value = emptyList()
                client?.listTools()
                val genAtReady = clientGen
                main.postDelayed({ if (genAtReady == clientGen) client?.listTools() }, 2500)
                client?.listSessionExtensions()
                // Finishing an assistant reset/create: only when THIS Ready is the reset's own
                // fresh session (its connection generation matches resetGen). Name it so both
                // the app (title match) and deliver.sh (name-grep) resolve it as the assistant
                // thread. The old-thread archive rename that used to run here is gone -- reset
                // clears in place now, so this path only ever fires when no thread existed.
                if (resetGen == clientGen) {
                    resetGen = -1
                    client?.renameSession(ev.sessionId, ASSISTANT_TITLE)
                    store.assistantSessionId = ev.sessionId
                }
                client?.listSessions()   // so the Assistant thread can be resolved by title
                if (replayActive.value) {
                    replayActive.value = false
                    // Swap the rebuilt transcript in only when it differs from what's shown.
                    // The common background-reconnect replay is byte-identical, and leaving
                    // `messages` untouched preserves the reading position by construction.
                    // Compare content fields, not ChatMessage itself: `id` is per-instance.
                    val same = replayBuffer.size == messages.size && replayBuffer.indices.all { i ->
                        val a = replayBuffer[i]; val b = messages[i]
                        a.role == b.role && a.text == b.text && a.detail == b.detail &&
                            a.status == b.status && a.output == b.output &&
                            a.images.size == b.images.size
                    }
                    if (!same) {
                        messages.clear(); messages.addAll(replayBuffer)
                        replayDoneTick.value++   // ChatScreen: new content -> snap to bottom
                    }
                    replayBuffer.clear()
                }
                // Prompts still waiting in the queue aren't in the server history (they haven't
                // been sent). Re-add their bubbles on top, in queue order, so the user's unsent
                // messages don't vanish from the screen. After the swap: a queued bubble was in
                // the shown list but never in the buffer, so `same` is false and the swap
                // dropped it.
                if (replayWiped) {
                    replayWiped = false
                    pendingSends.forEach { messages.add(ChatMessage("user", it.text, it.images)) }
                }
                // Send ONE queued prompt (bubbles were already added when queued); TurnDone drains
                // the rest. This used to `while`-loop the whole deque, firing every queued prompt
                // into the session at once -- which interleaves them in the transcript and
                // misattributes each reply, the exact failure the queue exists to prevent. It also
                // left turnInFlight false, so the next send() would fire a concurrent prompt too.
                dequeue()?.let { p ->
                    store.pendingPushSessionId = ev.sessionId
                    turnInFlight = true
                    busy.value = true
                    client?.sendPrompt(p.text, p.images, expect = currentSession.value)
                }
            }
            is AcpEvent.Projects -> projects.value = ev.list
            is AcpEvent.Schedules -> schedules.value = ev.list
            is AcpEvent.Recipes -> recipes.value = ev.list
            is AcpEvent.Skills -> skills.value = ev.list
            is AcpEvent.Sessions -> {
                sessions.value = ev.list
                store.rememberSessionCwds(ev.list.map { it.sessionId to it.cwd })
                // The fork-repoint block that used to live here is GONE (2026-08-01). It watched
                // for the cached assistant id turning up under a different title and hopped to
                // whichever session still bore ASSISTANT_TITLE -- necessary only because the
                // nightly rotation renamed the thread aside and minted a new one, so every client
                // had to notice and follow. deliver.sh now COMPACTS in place: the id never
                // changes, nothing is renamed, and there is nothing to repoint to. Keeping the
                // logic would mean keeping a recovery path for a failure that can no longer
                // happen, on a cache that is now stable by construction.
                //
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
            is AcpEvent.ServerConfig -> {
                when (ev.key) {
                    "GOOSE_CONTEXT_LIMIT" -> serverContextLimit.value = ev.value
                    "GOOSE_FAST_MODEL" -> serverFastModel.value = ev.value
                }
                serverConfig[ev.key] = ev.value   // generic mirror for settings UIs
            }
            is AcpEvent.Commands -> commands.value = ev.names
            is AcpEvent.Extensions -> { extensions.value = ev.list; extensionsBusy.value = false }
            is AcpEvent.SessionExtensions -> {
                sessionExtensionNames.value = ev.names
                sessionExtensionInfos.value = ev.infos
                // A re-listed name is attached again; its detached-row copy is stale.
                detachedPeerExts.value = detachedPeerExts.value.filterNot { it.name in ev.names.toSet() }
            }
            is AcpEvent.Tools -> {
                val g = group(ev.names)
                val target = discovering
                if (target != null) {
                    // Catalogue read: record the full set, then restore the session's real
                    // setting by round-tripping the SAME ExtInfo the discovery ran with (a
                    // peer DTO for federated sessions — never re-looked-up locally by name).
                    toolCatalog.value = toolCatalog.value + (catKey(target) to g[target.name].orEmpty())
                    discovering = null
                    val allowed = (target.raw["available_tools"] as? JsonArray)
                        ?.mapNotNull { it.jsonPrimitive.contentOrNull }?.toSet().orEmpty()
                    setSessionTools(target, if (allowed.isEmpty()) g[target.name].orEmpty().toSet() else allowed)
                } else sessionTools.value = g
            }
            is AcpEvent.MessageUsage -> {
                lastMessageUsage.value = ev
                // Stamp the reply this belongs to. Usage arrives at the end of a turn, so the
                // most recent assistant message is the one it describes.
                val idx = messages.indexOfLast { it.role == "assistant" }
                if (idx >= 0) messages[idx] = messages[idx].copy(usage = ev)
            }
            is AcpEvent.Permission -> {
                // Every conversation prompts, the Assistant thread included. It used to carry a
                // client-side "assistant actions" policy that could auto-approve or blanket-deny
                // on its behalf -- a second, invisible permission system layered on top of
                // goose's own mode, which the thread's mode picker then appeared to control and
                // did not. One mode, set in the chat like any other.
                permissions.add(ev)
                if (!appForeground) notifier.postApprovalNeeded(ev.title)
            }
        }
    }

    /** Merge a streamed chunk into the last bubble of the same role, else start a new one. */
    private fun appendStream(role: String, chunk: String) {
        val list = t()
        val last = list.lastOrNull()
        if (streamingRole == role && last != null && last.role == role) {
            list[list.lastIndex] = last.copy(text = last.text + chunk)
        } else {
            streamingRole = role
            list.add(ChatMessage(role, chunk))
        }
    }

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

        /** A session living on a roam peer arrives as `roam:<peer>:<remote id>` (the goose
         *  fork's ACP federation). Local ids never carry the prefix, so it doubles as the
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

/** Keep the cold-start transcript cache bounded: delete the oldest files beyond `keep`.
 *  Best-effort (a failed delete is not data loss) and a no-op on a missing/unreadable dir.
 *  Top-level internal so the JVM unit tests can exercise it. */
internal fun pruneTranscriptCache(dir: java.io.File, keep: Int = 20) {
    val files = dir.listFiles() ?: return          // missing/unreadable dir: no-op
    if (files.size <= keep) return
    files.sortedBy { it.lastModified() }.dropLast(keep).forEach { runCatching { it.delete() } }
}
