package id.gauvin.grouse

import kotlinx.serialization.json.*
import okhttp3.*
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

/**
 * Default container-side cwd for conversational sessions.
 *
 * NOT "/state", which it was until 2026-07-29. Goose Desktop has no project API and never asks
 * the server for one -- it derives its whole project list by grouping sessions on cwd and
 * labelling each group with the LAST PATH SEGMENT. So a cwd is a project name, and parking
 * everything in /state drew one project called "state" containing all 52 chats.
 *
 * Must EXIST inside the goose container or session/new rejects it; it is one of the
 * goose-projects mounts (host ~/services/goose-projects, mounted at /projects and under both
 * homedir shims). Safe to move sessions around because the assistant carries no
 * developer/shell extension, so its cwd is cosmetic.
 */
/** A selectable value inside a [ConfigOption] (goose "select" config). */
data class Choice(val value: String, val label: String)

/** One goose session config knob — provider / model / mode / thinking_effort. */
data class ConfigOption(
    val id: String,
    val name: String,
    val currentValue: String,
    val choices: List<Choice>,
)

/** An image to attach to a prompt (base64 payload + mime). */
data class ImageBlock(val mimeType: String, val dataB64: String)

/** One choice in a tool-approval request (allow_once / allow_always / reject_*). */
data class PermOption(val optionId: String, val label: String, val kind: String)

/** A resumable server-side goose session, from session/list. */
data class SessionInfo(
    val sessionId: String,
    val title: String,
    val updatedAt: String,
    val messageCount: Int,
    val model: String,
    /** One-line preview of the last message (server-side, opt-in via includeLastMessageSnippet). */
    val snippet: String = "",
    // Where the session's TOOLS run. No longer what a session belongs to -- see projectId.
    val cwd: String = "",
    /** True when this session was started from a recipe (session/list's `hasRecipe`).
     *
     *  It does not say WHICH recipe -- goose reports only the boolean -- so a client that cares
     *  matches the session title against the recipe list: a session started from a recipe is
     *  auto-titled with that recipe's title. That holds until someone renames the session, and
     *  the alternatives are worse: session/load per session is expensive AND rewrites
     *  working_dir, and a client-side record of "sessions I started" is per-device and blind to
     *  Desktop and the CLI. Reporting the recipe name in session/list is a two-line server
     *  change; it was deliberately not made, because a fork carries every change forever. */
    val hasRecipe: Boolean = false,
    /** The project this session is filed under, or null for unfiled.
     *
     *  goose has modelled projects as named sources with ids the whole time (sources.rs stores
     *  each as its own file; session/project/update links them); we just never read the field
     *  and grouped by cwd instead, which made a project and a directory the same thing. That
     *  coupling is why filing a chat also chose where its shell ran, why the same project
     *  opened from two machines drew two groups, and why a session/load carrying a stale cwd
     *  could silently re-file a conversation. An id has none of those properties. */
    val projectId: String? = null,
)

/** A goose project: a named source with a slug, NOT a directory. */
data class ProjectInfo(
    val id: String,
    val name: String,
    val description: String,
    val path: String = "",
    /** Directory this project's chats work in, from a `root: <path>` line in the project's own
     *  content. Blank for an ordinary project, whose chats do not care where tools run.
     *
     *  Stored on the SERVER rather than in app preferences so every client agrees, and because
     *  a path that only one phone knows about is a path the next client renders as nothing. */
    val root: String = "",
)

/** One scheduled job from `schedules/list`.
 *
 *  `source` is the recipe file the job runs. When the job was made with `recipes/schedule` that
 *  path is the LIBRARY file, so editing the recipe changes what runs; when it was made with
 *  `schedules/create` the scheduler wrote its own private copy and the library is irrelevant to
 *  it. Same DTO, and the only way to tell them apart is the path. */
data class ScheduleInfo(
    val id: String,
    val cron: String,
    val source: String,
    val paused: Boolean,
    val running: Boolean,
    val lastRun: String? = null,
    val currentSessionId: String? = null,
) {
    /** The recipe-library id this job runs, if it runs one. */
    val recipeFile: String get() = source.substringAfterLast('/')
}

/** A saved recipe, with the whole server DTO kept alongside the fields the UI shows.
 *
 *  KEEPING `raw` IS NOT OPTIONAL. Saving goes back through `recipes/save`, which replaces the
 *  entire recipe -- so an edit rebuilt from only the modelled fields would silently drop
 *  everything this class does not model: extension allowlists, sub-recipe wiring, response
 *  schemas, retry config. The UI edits a copy of `raw` and sends that. */
data class RecipeInfo(
    val id: String,
    val title: String,
    val description: String,
    val cron: String?,
    val provider: String?,
    val model: String?,
    val prompt: String?,
    val instructions: String?,
    val parameters: List<RecipeParam>,
    val subRecipes: List<String>,
    val extensions: List<String>,
    val filePath: String,
    val raw: JsonObject,
)

/** One skill from `sources/list {type: skill}` -- the same API that backs projects.
 *
 *  `content` is the whole SKILL.md, returned inline by the list call, so a skill can be read
 *  and edited without a second round trip. `writable` is false for the ones goose bundles:
 *  offering an edit that cannot be saved is worse than showing it read-only. */
data class SkillInfo(
    val name: String,
    val description: String,
    val content: String,
    val path: String,
    val global: Boolean,
    val writable: Boolean,
)

data class RecipeParam(
    val key: String,
    val requirement: String,
    val description: String,
    val default: String?,
)

/** One goose extension from the ACP `config/extensions/list` method (goose ≥1.42). */
data class ExtInfo(
    val name: String,
    val enabled: Boolean,
    val type: String,
    val description: String,
    val configKey: String,   // key in config.yaml; used by config/extensions/set-enabled
    // Core/required -- never offered for removal by a per-session extension profile (safety guard;
    // goose's own tagged-union extension config shape isn't hand-reconstructed anywhere else, so
    // this is parsed from the same server-sent object it guards).
    val bundled: Boolean = false,
    // The verbatim "extension" object as goose LISTED it. That is not the shape the add
    // methods accept -- a listed remote extension is `type: streamable_http`, which add rejects
    // outright -- so it goes through toExtensionDto on the way back in.
    val raw: JsonObject = JsonObject(emptyMap()),
    // True when this object came from a roam PEER's session/extensions/list. Session-scoped
    // tool operations on a federated session must only ever round-trip these: a local
    // catalog DTO of the same name can carry commands/paths that don't exist on the peer,
    // and pushing one would silently rewrite the peer's session extension.
    val fromPeer: Boolean = false,
)

/** Events surfaced from the ACP connection to the UI layer. */

/** Convert an extension as goose REPORTS it into the shape goose ACCEPTS.
 *
 *  These are not the same shape, and nothing says so. `config/extensions/list` and
 *  `session/extensions/list` hand back config.yaml's spelling -- `type: streamable_http` with a
 *  `uri` and a header MAP -- while the add methods take a tagged enum of builtin | platform |
 *  mcp, where a remote server is `{type: mcp, server: {type: http, url, headers: [{name, value}]}}`.
 *
 *  Feeding a listed extension straight back to add fails with -32602 "unknown variant
 *  `streamable_http`". That mattered because editing a tool allowlist is remove-then-add: the
 *  remove succeeded, the add was rejected, and the extension vanished from the session with no
 *  error surfaced anywhere -- which looks exactly like "my tool changes do nothing".
 *
 *  builtin and platform pass through: they are already variants the add method knows. */
internal fun toExtensionDto(raw: JsonObject): JsonObject {
    val type = raw["type"]?.jsonPrimitive?.contentOrNull ?: return raw
    if (type == "builtin" || type == "platform" || type == "mcp") return raw
    val name = raw["name"]?.jsonPrimitive?.contentOrNull ?: return raw

    val server = when (type) {
        "stdio" -> buildJsonObject {
            put("type", "stdio"); put("name", name)
            put("command", raw["cmd"]?.jsonPrimitive?.contentOrNull ?: "")
            put("args", (raw["args"] as? JsonArray) ?: JsonArray(emptyList()))
            put("env", JsonArray(emptyList()))
        }
        // sse and streamable_http are both HTTP transports to goose's client.
        else -> buildJsonObject {
            put("type", if (type == "sse") "sse" else "http"); put("name", name)
            put("url", raw["uri"]?.jsonPrimitive?.contentOrNull ?: "")
            put("headers", JsonArray(
                (raw["headers"] as? JsonObject).orEmpty().map { (k, v) ->
                    buildJsonObject {
                        put("name", k); put("value", v.jsonPrimitive.contentOrNull ?: "")
                    }
                }))
        }
    }
    return buildJsonObject {
        put("type", "mcp")
        put("server", server)
        raw["timeout"]?.let { put("timeout", it) }
        raw["description"]?.let { put("description", it) }
        raw["env_keys"]?.let { put("envKeys", it) }
        raw["available_tools"]?.let { put("available_tools", it) }
    }
}

private const val SKILLS_TAG = "_goose/unstable/sources/list#skill"

/** session/update tags that mutate the on-screen transcript. Any other tag carries its own
 *  session id in the event and is wanted no matter which session it names. */
private val TRANSCRIPT_TAGS = setOf(
    "user_message_chunk", "agent_message_chunk", "agent_thought_chunk",
    "tool_call", "tool_call_update", "usage_update",
)

sealed interface AcpEvent {
    data class Status(val text: String) : AcpEvent
    /** messageId is set on REPLAYED chunks (goose stamps _meta.goose.messageId per source
     *  message) and null on live streaming deltas — it is the message-boundary signal that
     *  keeps consecutive same-role history messages from merging into one bubble. */
    data class AgentChunk(val text: String, val messageId: String? = null) : AcpEvent
    data class ThoughtChunk(val text: String) : AcpEvent
    data class UserChunk(val text: String, val messageId: String? = null) : AcpEvent
    /** `detail` is the tool's rawInput (command/args), same extraction the permission sheet already
     *  does — Desktop shows this; Grouse was discarding it and only showing `title`.
     *  `toolCallId` correlates later ToolCallUpdate events (status + output) to this call. */
    data class ToolCall(
        val title: String, val detail: String = "", val toolCallId: String = "",
        // MCP-App fields, set when the tool_call carried _meta.goose.mcpApp: the server hosts
        // an HTML template for this tool's output (autovisualiser's chart/sankey/radar/donut/
        // treemap/chord/map/mermaid all work this way). appKey is "$ext|$uri" — the cache key.
        // appInput is the tool's rawInput as JSON; the template extracts what it needs.
        val appKey: String = "", val appUri: String = "", val appExt: String = "",
        val appInput: String = "",
    ) : AcpEvent
    /** Reply to a _goose/unstable/resources/read issued for an MCP-App template. */
    data class AppResource(val key: String, val html: String) : AcpEvent
    /** Progress for an in-flight tool call: status is in_progress/completed/failed; `output`
     *  carries the tool's result text when the update includes content (usually on completion). */
    /** `live=true` marks a streaming chunk (shell live_output): APPEND it to the tool's
     *  output rather than replacing, and expect many per call. */
    data class ToolCallUpdate(
        val toolCallId: String, val status: String, val output: String,
        val live: Boolean = false,
    ) : AcpEvent
    /** The session's active run started (runId set) or ended (null). Steering needs the id. */
    data class ActiveRun(val sessionId: String, val runId: String?) : AcpEvent
    /** probeSession reply. messageCount < 0 means the probe itself FAILED (dead socket or
     *  vanished session) — reconnect, don't compare. */
    data class Probe(val sessionId: String, val updatedAt: String?, val messageCount: Int) : AcpEvent
    /** session/export reply: the session serialized for backup/sharing. */
    data class SessionExport(val data: String) : AcpEvent
    /** A session's title/updatedAt changed server-side (auto-naming, a rename from any client). */
    data class SessionInfoChanged(val sessionId: String, val title: String?, val updatedAt: String?) : AcpEvent
    /** The session's approval mode changed (e.g. from another client). */
    data class ModeChanged(val modeId: String) : AcpEvent
    /** Per-message generation stats (goose-custom `_goose/unstable/session/update`, sessionUpdate
     *  "message_usage") — tok/s derived client-side from outputTokens/elapsedMs. Separate from the
     *  aggregate [Usage] (context window used/size), which comes from the STANDARD ACP usage_update. */
    data class MessageUsage(
        val outputTokens: Int, val elapsedMs: Long, val ttftMs: Long, val cost: Double?,
    ) : AcpEvent
    data class TurnDone(val stopReason: String) : AcpEvent
    /** A session/load history replay is about to stream. The server transcript is ground truth --
     *  it may hold turns other clients (Desktop, deliver.sh) added while this app wasn't looking --
     *  so the UI drops its local copy and rebuilds from the replayed chunks. */
    object ReplayStart : AcpEvent
    /** `background=true` means the failed call was NOT part of the visible turn (a sidebar
     *  refresh, a config read). ConnectionManager must not put those in the transcript or
     *  touch busy/turn state — a failed schedules/list once un-busied a live turn and left
     *  an error bubble in an unrelated open chat. */
    data class Error(val text: String, val background: Boolean = false) : AcpEvent
    data class Config(val options: List<ConfigOption>) : AcpEvent
    data class Ready(val sessionId: String) : AcpEvent
    data class Sessions(val list: List<SessionInfo>) : AcpEvent
    /** Reply to listProjects: the goose projects a session can be filed under. */
    data class Projects(val list: List<ProjectInfo>) : AcpEvent
    data class Schedules(val list: List<ScheduleInfo>) : AcpEvent
    data class Recipes(val list: List<RecipeInfo>) : AcpEvent
    data class Skills(val list: List<SkillInfo>) : AcpEvent
    data class Extensions(val list: List<ExtInfo>) : AcpEvent
    /** Names of a SPECIFIC session's currently-enabled extensions (session-scoped, not the global
     *  catalog) -- reply to listSessionExtensions, used to diff-and-apply an extension profile. */
    data class SessionExtensions(
        val sessionId: String,
        val names: List<String>,
        // Full extension objects, needed for federated sessions where the peer's own DTOs
        // are the only sound thing to write back (see ExtInfo.fromPeer).
        val infos: List<ExtInfo> = emptyList(),
    ) : AcpEvent
    /** Tools active in a session, as `extension__tool` names straight from goose.
     *  sessionId is null for the client's own session (legacy), set for targeted queries. */
    data class Tools(val names: List<String>, val sessionId: String? = null) : AcpEvent
    data class Commands(val names: List<String>) : AcpEvent
    data class Usage(val used: Int, val size: Int, val cost: Double, val currency: String) : AcpEvent
    data class Chart(val spec: String) : AcpEvent   // Chart.js-shaped JSON from autovisualiser
    /** A goose-custom status line (currently: compaction progress/notice text). Substring-matched
     *  by the consumer — see ConnectionManager.onEvent. */
    data class CompactionStatus(val message: String) : AcpEvent
    /** Reply to a config read: the requested key and its string value (empty if unset). */
    data class ServerConfig(val key: String, val value: String) : AcpEvent
    /** Live model list for one provider (reply to listSupportedModels) -- for an OpenAI-compatible
     *  backend like LocalAI this hits its /v1/models endpoint server-side, so it reflects whatever
     *  models are actually loadable right now, not just the goose-bundled "featured" set. */
    data class SupportedModels(val providerId: String, val models: List<String>) : AcpEvent
    data class Permission(
        val toolCallId: String, val title: String, val detail: String, val options: List<PermOption>,
    ) : AcpEvent
    /** Reply to a DIRECT tool invocation (_goose/unstable/tools/call — no model turn involved).
     *  `text` is the concatenated text content blocks. */
    data class DirectToolResult(val text: String, val isError: Boolean) : AcpEvent
    /** One field of a form elicitation. `type` is string/number/integer/boolean; a non-empty
     *  `options` list means single-select (rendered as choices instead of free text). */
    data class ElicitField(
        val name: String, val type: String, val title: String, val description: String,
        val options: List<Choice>, val required: Boolean,
    )
    /** A tool/extension is requesting structured input (MCP elicitation, ACP form mode).
     *  Answer with respondElicitation(requestKey, ...): accept with values, decline, or cancel.
     *  Previously these got the generic empty-result reply, silently no-op'ing any tool that
     *  asked — now they render as a real form. */
    data class Elicitation(
        val requestKey: String, val message: String, val title: String,
        val fields: List<ElicitField>,
    ) : AcpEvent
}

/**
 * Thin ACP (Agent Client Protocol) client. By default over a WebSocket to
 * `ws(s)://host:port/acp`; when `roam` is set, frames ride a pre-connected
 * iroh byte stream instead (same message layer, newline framing — see
 * RoamFrameCodec), driven by a reader thread.
 * NOTE: onEvent is invoked on OkHttp's WS thread (or the roam reader thread) —
 * the caller must marshal to main.
 */
class AcpClient(
    private val url: String,          // ws://host:port/acp
    private val secretKey: String,
    // Roam mode: when non-null, connect() drives this byte-stream link instead
    // of opening a WebSocket. No URL/secret-key handshake — the dial already
    // happened (roamConnect); the stream is authenticated.
    private val roam: RoamLink? = null,
    private val onEvent: (AcpEvent) -> Unit,
) {
    private val http = Net.builder()      // trust-all TLS for goosed's self-signed cert (wss)
        .pingInterval(20, TimeUnit.SECONDS)
        .readTimeout(0, TimeUnit.MILLISECONDS)
        .build()
    private var ws: WebSocket? = null
    private var readerThread: Thread? = null
    @Volatile private var closed = false
    private val nextId = AtomicInteger(1)
    // Touched from both the main thread (outbound rpc) and the OkHttp WS thread (responses).
    private val pending = ConcurrentHashMap<Int, String>()      // request id -> method we sent
    private val pendingConfigKeys = ConcurrentHashMap<Int, String>()  // config/read id -> key
    // Explicit-target session/extensions/list + tools/list requests (replies don't echo the session).
    private val pendingExtListSids = ConcurrentHashMap<Int, String>()
    private val pendingToolListSids = ConcurrentHashMap<Int, String>()
    private var sessionId: String? = null
    private val json = Json { ignoreUnknownKeys = true; encodeDefaults = true }
    // Outstanding tool-approval requests: toolCallId -> the JSON-RPC id we must answer.
    // Inserted on the WS thread, removed on the main thread — must be concurrent.
    private val pendingPermissions = ConcurrentHashMap<String, JsonElement>()
    // Outstanding elicitation requests: requestKey -> the JSON-RPC id to answer.
    private val pendingElicitations = ConcurrentHashMap<String, JsonElement>()
    // Recipe parameter requests share the elicitation FORM UI but not the response shape;
    // ownership of a requestKey decides which reply respondElicitation() builds.
    private val pendingRecipeParams = ConcurrentHashMap<String, JsonElement>()
    private val elicitSeq = AtomicInteger(1)

    /** Config values to re-apply once a session opens (persisted picks). id -> value. */
    var desiredOptions: Map<String, String> = emptyMap()
    // provider before model so the model list is valid when the model set lands.
    private val applyOrder = listOf("provider", "model", "mode", "thinking_effort")

    /** If set, resume this server-side session (session/load) instead of a fresh session/new. */
    var resumeSessionId: String? = null
    /** The cwd to resume `resumeSessionId` with (session/load). Must match the session's actual
     *  working_dir -- session/load's cwd param silently REWRITES working_dir if it differs, so
     *  passing the wrong value here would un-scope a Code session back to whatever's passed. */
    var resumeCwd: String = ""
    /** False when the caller could NOT determine the session's real cwd: the client then asks
     *  the server (_goose/unstable/session/info) before session/load, instead of guessing --
     *  a wrong guess is a silent working_dir rewrite (this re-homed the assistant thread once). */
    var resumeCwdKnown: Boolean = true
    private var loadAwaitingInfo = false
    /** The cwd for a brand-new session (session/new) when resumeSessionId is null. goose
     *  validates this is ABSOLUTE and rejects anything else, so there is no sane built-in
     *  default -- the caller supplies the user's configured working directory. */
    var desiredCwd: String = ""

    /** Library id of a recipe to start the new session FROM. goose loads the recipe, applies its
     *  extensions/settings/instructions, and titles the session after it -- which is also how a
     *  client can later tell what the session is for. Null starts a plain chat. */
    var desiredRecipeId: String? = null
    // True between sending session/load and its response (i.e. while history replays).
    private var replaying = false

    fun connect() {
        if (roam != null) {
            // The stream is already connected and authenticated: send initialize
            // immediately, then pump newline-delimited frames from the reader.
            closed = false
            onEvent(AcpEvent.Status("connected — initializing"))
            sendInitialize()
            readerThread = Thread({
                val codec = RoamFrameCodec()
                while (!closed) {
                    val bytes = roam.read(16_384)
                    if (bytes.isEmpty() || closed) break
                    codec.feed(bytes, bytes.size).forEach { handle(it) }
                }
                if (!closed) onEvent(AcpEvent.Status("disconnected"))
            }, "grouse-roam-reader").apply { isDaemon = true; start() }
            return
        }
        val req = Request.Builder().url(url).addHeader("X-Secret-Key", secretKey).build()
        ws = http.newWebSocket(req, listener)
    }

    fun close() {
        closed = true
        if (roam != null) {
            // Closing the stream drops the native handle, which unblocks the
            // reader's pending read (channel close -> error -> EOF).
            roam.close()
            readerThread?.interrupt()
        } else {
            ws?.close(1000, "bye"); ws = null
        }
    }

    /** Send one frame over whichever transport is live. */
    private fun sendFrame(text: String): Boolean =
        if (roam != null) roam.send(text) else ws?.send(text) ?: false

    /** Roam mode with no resume target: don't auto-create a session on the peer
     *  (session/new would litter the host's session list). The user picks one
     *  from the drawer, which reconnects with resume=<that session>. */
    var autoNewSession: Boolean = true

    /** Ask the agent for its resumable sessions; reply arrives as AcpEvent.Sessions.
     *  `_meta.types` (goose ≥1.42) filters out scheduled sessions server-side; on older builds
     *  it's ignored and parseSessions() drops them client-side by title. */
    fun listSessions() = rpc("session/list", buildJsonObject {
        putJsonObject("_meta") {
            putJsonArray("types") { add("user"); add("acp") }
            // Opt-in: each entry's _meta then carries lastMessageSnippet for drawer previews.
            putJsonObject("goose") { put("includeLastMessageSnippet", true) }
        }
    })

    /** List goose projects. Reply arrives as [AcpEvent.Projects]. */
    fun listProjects() = rpc("_goose/unstable/sources/list", buildJsonObject {
        put("type", "project")
    })

    /** Create a project. Global scope: a project is not scoped to a directory -- that is the
     *  entire point of the model. */
    fun createProject(name: String, description: String = "", root: String = "") =
        rpc("_goose/unstable/sources/create", buildJsonObject {
            put("type", "project")
            put("name", name)
            put("description", description)
            // A rooted project records its directory here; ProjectInfo.root reads it back.
            put("content", if (root.isBlank()) "" else "root: ${root.trimEnd('/')}\n")
            putJsonObject("target") { put("scope", "global") }
        })

    /** Delete a project. Identified by its source PATH, not its slug -- sources/delete takes the
     *  path SourceEntry handed back. Sessions filed under it are unaffected server-side; the
     *  caller decides whether to archive or unfile them. */
    fun deleteProject(path: String) =
        rpc("_goose/unstable/sources/delete", buildJsonObject {
            put("type", "project")   // required alongside path; omitting it is a bare -32602
            put("path", path)
        })

    /** File a session under a project, or pass null to unfile it. Changes ONE field and touches
     *  nothing on disk; the directory-based equivalent rewrote working_dir, which also moved
     *  where the session's tools ran. */
    fun assignSessionProject(sessionId: String, projectId: String?) =
        rpc("_goose/unstable/session/project/update", buildJsonObject {
            put("sessionId", sessionId)
            if (projectId == null) put("projectId", JsonNull) else put("projectId", projectId)
        })

    // ---- Schedules and recipes -------------------------------------------------------------
    //
    // Two APIs that only look like one. `recipes/*` is a LIBRARY of saved recipes on the server;
    // `schedules/*` is the cron table. `recipes/schedule` is the join: it creates a job pointing
    // at the library file, so a later edit to the recipe changes what the job runs. Creating a
    // job the other way (`schedules/create`, which takes a recipe inline) copies the recipe into
    // the scheduler's own directory, and from then on the library copy is decoration.
    //
    // BEWARE THE CASING. These params are snake_case (`cron_schedule`), unlike almost every
    // other goose ACP method, which is camelCase. An unknown field is dropped silently rather
    // than rejected, and for recipes/schedule a dropped `cron_schedule` reads as "no cron",
    // which UNSCHEDULES the recipe. It returns ok either way.

    /** List scheduled jobs. Reply arrives as [AcpEvent.Schedules]. */
    fun listSchedules() = rpc("_goose/unstable/schedules/list", buildJsonObject {})

    /** List saved recipes. Reply arrives as [AcpEvent.Recipes]. */
    fun listRecipes() = rpc("_goose/unstable/recipes/list", buildJsonObject {})

    /** List skills. Reply arrives as [AcpEvent.Skills]. Recipes are NOT listable this way --
     *  `sources/list` rejects type "recipe" outright; they have their own API above. */
    fun listSkills() = rpc("_goose/unstable/sources/list",
        buildJsonObject { put("type", "skill") }, tag = SKILLS_TAG)

    /** True once session/new or session/load has completed on this connection — the only
     *  state in which session/prompt can succeed. Callers should QUEUE, not send, before it. */
    val ready: Boolean get() = sessionId != null

    /** Cheap liveness + freshness check: one session/info round trip. Doubles as a dead-socket
     *  detector (no reply in a couple of seconds = the socket died while frozen and OkHttp
     *  hasn't noticed) and a change detector (updatedAt/messageCount moved = another client
     *  touched the session and a replay is worth its cost). Reply: AcpEvent.Probe. */
    fun probeSession(sid: String) =
        rpc("_goose/unstable/session/info", buildJsonObject { put("sessionId", sid) },
            tag = "probe|$sid")

    /** Inject a user message into the RUNNING turn (vs queueing until it ends). Needs the
     *  runId from AcpEvent.ActiveRun; the server double-checks it so a just-ended run fails
     *  cleanly rather than starting a stray turn. Text-only by design — steering mid-turn
     *  with images has no sane rendering anyway. */
    fun steer(text: String, runId: String) {
        val sid = sessionId ?: return
        rpc("_goose/unstable/session/steer", buildJsonObject {
            put("sessionId", sid)
            putJsonArray("prompt") { addJsonObject { put("type", "text"); put("text", text) } }
            put("expectedRunId", runId)
        })
    }

    /** Serialize a session for backup/sharing; reply arrives as AcpEvent.SessionExport. */
    fun exportSession(sid: String) =
        rpc("_goose/unstable/session/export", buildJsonObject { put("sessionId", sid) })

    /** Fetch an MCP-App HTML template (ui://... resource) for a tool that declared one. */
    fun readAppResource(key: String, uri: String, extensionName: String) {
        val sid = sessionId ?: return
        rpc("_goose/unstable/resources/read", buildJsonObject {
            put("sessionId", sid); put("uri", uri); put("extensionName", extensionName)
        }, tag = "appres|$key")
    }

    /** Rewrite a skill. `path` identifies it, exactly as with projects. */
    fun updateSkill(path: String, name: String, description: String, content: String) =
        rpc("_goose/unstable/sources/update", buildJsonObject {
            put("type", "skill"); put("path", path)
            put("name", name); put("description", description); put("content", content)
        })

    fun deleteSkill(path: String) =
        // The #skill tag exists because projects and skills share this method: the untagged
        // reply branch refreshes PROJECTS, so an untagged skill delete left stale skill rows
        // until some unrelated refresh. Same mechanism as sources/list#skill.
        rpc("_goose/unstable/sources/delete", buildJsonObject {
            put("type", "skill"); put("path", path)
        }, tag = "_goose/unstable/sources/delete#skill")

    fun pauseSchedule(id: String, paused: Boolean) =
        rpc("_goose/unstable/schedules/" + (if (paused) "pause" else "unpause"),
            buildJsonObject { put("scheduleId", id) })

    /** Run a schedule immediately. This BLOCKS server-side for the whole run -- a briefing is a
     *  couple of minutes -- so the reply is the finish, not the start. The UI must not wait on
     *  it; watch the schedule's running flag instead. */
    fun runScheduleNow(id: String) =
        rpc("_goose/unstable/schedules/run-now", buildJsonObject { put("scheduleId", id) })

    fun deleteSchedule(id: String) =
        rpc("_goose/unstable/schedules/delete", buildJsonObject { put("scheduleId", id) })

    /** Change a job's cron. Recipe content is NOT touched -- `schedules/update` takes a cron and
     *  nothing else. Edit the recipe through [saveRecipe]. */
    fun updateScheduleCron(id: String, cron: String) =
        rpc("_goose/unstable/schedules/update", buildJsonObject {
            put("scheduleId", id); put("cron", cron)
        })

    /** Schedule a library recipe, or pass null to unschedule it. */
    fun scheduleRecipe(recipeId: String, cron: String?) =
        rpc("_goose/unstable/recipes/schedule", buildJsonObject {
            put("id", recipeId)
            if (cron == null) put("cron_schedule", JsonNull) else put("cron_schedule", cron)
        })

    /** Overwrite a saved recipe. `dto` must be a COMPLETE recipe -- see [RecipeInfo.raw]. */
    fun saveRecipe(recipeId: String, dto: JsonObject) =
        rpc("_goose/unstable/recipes/save", buildJsonObject {
            put("id", recipeId); put("recipe", dto)
        })

    fun deleteRecipe(recipeId: String) =
        rpc("_goose/unstable/recipes/delete", buildJsonObject { put("id", recipeId) })

    /** List configured extensions (agent-global). Reply arrives as AcpEvent.Extensions.
     *  Replaces goosed's old GET /config/extensions — that REST endpoint is gone from
     *  `goose serve`; extension config is now an ACP method over this same socket. */
    fun listExtensions() = rpc("_goose/unstable/config/extensions/list", buildJsonObject {})

    /** Enable/disable a configured extension (affects new sessions); refreshes the list on reply. */
    fun setExtensionEnabled(configKey: String, enabled: Boolean) =
        rpc("_goose/unstable/config/extensions/set-enabled", buildJsonObject {
            put("configKey", configKey); put("enabled", enabled)
        })

    // --- Session-scoped extensions (a DIFFERENT, session-local API from config/extensions/* above:
    // that one writes config.yaml and affects every session; this one mutates only ONE session's own
    // extension_data, so a per-chat override can't leak into other sessions). ---

    /** List the CURRENT session's enabled extensions. Reply arrives as AcpEvent.SessionExtensions. */
    fun listSessionExtensions() {
        val sid = sessionId ?: return
        rpc("_goose/unstable/session/extensions/list", buildJsonObject { put("sessionId", sid) })
    }

    /** Enable one extension for just the current session. `extension` is an ExtInfo.raw as
     *  goose listed it; toExtensionDto translates it into the shape add accepts, because those
     *  two shapes are not the same and the mismatch fails silently. */
    fun addSessionExtension(extension: JsonObject) {
        val sid = sessionId ?: return
        rpc("_goose/unstable/session/extensions/add", buildJsonObject {
            put("sessionId", sid); put("extension", toExtensionDto(extension))
        })
    }

    fun removeSessionExtension(name: String) {
        val sid = sessionId ?: return
        rpc("_goose/unstable/session/extensions/remove", buildJsonObject {
            put("sessionId", sid); put("name", name)
        })
    }

    // --- Explicit-target variants: operate on ANY session (the Assistant thread's tool
    // profile is edited from Settings without that session being the one on screen). ---
    fun listSessionExtensionsFor(target: String) {
        rpc("_goose/unstable/session/extensions/list",
            buildJsonObject { put("sessionId", target) },
            register = { pendingExtListSids[it] = target })
    }
    fun listToolsFor(target: String) {
        rpc("_goose/unstable/tools/list", buildJsonObject { put("sessionId", target) },
            register = { pendingToolListSids[it] = target })
    }
    fun addSessionExtensionFor(target: String, extension: JsonObject) =
        rpc("_goose/unstable/session/extensions/add", buildJsonObject {
            put("sessionId", target); put("extension", toExtensionDto(extension))
        })
    fun removeSessionExtensionFor(target: String, name: String) =
        rpc("_goose/unstable/session/extensions/remove", buildJsonObject {
            put("sessionId", target); put("name", name)
        })

    /** Tools currently active in this session. Names are `extension__tool`; goose only returns
     *  ALLOWED tools, so this reflects `available_tools` filtering rather than the full catalogue
     *  (see ConnectionManager.discoverTools for how the full set is obtained). */
    fun listTools() {
        val sid = sessionId ?: return
        rpc("_goose/unstable/tools/list", buildJsonObject { put("sessionId", sid) })
    }

    /** Upsert an extension in config.yaml (the GLOBAL default for new sessions). Sending the
     *  extension object back with a modified `available_tools` is how a tool allowlist is saved.
     *  NOTE this rewrites the whole config.yaml and drops its comments -- goose re-serialises from
     *  its parsed model. Same is true of setExtensionEnabled. */
    fun addExtensionConfig(extension: JsonObject, enabled: Boolean) =
        rpc("_goose/unstable/config/extensions/add", buildJsonObject {
            put("extension", toExtensionDto(extension)); put("enabled", enabled)
        })

    /** Read a global goose config value (e.g. GOOSE_FAST_MODEL). Reply arrives as
     *  AcpEvent.ServerConfig. These live in goose's config.yaml, NOT per-session — so the
     *  value only takes effect for NEW sessions/tasks, and an env var of the same name in the
     *  container would override it (we moved GOOSE_FAST_MODEL out of .env.goose for this). */
    fun readConfig(key: String): Int {
        // Record the key -> id mapping BEFORE the frame goes out: the reply doesn't echo the key,
        // and on a LAN/localhost goosed the response can land before a put-after-send would run,
        // dropping the reply. Mirror rpc()'s id/pending bookkeeping so ordering is guaranteed.
        val id = nextId.getAndIncrement()
        pending[id] = "_goose/unstable/config/read"
        pendingConfigKeys[id] = key
        ws?.send(buildJsonObject {
            put("jsonrpc", "2.0"); put("id", id); put("method", "_goose/unstable/config/read")
            putJsonObject("params") { put("key", key) }
        }.toString())
        return id
    }

    /** Upsert a global goose config value; server replies empty, so we re-read to confirm. */
    fun upsertConfig(key: String, value: String) =
        rpc("_goose/unstable/config/upsert", buildJsonObject {
            put("key", key); put("value", value)
        })

    /** Ask a provider for its LIVE model list (reply arrives as AcpEvent.SupportedModels). For the
     *  "openai" provider goose calls fetch_supported_models(), which hits the configured backend's
     *  /v1/models -- e.g. LocalAI -- so this surfaces every model actually loadable right now, not
     *  just goose's bundled "featured" names or what the app happens to remember from past typing. */
    fun listSupportedModels(providerId: String) =
        rpc("_goose/unstable/providers/supported-models/list", buildJsonObject {
            put("providerId", providerId)
        })

    /** Rename a session (sets its title). Used by the assistant-thread reset: the old thread is
     *  renamed aside and a fresh one is renamed to "goose-assistant". Server replies empty. */
    fun renameSession(targetSessionId: String, title: String) =
        rpc("_goose/unstable/session/rename", buildJsonObject {
            put("sessionId", targetSessionId); put("title", title)
        })

    /** Archive a session: leaves session/list, history stays on disk (reversible via
     *  unarchiveSession). The soft option next to deleteSession. */
    fun archiveSession(targetSessionId: String) =
        rpc("_goose/unstable/session/archive", buildJsonObject { put("sessionId", targetSessionId) })

    /** Delete a session outright (goose ≥1.44 has real session/delete; the old "-32601 Method
     *  not found" note predates it). Archive remains the soft option. Reply re-lists. */
    fun deleteSession(targetSessionId: String) =
        rpc("session/delete", buildJsonObject { put("sessionId", targetSessionId) })

    /** Bring an archived session back into session/list. No UI browses archived sessions yet,
     *  but the capability is wired for parity with delete. */
    fun unarchiveSession(targetSessionId: String) =
        rpc("_goose/unstable/session/unarchive", buildJsonObject { put("sessionId", targetSessionId) })

    /** Rewrite a session's working_dir server-side -- the sanctioned form of the rewrite that
     *  session/load does silently. Used to move a chat into/out of a project and to repair
     *  sessions stranded by a renamed project directory. */
    fun updateWorkingDir(targetSessionId: String, workingDir: String) =
        rpc("_goose/unstable/session/working-dir/update", buildJsonObject {
            put("sessionId", targetSessionId); put("workingDir", workingDir)
        })

    /** Invoke a tool DIRECTLY -- no model turn, no prompt, deterministic. Name is the
     *  `extension__tool` form (e.g. "developer__shell"). Reply arrives as DirectToolResult.
     *  A session that only ever does this has zero messages, so session/list (which filters
     *  only_sessions_with_messages) never shows it -- the invisible-utility-session property
     *  the bootstrap flows rely on. */
    fun callTool(targetSessionId: String, name: String, arguments: JsonObject) =
        rpc("_goose/unstable/tools/call", buildJsonObject {
            put("sessionId", targetSessionId); put("name", name); put("arguments", arguments)
        })

    /** Change a session config knob; server replies with the refreshed configOptions. */
    fun setConfigOption(configId: String, value: String) {
        val sid = sessionId ?: return
        rpc("session/set_config_option", buildJsonObject {
            put("sessionId", sid); put("configId", configId); put("value", value)
        })
    }

    /** @param expect the session the UI believes it is in. sendPrompt used to trust only this
     *  client's own `sessionId`, which is set from whatever session/new or session/load last
     *  returned -- nothing tied it to the conversation on screen. When the two diverged the prompt
     *  went silently to the wrong chat. A mismatch is now refused and surfaced instead. */
    fun sendPrompt(text: String, images: List<ImageBlock> = emptyList(), expect: String? = null) {
        val sid = sessionId
        if (sid == null) { onEvent(AcpEvent.Error("not ready — no session")); return }
        if (expect != null && expect != sid) {
            onEvent(AcpEvent.Error("not sent — this chat isn't loaded yet (showing $expect, socket on $sid). Try again."))
            return
        }
        rpc("session/prompt", buildJsonObject {
            put("sessionId", sid)
            putJsonArray("prompt") {
                if (text.isNotBlank()) add(buildJsonObject { put("type", "text"); put("text", text) })
                images.forEach { img ->
                    add(buildJsonObject {
                        put("type", "image"); put("mimeType", img.mimeType); put("data", img.dataB64)
                    })
                }
            }
        })
    }

    /** Interrupt the running turn (ACP notification — no response expected). */
    fun cancel() {
        val sid = sessionId ?: return
        ws?.send(buildJsonObject {
            put("jsonrpc", "2.0"); put("method", "session/cancel")
            putJsonObject("params") { put("sessionId", sid) }
        }.toString())
    }

    /** Answer a pending elicitation. accept=true sends `values`; accept=false declines;
     *  values ignored when declining. Cancel (sheet dismissed) is decline=false+cancel. */
    fun respondElicitation(requestKey: String, values: Map<String, JsonPrimitive>?, cancelled: Boolean = false) {
        // Recipe-params answers ride the same UI but a different wire shape:
        // {action:"submit"|"cancel", values:{key:string}} — and values must be STRINGS
        // (RecipeParamsResponse is HashMap<String,String> server-side).
        pendingRecipeParams.remove(requestKey)?.let { id ->
            respond(id, buildJsonObject {
                if (cancelled || values == null) put("action", "cancel")
                else {
                    put("action", "submit")
                    putJsonObject("values") { values.forEach { (k, v) -> put(k, v.content) } }
                }
            })
            return
        }
        val id = pendingElicitations.remove(requestKey) ?: return
        respond(id, buildJsonObject {
            when {
                cancelled -> put("action", "cancel")
                values == null -> put("action", "decline")
                else -> {
                    put("action", "accept")
                    putJsonObject("content") { values.forEach { (k, v) -> put(k, v) } }
                }
            }
        })
    }

    /** Answer a pending tool-approval request; null optionId = cancelled/deny. */
    fun respondPermission(toolCallId: String, optionId: String?) {
        val id = pendingPermissions.remove(toolCallId) ?: return
        respond(id, buildJsonObject {
            putJsonObject("outcome") {
                if (optionId != null) { put("outcome", "selected"); put("optionId", optionId) }
                else put("outcome", "cancelled")
            }
        })
    }

    /** @param tag what the reply dispatcher matches on. Defaults to the method, and differs
     *  only where one method serves two features -- `sources/list` backs both projects and
     *  skills, and the reply carries nothing that says which was asked for. */
    private fun rpc(
        method: String, params: JsonObject, tag: String = method,
        // Correlation state that must exist BEFORE the frame is on the wire. A localhost
        // reply can beat any bookkeeping done after send() returns — readConfig was
        // rewritten for exactly this race; this hook makes the fix available to every call.
        register: ((Int) -> Unit)? = null,
    ): Int {
        if (roam == null && ws == null) return -1   // no transport: record nothing, so nothing leaks
        val id = nextId.getAndIncrement()
        pending[id] = tag
        register?.invoke(id)
        sendFrame(buildJsonObject {
            put("jsonrpc", "2.0"); put("id", id); put("method", method); put("params", params)
        }.toString())
        return id
    }

    private fun respond(id: JsonElement, result: JsonObject) {
        sendFrame(buildJsonObject {
            put("jsonrpc", "2.0"); put("id", id); put("result", result)
        }.toString())
    }

    /** JSON-RPC error reply. An unknown server request must get -32601, not an empty
     *  result — `{}` is a structurally invalid response for every typed request (e.g.
     *  fs/read_text_file expects `content`) and makes the failure the server's to debug. */
    private fun respondError(id: JsonElement, code: Int, message: String) {
        sendFrame(buildJsonObject {
            put("jsonrpc", "2.0"); put("id", id)
            putJsonObject("error") { put("code", code); put("message", message) }
        }.toString())
    }

    private fun sendInitialize() {
        rpc("initialize", buildJsonObject {
            put("protocolVersion", 1)
            putJsonObject("clientCapabilities") {
                putJsonObject("fs") { put("readTextFile", false); put("writeTextFile", false) }
                // Form elicitation: tools can request structured input and we render a real
                // form (see AcpEvent.Elicitation). Without this goose cancels elicitations
                // server-side ("client does not support form elicitation").
                putJsonObject("elicitation") { putJsonObject("form") {} }
                // Opt into goose's custom notifications (currently: compaction status lines).
                // Purely additive — the standard usage_update still always fires regardless,
                // so this can't regress anything already working.
                putJsonObject("_meta") {
                    putJsonObject("goose") {
                        put("customNotifications", true)
                        // Without this, session/new HARD-FAILS for any recipe that
                        // declares parameters ("recipe requires parameters but the
                        // client does not support recipeParameterRequests") — the
                        // server refuses rather than degrades. The request arrives as
                        // _goose/unstable/session/recipe/request-params and is rendered
                        // through the same form UI as elicitations.
                        put("recipeParameterRequests", true)
                        // Free label quality: the server enriches tool_call titles for
                        // clients that declare this (goose 1.45 feature). Read path is
                        // unchanged — the enrichment arrives in the same `title` field.
                        put("toolCallLabelEnrichment", true)
                    }
                }
            }
        })
    }

    private val listener = object : WebSocketListener() {
        override fun onOpen(webSocket: WebSocket, response: Response) {
            onEvent(AcpEvent.Status("connected — initializing"))
            sendInitialize()
        }
        override fun onMessage(webSocket: WebSocket, text: String) = handle(text)
        override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
            // Transport drop (usually just backgrounding) — surface on the status line, not as a
            // chat error bubble. ConnectionManager reconnects on resume.
            onEvent(AcpEvent.Status("disconnected"))
        }
        override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
            onEvent(AcpEvent.Status("disconnected"))
        }
    }

    internal fun handle(text: String) {
        val obj = try { json.parseToJsonElement(text).jsonObject } catch (e: Exception) {
            onEvent(AcpEvent.Error("bad json: ${e.message}")); return
        }
        // Dispatch runs on the OkHttp WS thread; a field of an unexpected JSON kind (e.g. a
        // structured value where we read .jsonPrimitive) must surface as an error, not throw out
        // of onMessage and tear the socket down mid-turn.
        try {
            val method = obj["method"]?.jsonPrimitive?.contentOrNull
            val id = obj["id"]
            when {
                method != null && id != null -> serverRequest(method, id, obj["params"] as? JsonObject)
                method != null -> notification(method, obj["params"] as? JsonObject)
                // Tolerate a string-typed id: the spec allows them, and a stringified int
                // ("42") must still find its pending entry rather than silently leak it.
                id != null -> response(
                    (id as? JsonPrimitive)?.let { it.intOrNull ?: it.contentOrNull?.toIntOrNull() },
                    obj["result"] as? JsonObject, obj["error"])
            }
        } catch (e: Exception) {
            onEvent(AcpEvent.Error("bad message: ${e.message}"))
        }
    }

    private fun response(id: Int?, result: JsonObject?, error: JsonElement?) {
        val method = id?.let { pending.remove(it) }   // ConcurrentHashMap rejects a null key
        if (error != null && error !is JsonNull) {
            // An errored call never reaches its dispatch — clean every per-id side map here
            // or the entries leak (and a stale sid mapping mislabels a later reply).
            id?.let { pendingConfigKeys.remove(it); pendingExtListSids.remove(it); pendingToolListSids.remove(it) }
            // A stale/expired session can't be resumed — fall back to a fresh one.
            if (method == "session/load") { replaying = false; startNewSession(); return }
            // Info probe failed (very old goose?): resume with /state rather than hanging.
            if (method == "_goose/unstable/session/info" && loadAwaitingInfo) {
                loadAwaitingInfo = false
                val resume = resumeSessionId
                if (resume != null) {
                    replaying = true
                    onEvent(AcpEvent.ReplayStart)
                    rpc("session/load", buildJsonObject {
                        put("sessionId", resume); put("cwd", resumeCwd)
                        putJsonArray("mcpServers") {}
                    })
                }
                return
            }
            // A failed template fetch must clear the in-flight marker (via AppResource) or the
            // bubble is wedged forever and no retry can ever start — not become an error bubble.
            if (method != null && method.startsWith("appres|")) {
                onEvent(AcpEvent.AppResource(method.substringAfter('|'), "")); return
            }
            // A failed probe IS its answer: the session (or socket) is gone — reconnect.
            if (method != null && method.startsWith("probe|")) {
                onEvent(AcpEvent.Probe(method.substringAfter('|'), null, -1)); return
            }
            // Only the calls whose failure IS the turn's failure belong in the transcript;
            // everything else is a background refresh whose error must not corrupt the chat.
            // steer is foreground: a failed steer means the user's message did NOT reach the
            // model, which they must see (typically the run ended between typing and sending).
            val foreground = method == "session/prompt" || method == "session/new" ||
                method == "_goose/unstable/session/steer"
            onEvent(AcpEvent.Error("$method: $error", background = !foreground)); return
        }
        // MCP-App template fetch: tag carries the cache key because several tools can share
        // one template and several bubbles can wait on one fetch.
        if (method != null && method.startsWith("appres|")) {
            val key = method.substringAfter('|')
            // ReadResourceResponse nests MCP's ReadResourceResult under "result":
            // { result: { contents: [{ uri, mimeType, text }] } }
            val html = ((result?.get("result") as? JsonObject)?.get("contents") as? JsonArray)
                ?.firstNotNullOfOrNull {
                    (it as? JsonObject)?.get("text")?.jsonPrimitive?.contentOrNull
                }
            onEvent(AcpEvent.AppResource(key, html ?: ""))
            return
        }
        when (method) {
            "initialize" -> {
                val resume = resumeSessionId
                if (resume != null && !resumeCwdKnown) {
                    // Ask the server for the session's real cwd rather than guessing.
                    loadAwaitingInfo = true
                    rpc("_goose/unstable/session/info", buildJsonObject { put("sessionId", resume) })
                } else if (resume != null) {
                    replaying = true
                    onEvent(AcpEvent.ReplayStart)
                    rpc("session/load", buildJsonObject {
                        put("sessionId", resume)
                        // session/load's cwd SILENTLY REWRITES the session's working_dir if it
                        // differs from what's stored server-side -- must be the session's real cwd
                        // (resumeCwd, set by the caller from the cached SessionInfo) or a Code
                        // session would get un-scoped back to /state on every reconnect.
                        put("cwd", resumeCwd)
                        putJsonArray("mcpServers") {}
                    })
                } else if (autoNewSession) startNewSession()
                else onEvent(AcpEvent.Status("ready — pick a session"))
            }
            "session/new" -> {
                sessionId = result?.get("sessionId")?.jsonPrimitive?.contentOrNull
                onEvent(AcpEvent.Status(if (sessionId != null) "ready" else "session/new returned no sessionId"))
                sessionId?.let { onEvent(AcpEvent.Ready(it)) }
                val cfg = parseConfig(result)
                onEvent(AcpEvent.Config(cfg))
                applyDesired(cfg)
            }
            "session/load" -> {
                replaying = false
                sessionId = resumeSessionId
                onEvent(AcpEvent.Status("ready"))
                sessionId?.let { onEvent(AcpEvent.Ready(it)) }
                onEvent(AcpEvent.Config(parseConfig(result)))   // reflects this session's model
            }
            "session/list" -> onEvent(AcpEvent.Sessions(parseSessions(result)))
            "_goose/unstable/sources/list" -> onEvent(AcpEvent.Projects(parseProjects(result)))
            "_goose/unstable/schedules/list" -> onEvent(AcpEvent.Schedules(parseSchedules(result)))
            "_goose/unstable/recipes/list" -> onEvent(AcpEvent.Recipes(parseRecipes(result)))
            SKILLS_TAG -> onEvent(AcpEvent.Skills(parseSkills(result)))
            "_goose/unstable/sources/update" -> listSkills()
            // Every mutation re-lists rather than patching local state: the server owns the
            // paused/running flags, and run-now in particular changes them without telling us.
            "_goose/unstable/schedules/pause",
            "_goose/unstable/schedules/unpause",
            "_goose/unstable/schedules/delete",
            "_goose/unstable/schedules/update",
            "_goose/unstable/schedules/run-now" -> listSchedules()
            "_goose/unstable/recipes/schedule" -> { listSchedules(); listRecipes() }
            "_goose/unstable/recipes/save",
            "_goose/unstable/recipes/delete" -> listRecipes()
            // create/assign replies carry no useful body; re-list so the drawer reflects them.
            "_goose/unstable/sources/create" -> listProjects()
            "_goose/unstable/sources/delete" -> listProjects()
            "_goose/unstable/sources/delete#skill" -> listSkills()
            "_goose/unstable/session/project/update" -> listProjects()
            "_goose/unstable/config/extensions/list" -> onEvent(AcpEvent.Extensions(parseExtensions(result)))
            // After a toggle, re-list so the UI reflects the new enabled state.
            "_goose/unstable/config/extensions/set-enabled" -> listExtensions()
            // Session-scoped (a session's OWN extension_data, not the global catalog above). The
            // array elements ARE the extension objects (goose's tagged union carries `name` at the
            // top level), unlike config/extensions/list's {extension:{...}, enabled, configKey} wrap.
            "_goose/unstable/session/extensions/list" -> {
                val target = id?.let { pendingExtListSids.remove(it) } ?: sessionId
                val infos = (result?.get("extensions") as? JsonArray).orEmpty().mapNotNull { el ->
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
                        fromPeer = target?.startsWith("roam:") == true,
                    )
                }
                target?.let { onEvent(AcpEvent.SessionExtensions(it, infos.map { i -> i.name }, infos)) }
            }
            // add/remove reply empty -- ConnectionManager's diff-and-apply already knows the target
            // state, so there's nothing to re-fetch (unlike the global toggle above).
            "_goose/unstable/tools/list" -> {
                val names = (result?.get("tools") as? JsonArray).orEmpty().mapNotNull {
                    (it as? JsonObject)?.get("name")?.jsonPrimitive?.contentOrNull
                }
                onEvent(AcpEvent.Tools(names, id?.let { pendingToolListSids.remove(it) }))
            }
            "_goose/unstable/config/extensions/add" -> listExtensions()
            "_goose/unstable/session/extensions/add" -> listTools()
            "_goose/unstable/session/extensions/remove" -> {}   // the paired add re-lists
            "_goose/unstable/config/read" -> {
                // The reply doesn't echo the key, so recover it from the request we cached.
                val key = pendingConfigKeys.remove(id) ?: return
                val v = result?.get("value")
                val s = when (v) {
                    is JsonPrimitive -> v.contentOrNull ?: ""
                    null -> ""
                    else -> v.toString()
                }
                onEvent(AcpEvent.ServerConfig(key, s))
            }
            // Upsert returns empty; nothing to reflect (the caller re-reads if it wants confirmation).
            "_goose/unstable/config/upsert" -> {}
            "_goose/unstable/providers/supported-models/list" -> {
                val providerId = result?.get("providerId")?.jsonPrimitive?.contentOrNull ?: return
                val models = (result["models"] as? JsonArray).orEmpty()
                    .mapNotNull { it.jsonPrimitive.contentOrNull }
                onEvent(AcpEvent.SupportedModels(providerId, models))
            }
            // Rename returns empty; re-list so every consumer sees the new title.
            "_goose/unstable/session/rename" -> listSessions()
            "_goose/unstable/session/steer" -> {}   // the steered message streams back as chunks
            "_goose/unstable/session/export" ->
                result?.get("data")?.jsonPrimitive?.contentOrNull
                    ?.let { onEvent(AcpEvent.SessionExport(it)) }
            "_goose/unstable/tools/call" -> {
                val texts = (result?.get("content") as? JsonArray).orEmpty().mapNotNull {
                    (it as? JsonObject)?.get("text")?.jsonPrimitive?.contentOrNull
                }
                onEvent(AcpEvent.DirectToolResult(
                    texts.joinToString("\n"),
                    result?.get("isError")?.jsonPrimitive?.booleanOrNull ?: false))
            }
            "_goose/unstable/session/archive" -> listSessions()
            "session/delete" -> listSessions()
            "_goose/unstable/session/unarchive" -> listSessions()
            "_goose/unstable/session/working-dir/update" -> {}   // caller updates optimistically
            "_goose/unstable/session/info" -> {
                val cwd = (result?.get("session") as? JsonObject)
                    ?.get("cwd")?.jsonPrimitive?.contentOrNull
                if (loadAwaitingInfo) {
                    loadAwaitingInfo = false
                    resumeCwd = cwd?.takeIf { it.isNotBlank() } ?: desiredCwd
                    resumeCwdKnown = true
                    val resume = resumeSessionId
                    if (resume != null) {
                        replaying = true
                        onEvent(AcpEvent.ReplayStart)
                        rpc("session/load", buildJsonObject {
                            put("sessionId", resume)
                            put("cwd", resumeCwd)
                            putJsonArray("mcpServers") {}
                        })
                    }
                }
            }
            "session/set_config_option" -> onEvent(AcpEvent.Config(parseConfig(result)))
            "session/set_mode" -> {}
            "session/prompt" ->
                onEvent(AcpEvent.TurnDone(result?.get("stopReason")?.jsonPrimitive?.contentOrNull ?: "end"))
            else -> if (method?.startsWith("probe|") == true) {
                // Probe reply: list-style metadata without loading the conversation.
                val s = result?.get("session") as? JsonObject
                onEvent(AcpEvent.Probe(
                    method.substringAfter('|'),
                    s?.get("updatedAt")?.jsonPrimitive?.contentOrNull,
                    (s?.get("_meta") as? JsonObject)?.get("messageCount")?.jsonPrimitive?.intOrNull ?: 0))
            }
        }
    }

    private fun startNewSession() = rpc("session/new", buildJsonObject {
        // cwd must exist INSIDE the goose container (not the host). Conversational chats live
        // under <home>/Projects/<Name> (Inbox for unfiled) -- that path is also what Goose
        // Desktop groups on to build its project list, so the cwd IS the project.
        put("cwd", desiredCwd)
        putJsonArray("mcpServers") {}
        // WITHOUT THIS, EVERY CHAT STARTED HERE IS INVISIBLE IN GOOSE DESKTOP.
        // goose types a new session from this one field (acp/server/new_session.rs):
        //     _meta.client present -> SessionType::User ; absent -> SessionType::Acp
        // and Desktop asks session/list for types ['user','scheduled'] only. Omitting it meant
        // Grouse chats were 'acp' and structurally unlistable there -- measured 3 sessions
        // visible against 50 actually present. The goose CLI's `session list` filters the same
        // way, which is what let deliver.sh create a duplicate Assistant it could not see
        // (the 2026-07-26 fork). The value is not interpreted; only its presence matters.
        putJsonObject("_meta") {
            put("client", "grouse")
            // Start FROM a recipe: goose resolves it server-side, applies its extensions,
            // settings and instructions, and names the session after the recipe.
            desiredRecipeId?.let { put("recipeId", it) }
        }
    })

    internal fun parseConfig(result: JsonObject?): List<ConfigOption> {
        val arr = result?.get("configOptions") as? JsonArray ?: return emptyList()
        return arr.mapNotNull { el ->
            val o = el as? JsonObject ?: return@mapNotNull null
            val id = o["id"]?.jsonPrimitive?.contentOrNull ?: return@mapNotNull null
            val choices = (o["options"] as? JsonArray).orEmpty().mapNotNull { c ->
                val co = c as? JsonObject ?: return@mapNotNull null
                val v = co["value"]?.jsonPrimitive?.contentOrNull ?: return@mapNotNull null
                Choice(v, co["name"]?.jsonPrimitive?.contentOrNull ?: v)
            }
            ConfigOption(
                id = id,
                name = o["name"]?.jsonPrimitive?.contentOrNull ?: id,
                currentValue = o["currentValue"]?.jsonPrimitive?.contentOrNull ?: "",
                choices = choices,
            )
        }
    }

    /** sources/list returns SourceEntry objects; a project's slug is its file stem, which is
     *  what session.projectId holds. `name` is the human label and may differ. */
    internal fun parseProjects(result: JsonObject?): List<ProjectInfo> {
        val arr = result?.get("sources") as? JsonArray ?: return emptyList()
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

    internal fun parseSkills(result: JsonObject?): List<SkillInfo> {
        val arr = result?.get("sources") as? JsonArray ?: return emptyList()
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

    internal fun parseSchedules(result: JsonObject?): List<ScheduleInfo> {
        val arr = result?.get("jobs") as? JsonArray ?: return emptyList()
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

    internal fun parseRecipes(result: JsonObject?): List<RecipeInfo> {
        val arr = result?.get("recipes") as? JsonArray ?: return emptyList()
        return arr.mapNotNull { el ->
            val e = el as? JsonObject ?: return@mapNotNull null
            val r = e["recipe"] as? JsonObject ?: return@mapNotNull null
            val settings = r["settings"] as? JsonObject
            RecipeInfo(
                id = e["id"]?.jsonPrimitive?.contentOrNull ?: return@mapNotNull null,
                title = r["title"]?.jsonPrimitive?.contentOrNull ?: "(untitled)",
                description = r["description"]?.jsonPrimitive?.contentOrNull ?: "",
                // snake_case, like recipes/schedule's cron_schedule and unlike most of goose's
                // ACP surface. Read as camelCase these came back null, which does not fail --
                // it just makes every recipe look unscheduled and unlinkable to its job.
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

    internal fun parseSessions(result: JsonObject?): List<SessionInfo> {
        val arr = result?.get("sessions") as? JsonArray ?: return emptyList()
        return arr.mapNotNull { el ->
            val o = el as? JsonObject ?: return@mapNotNull null
            val sid = o["sessionId"]?.jsonPrimitive?.contentOrNull ?: return@mapNotNull null
            val title = o["title"]?.jsonPrimitive?.contentOrNull ?: sid
            // Drop scheduler-created sessions (goose names them "Scheduled job: <id>") so they don't
            // clutter the list. (Belt-and-suspenders: the proactive job now runs --no-session anyway.)
            if (title.startsWith("Scheduled job:")) return@mapNotNull null
            val meta = o["_meta"] as? JsonObject
            // goose's archive only stamps archivedAt — session/list has NO archived filter
            // (verified in source: list_sessions_paged never checks it), so every client must
            // filter for itself or archived sessions pop right back on the next refresh.
            if (meta?.get("archivedAt") != null) return@mapNotNull null
            SessionInfo(
                sessionId = sid,
                title = title,
                updatedAt = o["updatedAt"]?.jsonPrimitive?.contentOrNull ?: "",
                messageCount = meta?.get("messageCount")?.jsonPrimitive?.intOrNull ?: 0,
                model = meta?.get("modelId")?.jsonPrimitive?.contentOrNull ?: "",
                snippet = meta?.get("lastMessageSnippet")?.jsonPrimitive?.contentOrNull ?: "",
                cwd = o["cwd"]?.jsonPrimitive?.contentOrNull ?: "",
                hasRecipe = meta?.get("hasRecipe")?.jsonPrimitive?.booleanOrNull ?: false,
                projectId = meta?.get("projectId")?.jsonPrimitive?.contentOrNull,
            )
        }
    }

    /** Parse the config/extensions/list reply: {extensions:[{extension:{name,type,description}, enabled, configKey}]}. */
    internal fun parseExtensions(result: JsonObject?): List<ExtInfo> {
        val arr = result?.get("extensions") as? JsonArray ?: return emptyList()
        return arr.mapNotNull { el ->
            val o = el as? JsonObject ?: return@mapNotNull null
            val ext = o["extension"] as? JsonObject ?: return@mapNotNull null
            // type=mcp extensions (nextcloud, kagi, fastmail, Fetch -- anything backed by an actual
            // MCP server) carry their name NESTED at extension.server.name, not the top-level
            // extension.name that builtin/platform types use. Missing this silently dropped every
            // MCP extension from the list (confirmed live: nextcloud/kagi/fastmail/Fetch all lack a
            // top-level name). Check both.
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

    /** Re-apply persisted picks after a session opens (provider first for the model cascade). */
    private fun applyDesired(cfg: List<ConfigOption>) {
        if (desiredOptions.isEmpty()) return
        val current = cfg.associate { it.id to it.currentValue }
        for (id in applyOrder) {
            val want = desiredOptions[id] ?: continue
            if (want.isNotBlank() && want != current[id]) setConfigOption(id, want)
        }
    }

    private fun notification(method: String, params: JsonObject?) {
        when (method) {
            "session/update" -> standardUpdate(params)
            "_goose/unstable/session/update" -> gooseUpdate(params)
        }
    }

    private fun gooseUpdate(params: JsonObject?) {
        val update = params?.get("update") as? JsonObject ?: return
        when (update["sessionUpdate"]?.jsonPrimitive?.contentOrNull) {
            "status_message" -> {
                val status = update["status"] as? JsonObject ?: return
                val msg = status["message"]?.jsonPrimitive?.contentOrNull ?: return
                onEvent(AcpEvent.CompactionStatus(msg))
            }
            // Per-message tok/s + cost (goose-sdk-types MessageUsageData: outputTokens, elapsedMs,
            // timeToFirstTokenMs, cost — camelCase on the wire). Distinct from the standard ACP
            // usage_update (context-window used/size) already handled in standardUpdate().
            "message_usage" -> {
                // Per-message stats belong to the bound session's turn; a broadcast for another
                // session would stamp wrong numbers onto this chat's last reply.
                val bound = sessionId ?: resumeSessionId
                val sid = params?.get("sessionId")?.jsonPrimitive?.contentOrNull
                if (bound != null && sid != null && sid != bound) return
                val usage = update["usage"] as? JsonObject ?: return
                val outTok = usage["outputTokens"]?.jsonPrimitive?.intOrNull ?: return
                val elapsed = usage["elapsedMs"]?.jsonPrimitive?.longOrNull ?: return
                if (elapsed <= 0) return   // can't derive a tok/s rate from a zero/missing duration
                val ttft = usage["timeToFirstTokenMs"]?.jsonPrimitive?.longOrNull ?: 0L
                val cost = usage["cost"]?.jsonPrimitive?.doubleOrNull
                onEvent(AcpEvent.MessageUsage(outTok, elapsed, ttft, cost))
            }
        }
    }

    private fun standardUpdate(params: JsonObject?) {
        val update = params?.get("update") as? JsonObject ?: return
        val tag = update["sessionUpdate"]?.jsonPrimitive?.contentOrNull
        // goose broadcasts session/update for OTHER sessions onto the same socket (turns this
        // client started elsewhere, active-run lifecycles). The transcript-mutating tags must
        // only render when they belong to the session THIS transport is bound to — otherwise a
        // response to a chat you left streams into whichever chat is on screen. Metadata tags
        // (session_info_update, mode/config/commands) carry their own session id and are wanted
        // regardless of which session they name, so they pass. During the pre-bind replay
        // window (sessionId null) resumeSessionId is the session being loaded.
        if (tag in TRANSCRIPT_TAGS) {
            val bound = sessionId ?: resumeSessionId
            val sid = params?.get("sessionId")?.jsonPrimitive?.contentOrNull
            if (bound != null && sid != null && sid != bound) return
        }
        // Replays are never suppressed: every reconnect rebuilds the transcript from the server's
        // history (see AcpEvent.ReplayStart). Suppression used to guard a socket blip against
        // duplicate bubbles, but it couldn't tell "what I already show" from "turns another client
        // added while I was away", so those turns were silently dropped.
        // Non-text content blocks (image/audio/resource_link) used to vanish entirely; a
        // placeholder keeps the message's existence visible even when we can't render it.
        fun text(): String? {
            val c = update["content"] as? JsonObject ?: return null
            return when (val type = c["type"]?.jsonPrimitive?.contentOrNull) {
                "text", null -> c["text"]?.jsonPrimitive?.contentOrNull
                "resource_link" -> "[resource: ${c["name"]?.jsonPrimitive?.contentOrNull
                    ?: c["uri"]?.jsonPrimitive?.contentOrNull ?: "unnamed"}]"
                else -> "[$type]"
            }
        }
        fun msgId() = ((update["_meta"] as? JsonObject)?.get("goose") as? JsonObject)
            ?.get("messageId")?.jsonPrimitive?.contentOrNull
        when (tag) {
            // user_message_chunk only appears during a session/load replay (live prompts aren't echoed).
            "user_message_chunk" -> text()?.let { onEvent(AcpEvent.UserChunk(it, msgId())) }
            "agent_message_chunk" -> text()?.let { onEvent(AcpEvent.AgentChunk(it, msgId())) }
            // Thoughts stream live (own collapsible bubble); skipped in a rebuilt transcript.
            "agent_thought_chunk" -> if (!replaying) text()?.let { onEvent(AcpEvent.ThoughtChunk(it)) }
            "tool_call" -> {
                val goose = (update["_meta"] as? JsonObject)?.get("goose") as? JsonObject
                val toolName = (goose?.get("toolCall") as? JsonObject)
                    ?.get("toolName")?.jsonPrimitive?.contentOrNull
                val rawInput = update["rawInput"] as? JsonObject
                // MCP-App path: the server names a UI resource for this tool's output and the
                // client is expected to fetch + render it. This is how ALL the autovisualiser
                // types work, not just charts — sankey/radar/map/mermaid never had a bespoke
                // branch here, which is why they showed as bare tool calls.
                val mcpApp = goose?.get("mcpApp") as? JsonObject
                val appUri = mcpApp?.get("resourceUri")?.jsonPrimitive?.contentOrNull
                val appExt = mcpApp?.get("extensionName")?.jsonPrimitive?.contentOrNull
                // Legacy fallback only (server too old to send mcpApp meta): `data` arrives as
                // an OBJECT, not a string — reading only the primitive form silently disabled
                // every chart once.
                val chartData = when (val d = rawInput?.get("data")) {
                    is JsonObject -> d.toString()
                    is JsonPrimitive -> d.contentOrNull
                    else -> null
                }
                if (appUri != null && appExt != null) {
                    onEvent(AcpEvent.ToolCall(
                        update["title"]?.jsonPrimitive?.contentOrNull ?: "tool call",
                        toolCallId = update["toolCallId"]?.jsonPrimitive?.contentOrNull ?: "",
                        appKey = "$appExt|$appUri", appUri = appUri, appExt = appExt,
                        appInput = rawInput?.toString() ?: "{}"))
                } else if (toolName == "autovisualiser__show_chart" && chartData != null) {
                    onEvent(AcpEvent.Chart(chartData))
                } else {
                    // Same rawInput.command-first extraction the permission sheet already does —
                    // Desktop shows this detail, Grouse was dropping it and only keeping the title.
                    val detail = rawInput?.let { ri ->
                        ri["command"]?.jsonPrimitive?.contentOrNull
                            ?: ri.toString().takeIf { it != "{}" } ?: ""
                    } ?: ""
                    onEvent(AcpEvent.ToolCall(
                        update["title"]?.jsonPrimitive?.contentOrNull ?: "tool call", detail,
                        update["toolCallId"]?.jsonPrimitive?.contentOrNull ?: ""))
                }
            }
            "usage_update" -> {
                val used = update["used"]?.jsonPrimitive?.intOrNull ?: 0
                val size = update["size"]?.jsonPrimitive?.intOrNull ?: 0
                val cost = update["cost"] as? JsonObject
                onEvent(AcpEvent.Usage(used, size,
                    cost?.get("amount")?.jsonPrimitive?.doubleOrNull ?: 0.0,
                    cost?.get("currency")?.jsonPrimitive?.contentOrNull ?: ""))
            }
            "tool_call_update" -> {
                val id = update["toolCallId"]?.jsonPrimitive?.contentOrNull ?: return
                val status = update["status"]?.jsonPrimitive?.contentOrNull ?: ""
                // Streaming shell output rides _meta.toolNotification (tagged union; the
                // live_output variant carries {params:{chunks:[{stream,output}]}}). Without
                // this a long shell run is a blank chip until it finishes.
                val notif = (update["_meta"] as? JsonObject)?.get("toolNotification") as? JsonObject
                if (notif?.get("type")?.jsonPrimitive?.contentOrNull == "live_output") {
                    val chunk = ((notif["params"] as? JsonObject)?.get("chunks") as? JsonArray)
                        .orEmpty().mapNotNull {
                            (it as? JsonObject)?.get("output")?.jsonPrimitive?.contentOrNull
                        }.joinToString("")
                    if (chunk.isNotEmpty()) onEvent(AcpEvent.ToolCallUpdate(id, status, chunk, live = true))
                    return
                }
                // content: [{type:"content", content:{type:"text", text:...}}, ...]
                val output = (update["content"] as? JsonArray).orEmpty().mapNotNull { el ->
                    ((el as? JsonObject)?.get("content") as? JsonObject)
                        ?.get("text")?.jsonPrimitive?.contentOrNull
                }.joinToString("\n")
                onEvent(AcpEvent.ToolCallUpdate(id, status, output))
            }
            "session_info_update" -> {
                val sid = params["sessionId"]?.jsonPrimitive?.contentOrNull ?: return
                // This notification is overloaded: title updates, active-run lifecycle, and
                // queued-steer acks share one tag, distinguished only by which _meta.goose
                // keys are present. activeRunId is what makes session/steer possible at all.
                val gm = (update["_meta"] as? JsonObject)?.get("goose") as? JsonObject
                if (gm?.containsKey("activeRunId") == true) {
                    onEvent(AcpEvent.ActiveRun(sid, gm["activeRunId"]?.jsonPrimitive?.contentOrNull))
                }
                onEvent(AcpEvent.SessionInfoChanged(
                    sid,
                    update["title"]?.jsonPrimitive?.contentOrNull,
                    update["updatedAt"]?.jsonPrimitive?.contentOrNull))
            }
            "config_option_update" -> {
                val opts = parseConfig(update)
                if (opts.isNotEmpty()) onEvent(AcpEvent.Config(opts))
            }
            "current_mode_update" -> {
                update["currentModeId"]?.jsonPrimitive?.contentOrNull
                    ?.let { onEvent(AcpEvent.ModeChanged(it)) }
            }
            "available_commands_update" -> {
                val names = (update["availableCommands"] as? JsonArray).orEmpty().mapNotNull {
                    (it as? JsonObject)?.get("name")?.jsonPrimitive?.contentOrNull
                }
                if (names.isNotEmpty()) onEvent(AcpEvent.Commands(names))
            }
        }
    }

    private fun serverRequest(method: String, id: JsonElement, params: JsonObject?) {
        if (method == "session/request_permission") {
            // Surface it to the user — goose's mode (smart_approve by default) already decides
            // which tools reach here; we just present the decision.
            val tc = params?.get("toolCall") as? JsonObject
            val toolCallId = tc?.get("toolCallId")?.jsonPrimitive?.contentOrNull
            if (toolCallId == null) {
                // A permission we can't correlate can't be asked — answer with a VALID
                // cancelled outcome, not `{}` (which is not a RequestPermissionResponse).
                respond(id, buildJsonObject {
                    putJsonObject("outcome") { put("outcome", "cancelled") }
                })
                return
            }
            val title = tc["title"]?.jsonPrimitive?.contentOrNull ?: "tool"
            val detail = (tc["rawInput"] as? JsonObject)?.let { ri ->
                ri["command"]?.jsonPrimitive?.contentOrNull ?: ri.toString()
            } ?: ""
            val opts = (params["options"] as? JsonArray).orEmpty().mapNotNull { el ->
                val o = el as? JsonObject ?: return@mapNotNull null
                val oid = o["optionId"]?.jsonPrimitive?.contentOrNull ?: return@mapNotNull null
                PermOption(oid, o["name"]?.jsonPrimitive?.contentOrNull ?: oid,
                    o["kind"]?.jsonPrimitive?.contentOrNull ?: "")
            }
            pendingPermissions[toolCallId] = id
            onEvent(AcpEvent.Permission(toolCallId, title, detail, opts))
        } else if (method == "elicitation/create") {
            val mode = params?.get("mode")?.jsonPrimitive?.contentOrNull
            val schema = params?.get("requestedSchema") as? JsonObject
            if (mode != "form" || schema == null) {
                respond(id, buildJsonObject { put("action", "cancel") }); return
            }
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
            val key = "elicit-" + elicitSeq.getAndIncrement()
            pendingElicitations[key] = id
            onEvent(AcpEvent.Elicitation(
                key,
                params["message"]?.jsonPrimitive?.contentOrNull ?: "Input requested",
                schema["title"]?.jsonPrimitive?.contentOrNull ?: "",
                fields))
        } else if (method == "_goose/unstable/session/recipe/request-params") {
            // A recipe with declared parameters, started via _meta.recipeId. Rendered through
            // the SAME form UI as elicitations (the field model maps 1:1); the answer is routed
            // back here by which pending map owns the requestKey, because the response shapes
            // differ: {action: "submit"|"cancel", values:{...}} vs elicitation's accept/content.
            // NOTE the params envelope is camelCase but each parameter DTO is snake_case
            // (input_type) — the recipes family's usual casing split.
            val fields = (params?.get("parameters") as? JsonArray).orEmpty().mapNotNull { el ->
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
            val key = "recipeparams-${elicitSeq.getAndIncrement()}"
            pendingRecipeParams[key] = id
            onEvent(AcpEvent.Elicitation(key, "This recipe needs parameters", "Recipe parameters", fields))
        } else {
            // Unknown server request: a real JSON-RPC error, not `{}` — see respondError.
            respondError(id, -32601, "not supported by this client: $method")
        }
    }
}
