package id.gauvin.grouse

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

/**
 * The app's UI DTOs. Kept as plain records, exactly the shapes the UI reads; the
 * controller translates grouse-core records into these (one mapping layer, per the
 * desktop's pattern). The grouse-core records are enriched (SessionSummary carries
 * projectId/messageCount/model/hasRecipe; ConfigOption carries named choices), so the
 * translation is mechanical.
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

/** A non-image file attached to a prompt (uri + mime + one of text/blob). */
data class FileBlock(
    val name: String,
    val mimeType: String,
    val text: String? = null,
    val blobB64: String? = null,
)

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
    /** True when this session was started from a recipe (session/list's `hasRecipe`). */
    val hasRecipe: Boolean = false,
    /** The project this session is filed under, or null for unfiled. */
    val projectId: String? = null,
    /** True while the session has backgrounded (staged) content the UI has not
     *  shown yet — the green-dot indicator. */
    val hasNew: Boolean = false,
)

/** A goose project: a named source with a slug, NOT a directory. */
data class ProjectInfo(
    val id: String,
    val name: String,
    val description: String,
    val path: String = "",
    /** Directory this project's chats work in, from a `root: <path>` line in the project's own
     *  content. The core's ProjectSummary does not carry content, so this is only populated
     *  through the unstable sources/list path. */
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

/** One skill from `sources/list {type: skill}` -- the same API that backs projects. */
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
    val configKey: String,   // key in config.yaml
    // Core/required -- never offered for removal by a per-session extension profile (safety guard).
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

internal val chatMessageSeq = java.util.concurrent.atomic.AtomicLong(0)
/** Stable per-message id so the chat LazyColumn keys on identity, not position. copy() preserves it,
 *  so the streaming message keeps the same id as its text grows → its composition is reused, not rebuilt. */
data class ChatMessage(
    val role: String,
    val text: String,
    val images: List<ImageBlock> = emptyList(),
    val files: List<FileBlock> = emptyList(),
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

/** UI-facing event records translated from the grouse-core listeners. Only the shapes the
 *  Screens actually consume survive; everything wire-specific lives in the controller now. */
sealed interface AcpEvent {
    /** Context window used/size + cost (the standard ACP usage_update). */
    data class Usage(val used: Int, val size: Int, val cost: Double, val currency: String) : AcpEvent

    /** Per-message generation stats (tok/s + cost) for one finished assistant reply. */
    data class MessageUsage(
        val outputTokens: Int, val elapsedMs: Long, val ttftMs: Long, val cost: Double?,
    ) : AcpEvent

    /** A pending tool-approval request (session/request_permission). */
    data class Permission(
        val toolCallId: String, val title: String, val detail: String, val options: List<PermOption>,
    ) : AcpEvent

    /** One field of a form elicitation. `type` is string/number/integer/boolean; a non-empty
     *  `options` list means single-select (rendered as choices instead of free text). */
    data class ElicitField(
        val name: String, val type: String, val title: String, val description: String,
        val options: List<Choice>, val required: Boolean,
    )

    /** A tool/extension is requesting structured input (MCP elicitation, ACP form mode, or a
     *  recipe's parameter request). Answer with [ConnectionManager.answerElicitation]:
     *  accept with values, decline, or cancel. `recipeParams` routes the answer to
     *  respond_recipe_params (submit/cancel) instead of respond_elicitation (accept/decline). */
    data class Elicitation(
        val requestKey: String, val message: String, val title: String,
        val fields: List<ElicitField>,
        val recipeParams: Boolean = false,
    ) : AcpEvent
}

/** One entry of goose's provider inventory (`_goose/unstable/providers/list`).
 *
 *  `configured` is the server's own judgement of whether it has enough config to USE the
 *  provider, and `models` is that provider's inventory — both were previously guessed by
 *  the app from hardcoded lists. */
data class ProviderInfo(
    val id: String,
    val name: String,
    val configured: Boolean,
    val models: List<String>,
)
