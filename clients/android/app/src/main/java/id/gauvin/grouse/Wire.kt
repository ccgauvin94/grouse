// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.put
import kotlinx.serialization.json.jsonPrimitive

import uniffi.grouse_core.ConfigOption as CoreConfigOption
import uniffi.grouse_core.ProjectSummary
import uniffi.grouse_core.SessionSummary

/**
 * The wire parsers and record translators formerly nested in
 * [ConnectionManager]'s companion — moved to top-level internal so they are
 * pure, JVM-testable, and not buried in the god file. Same package, so the
 * in-class callers in `ConnectionManager` resolve these unqualified.
 */

/** (De)serialise the remembered extension-key -> full-catalogue map (the same
 *  thing toolCatalog holds in memory, and the desktop persists per session).
 *  An EMPTY list is real knowledge — "this extension has no namespaced
 *  sub-tools" — so it round-trips too; that is what keeps a bare-named
 *  builtin's arrow from resurrecting on the next launch. */
internal fun parseToolCatalogCache(json: String): Map<String, List<String>> {
    if (json.isBlank()) return emptyMap()
    val o = try { Json.parseToJsonElement(json) as? JsonObject } catch (e: Exception) { null } ?: return emptyMap()
    return o.mapNotNull { (k, v) ->
        val arr = v as? JsonArray ?: return@mapNotNull null
        k to arr.mapNotNull { it.jsonPrimitive.contentOrNull }
    }.toMap()
}

internal fun encodeToolCatalog(catalogs: Map<String, List<String>>): String =
    buildJsonObject {
        catalogs.forEach { (k, v) -> put(k, JsonArray(v.map { JsonPrimitive(it) })) }
    }.toString()

/** (De)serialise the per-session pinned MCP-App (SecureStore's `pinned_apps`): one appKey
 *  ("<extension>|<uri>") per session. Android pins a single app; the desktop's top/bottom
 *  panes are its own store. Sessions with no pin are dropped, so unpinning removes the
 *  entry rather than leaving an empty tombstone. */
internal fun parsePinnedApps(json: String): Map<String, String> {
    if (json.isBlank()) return emptyMap()
    val o = try { Json.parseToJsonElement(json) as? JsonObject } catch (e: Exception) { null } ?: return emptyMap()
    return o.mapNotNull { (session, v) ->
        // A string is the current shape. An object is the earlier top/bottom build — read its
        // slots so a pin made before this change survives instead of crashing the app on start.
        val key = when (v) {
            is JsonPrimitive -> v.contentOrNull.orEmpty()
            is JsonObject -> v["top"]?.jsonPrimitive?.contentOrNull?.takeIf { it.isNotEmpty() }
                ?: v["bottom"]?.jsonPrimitive?.contentOrNull.orEmpty()
            else -> ""
        }
        if (key.isEmpty()) null else session to key
    }.toMap()
}

internal fun encodePinnedApps(apps: Map<String, String>): String =
    buildJsonObject {
        apps.filterValues { it.isNotEmpty() }.forEach { (session, key) -> put(session, JsonPrimitive(key)) }
    }.toString()

/** (De)serialise the on-disk MCP-App template cache (SecureStore's `app_templates`):
 *  appKey ("<extension>|<uri>") -> HTML. Templates are static per server version, so keeping
 *  them means a pinned app (and a transcript replay) paints on a cold start with no fetch and
 *  no dependence on when the session became active. Capped so prefs can't grow unbounded. */
internal fun parseAppTemplates(json: String): Map<String, String> {
    if (json.isBlank()) return emptyMap()
    val o = try { Json.parseToJsonElement(json) as? JsonObject } catch (e: Exception) { null } ?: return emptyMap()
    return o.mapNotNull { (k, v) ->
        (v as? JsonPrimitive)?.contentOrNull?.takeIf { it.isNotEmpty() }?.let { k to it }
    }.toMap()
}

internal fun encodeAppTemplates(templates: Map<String, String>, cap: Int = 32): String =
    buildJsonObject {
        templates.filterValues { it.isNotEmpty() }.toList().takeLast(cap)
            .forEach { (k, v) -> put(k, JsonPrimitive(v)) }
    }.toString()

/** The four selectable config ids the app drives via `setConfigOption`. */
internal val CONFIG_IDS = listOf("provider", "model", "mode", "thinking_effort")

// ---------------------------------------------------------------------------
// Record translation (core -> app DTO)
// ---------------------------------------------------------------------------

internal fun SessionSummary.toInfo(): SessionInfo = SessionInfo(
    sessionId = id,
    title = title,
    updatedAt = updatedAt,
    messageCount = messageCount.toInt(),
    model = model,
    snippet = lastMessageSnippet.orEmpty(),
    hasRecipe = hasRecipe,
    projectId = projectId,
    hasNew = hasNew,
    archived = archived,
)

/** RFC3339/ISO8601 `updatedAt` -> epoch millis for chronological sorting. The server's
 *  timestamps can vary in shape (fractional seconds, offsets, Z), so string comparison is
 *  unreliable (recent sessions would mis-sort). Parse when possible; fall back to 0. */
private fun chronoEpoch(ts: String): Long =
    try { java.time.OffsetDateTime.parse(ts).toInstant().toEpochMilli() }
    catch (_: Exception) { try { java.time.Instant.parse(ts).toEpochMilli() } catch (_: Exception) { 0L } }

internal fun SessionInfo.updatedEpoch(): Long = chronoEpoch(updatedAt)

internal fun CoreConfigOption.toApp(): ConfigOption = ConfigOption(
    id = id,
    name = name,
    currentValue = value,
    choices = choices.map { Choice(it.value, it.name) },
)

internal fun ProjectSummary.toInfo(): ProjectInfo = ProjectInfo(
    id = path.substringAfterLast('/').removeSuffix(".md").ifEmpty { name },
    name = name,
    description = description.orEmpty(),
    path = path,
    root = "",
)

// ---------------------------------------------------------------------------
// Unstable payload parsers (the core hands the raw JSON reply payloads)
// ---------------------------------------------------------------------------

/** Parse `_goose/unstable/recipes/list` entries. Pure so it is JVM-testable without a
 *  Context; a malformed payload yields an empty list, never a throw. */
internal fun parseRecipes(json: String): List<RecipeInfo> {
    val arr = try {
        Json.parseToJsonElement(json) as? JsonArray
    } catch (e: Exception) { null } ?: return emptyList()
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

/** Parse `_goose/unstable/recipes/schedule/list` entries. Pure so it is JVM-testable
 *  without a Context; a malformed payload yields an empty list, never a throw. */
internal fun parseSchedules(json: String): List<ScheduleInfo> {
    val arr = try {
        Json.parseToJsonElement(json) as? JsonArray
    } catch (e: Exception) { null } ?: return emptyList()
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

/** Parse `_goose/unstable/config/extensions/list`. Pure so it is JVM-testable without a
 *  Context; a malformed payload yields an empty list, never a throw. */
internal fun parseGlobalExtensions(json: String): List<ExtInfo> {
    val arr = try {
        Json.parseToJsonElement(json) as? JsonArray
    } catch (e: Exception) { null } ?: return emptyList()
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

/** Parse `_goose/unstable/session/extensions/list`. Current goose WRAPS each entry:
 *  `{"extension": {...}, "extensionKey": "..."}` — the same wrap
 *  config/extensions/list uses, with `extensionKey` where `configKey` is there.
 *  An older server (and the raw tagged union some replies still carry) sends the
 *  extension object bare; both shapes parse. Identity is the KEY, never
 *  `extension.name` — "Extension Manager" is keyed `extensionmanager`, and only the
 *  key round-trips through session/extensions/remove. `fromPeer` is supplied by the
 *  caller (instance state); the parsing itself is pure so it is JVM-testable. A
 *  malformed payload yields an empty list, never a throw. */
internal fun parseSessionExtensions(json: String, fromPeer: Boolean): List<ExtInfo> {
    val arr = try {
        Json.parseToJsonElement(json) as? JsonArray
    } catch (e: Exception) { null } ?: return emptyList()
    return arr.mapNotNull { el ->
        val wrap = el as? JsonObject ?: return@mapNotNull null
        val ext = (wrap["extension"] as? JsonObject) ?: wrap
        val name = ext["name"]?.jsonPrimitive?.contentOrNull
            ?: (ext["server"] as? JsonObject)?.get("name")?.jsonPrimitive?.contentOrNull
            ?: return@mapNotNull null
        val key = wrap["extensionKey"]?.jsonPrimitive?.contentOrNull ?: name
        ExtInfo(
            name = name,
            enabled = true,   // attached to the session by definition
            type = ext["type"]?.jsonPrimitive?.contentOrNull ?: "",
            description = ext["description"]?.jsonPrimitive?.contentOrNull ?: "",
            configKey = key,
            bundled = ext["bundled"]?.jsonPrimitive?.booleanOrNull ?: false,
            raw = ext,
            fromPeer = fromPeer,
        )
    }
}
