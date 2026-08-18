package id.gauvin.grouse

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * Persisted app state. Non-secret config lives in plain SharedPreferences; the X-Secret-Key
 * (which can drive an RCE-capable agent) lives in Keystore-backed EncryptedSharedPreferences.
 */
class SecureStore(context: Context) {
    private val app = context.applicationContext
    private val cfg: SharedPreferences = app.getSharedPreferences("goose", Context.MODE_PRIVATE)

    private val secure: SharedPreferences by lazy { openSecure() }

    private fun openSecure(): SharedPreferences {
        fun create(): SharedPreferences {
            val masterKey = MasterKey.Builder(app)
                .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
                .build()
            return EncryptedSharedPreferences.create(
                app, "goose_secure", masterKey,
                EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
                EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
            )
        }
        return try {
            create()
        } catch (e: Exception) {
            // The Keystore-wrapped keyset can't be decrypted (e.g. app data restored to a new
            // device, or the key was invalidated). Reset the store so the app starts and can
            // re-onboard, instead of hard-crashing on every launch. The secret must be re-entered.
            app.deleteSharedPreferences("goose_secure")
            create()
        }
    }

    init {
        // One-time migration: the POC stored the key in plaintext "goose" prefs.
        val legacy = cfg.getString("key", null)
        if (!legacy.isNullOrBlank() && secure.getString("key", null).isNullOrBlank()) {
            secure.edit().putString("key", legacy).apply()
            cfg.edit().remove("key").apply()
        }
        // Move off the ACP-only server (:3285) to the full agent server (:3284) which also
        // serves ACP and exposes the extension API.
        if (cfg.getString("port", null) == "3285") cfg.edit().putString("port", "3284").apply()
        // 3284 was the PLAIN-goose port; this client is TLS-only (wss), so nothing on 3284
        // was ever reachable through it. A saved 3284 is the failed "no port" default from
        // an earlier build — move it to the wss default (443), which is what the empty-port
        // setup actually needs (goose.gauvin.id only forwards 443).
        if (cfg.getString("port", null) == "3284") cfg.edit().putString("port", "443").apply()
        // The per-provider known_models cache is GONE (2026-07-25). Models are fetched live
        // from the server on every connect and held in memory only -- see
        // ConnectionManager's supportedModels handling. Two one-time migrations used to live
        // here (flat -> per-provider split, then a cross-provider sanitize); both existed
        // purely to repair a cache that should never have been persistent, so they went with
        // it. Drop the dead keys so an upgraded install doesn't carry them forever.
        cfg.edit()
            .remove("known_models")
            .remove("known_models_openai")
            .remove("known_models_openrouter")
            .remove("known_models_sanitized")
            .apply()
        // Per-session-type extension profiles are GONE (2026-07-25) -- see the note in
        // ConnectionManager. Goose defines exactly two scopes (global config.yaml, and
        // session-scoped add/remove); a third layer keyed on session type just gave "what tools does
        // this chat have" a third owner. Drop its keys so they don't linger in prefs forever.
        // The per-voice-turn model override is GONE (2026-08-01) -- it was a third place to set
        // a model, kept only to dodge self-hosted latency, and the default chat model is a fast
        // cloud one now. Clear it so an upgraded install cannot keep an override with no UI.
        cfg.edit().remove("voice_provider").remove("voice_model")
            // The Assistant thread's own approval policy is gone with it (2026-08-01): goose's
            // mode is the only permission control now, set in the thread like any other chat.
            .remove("assistant_actions").apply()
        cfg.edit().apply {
            for (k in listOf("assistant", "chat", "code")) {
                remove("profile_${k}_on")
                remove("profile_${k}_ext")
            }
        }.apply()
    }

    var host: String
        get() = cfg.getString("host", "192.168.1.") ?: "192.168.1."
        set(v) = cfg.edit().putString("host", v).apply()

    var port: String
        // The app is TLS-only; 443 is the wss default and the reachable port on a
        // MagicDNS name. 3284 is the PLAIN goose port — TLS to it never succeeds,
        // and defaulting to it broke "host + key, no port" setups.
        get() = cfg.getString("port", "443") ?: "443"
        set(v) = cfg.edit().putString("port", v).apply()

    var dynamicColor: Boolean
        get() = cfg.getBoolean("dynamic_color", true)
        set(v) = cfg.edit().putBoolean("dynamic_color", v).apply()

    /** Gate app open / stored-key reconnect behind a biometric (or device-credential) prompt.
     *  Default OFF — the transport is tailnet-only and the lock is friction most users don't
     *  want; opt in from Settings › Security. Only takes effect when the device actually has an
     *  authenticator enrolled (Biometric.available). */
    var biometricLock: Boolean
        get() = cfg.getBoolean("biometric_lock", false)
        set(v) = cfg.edit().putBoolean("biometric_lock", v).apply()

    /** Keep a foreground connection alive even when idle (opt-in; costs battery). */
    var persistentConnection: Boolean
        get() = cfg.getBoolean("persistent_conn", false)
        set(v) = cfg.edit().putBoolean("persistent_conn", v).apply()

    /** Show goose's full provider catalog vs. just the configured ones. */
    var showAllProviders: Boolean
        get() = cfg.getBoolean("show_all_providers", false)
        set(v) = cfg.edit().putBoolean("show_all_providers", v).apply()

    /** Convert an attached image to a text description before sending it, instead of putting the
     *  image itself in the prompt.
     *
     *  OFF by default, because the imagegate proxy on the server now does exactly this for every
     *  request that passes through it -- for this app, for Goose Desktop, and for the scheduled
     *  recipes alike, which one client doing it for itself cannot. Turn this on only when
     *  talking to a goose whose traffic does NOT go through that proxy; with the proxy in place
     *  it just does the same work a second time, more slowly, before the message is even sent. */
    /** Absolute directory new sessions are created in. goose validates that session/new's cwd
     *  is absolute and refuses anything else, and it has no notion of a per-user default, so
     *  this is asked for at connect time rather than guessed. */
    var workingDir: String
        get() = cfg.getString("working_dir", "") ?: ""
        set(v) = cfg.edit().putString("working_dir", v.trim().trimEnd('/')).apply()

    /** Last opened session, so a notification reply after process death can resume it. */
    var lastSessionId: String?
        get() = cfg.getString("last_session", null)
        set(v) = cfg.edit().putString("last_session", v).apply()

    /** Last `_goose/unstable/sources/list` projects reply (raw JSON), so the drawer
     *  renders project names before the first live fetch on cold start. */
    var projectsCache: String
        get() = cfg.getString("projects_cache", "") ?: ""
        set(v) = cfg.edit().putString("projects_cache", v).apply()

    /** Last known cwd PER session id, merged from every session/list and every open. The resume
     *  path MUST hand session/load the session's REAL cwd — a wrong value silently REWRITES the
     *  session's working_dir server-side. The old global last_session_cwd fallback ("wrong here
     *  just means a stale guess, never a crash") did exactly that on 2026-07-27: a cold-start
     *  assistant open inherited the previous chat's project cwd and re-homed the assistant
     *  thread into /workspace/Cooking. Newline-delimited "id<TAB>cwd", newest first, capped. */
    fun sessionCwd(id: String): String? =
        (cfg.getString("session_cwds", "") ?: "").split("\n")
            .firstOrNull { it.substringBefore("\t") == id }
            ?.substringAfter("\t")?.takeIf { it.isNotBlank() }

    fun rememberSessionCwds(entries: List<Pair<String, String>>) {
        val fresh = entries.filter { it.first.isNotBlank() && it.second.isNotBlank() }
        if (fresh.isEmpty()) return
        val freshIds = fresh.map { it.first }.toSet()
        val kept = (cfg.getString("session_cwds", "") ?: "").split("\n")
            .filter { it.isNotBlank() && it.substringBefore("\t") !in freshIds }
        val next = (fresh.map { "${it.first}\t${it.second}" } + kept).take(200)
        cfg.edit().putString("session_cwds", next.joinToString("\n")).apply()
    }

    // recentWorkspaceProjects was REMOVED 2026-08-01 along with the drawer that read it. Projects
    // now come from the server (sources/list); a locally-remembered list of typed names could
    // only ever drift from it, and did -- "Media" survived its sessions, its directory and its
    // server entry because this cache still held the string. The stale preference key is left on
    // device deliberately: nothing reads it, and clearing it would be a migration for no gain.

    // --- Server-side speech (LocalAI) ---------------------------------------------------
    // Android's own SpeechRecognizer/TextToSpeech stay the default: no network, streaming partials,
    // works offline. These opt into the box's own models instead -- Kokoro sounds far better than
    // the stock Android voice, and Whisper transcribes better than on-device recognition. LocalAI
    // publishes 0.0.0.0:8080 so the phone reaches it directly; goose is not in this path.
    /** Models the user has confirmed DO accept images, by sending anyway past the warning.
     *  isLikelyVisionModel() is a substring heuristic over model names and cannot be right in
     *  general -- it missed Qwen3.6-35B-A3B, which is vision-capable via its mmproj, and every
     *  model rename invalidates it again. So the user's own answer is recorded and wins. */
    fun visionOk(model: String): Boolean =
        model.isNotBlank() && model in (cfg.getStringSet("vision_ok", emptySet()) ?: emptySet())
    fun markVisionOk(model: String) {
        if (model.isBlank()) return
        val next = HashSet(cfg.getStringSet("vision_ok", emptySet()) ?: emptySet()); next.add(model)
        cfg.edit().putStringSet("vision_ok", next).apply()
    }

    /** When the last proactive briefing push arrived (epoch millis) — shown on the Assistant status. */
    var lastBriefingAt: Long
        get() = cfg.getLong("last_briefing_at", 0L)
        set(v) = cfg.edit().putLong("last_briefing_at", v).apply()

    /** The text of the last proactive briefing push — the day's headline, shown on the status card. */
    var lastBriefingText: String
        get() = cfg.getString("last_briefing_text", "") ?: ""
        set(v) = cfg.edit().putString("last_briefing_text", v).apply()

    // --- UnifiedPush ---
    /** Whether the user has turned push on (drives register/refresh). */
    var pushEnabled: Boolean
        get() = cfg.getBoolean("push_on", false)
        set(v) = cfg.edit().putBoolean("push_on", v).apply()

    /** The distributor-issued endpoint URL the server POSTs to. Set by GoosePushService. */
    var pushEndpoint: String
        get() = cfg.getString("push_endpoint", "") ?: ""
        set(v) = cfg.edit().putString("push_endpoint", v).apply()

    /** Optional external registry to POST the endpoint to on rotation. Blank = disabled. */
    var pushRegistryUrl: String
        get() = cfg.getString("push_registry", "") ?: ""
        set(v) = cfg.edit().putString("push_registry", v).apply()

    /** The session id this device is awaiting a finished-turn nudge for (turn-push filtering). */
    var pendingPushSessionId: String?
        get() = cfg.getString("pending_push_session", null)
        set(v) = cfg.edit().putString("pending_push_session", v).apply()

    /** Whether the one-time "this is your assistant" hint has been dismissed. */
    var assistantHintSeen: Boolean
        get() = cfg.getBoolean("assistant_hint_seen", false)
        set(v) = cfg.edit().putBoolean("assistant_hint_seen", v).apply()

    /** Cached id of the goose-assistant thread so the app can land on it directly at startup. */
    var assistantSessionId: String?
        get() = cfg.getString("assistant_session", null)
        set(v) = cfg.edit().putString("assistant_session", v).apply()

    /** How the privileged Assistant thread handles tool actions: confirm | auto | readonly. */
    /** Master switch for every assistant feature — the drawer entry, landing on the thread at
     *  startup, and the Assistant settings screen. OFF BY DEFAULT, deliberately.
     *
     *  The "Assistant" is one persistent thread named by ASSISTANT_TITLE, kept fed by scheduled
     *  recipes that run server-side. That is a setup a particular server has, not something a
     *  stock `goose serve` provides: on a server without those jobs the drawer entry opens a
     *  thread nothing ever writes to, and the briefing-health indicator reads permanently stale.
     *  Defaulting it off means a fresh install against any goose server is a plain chat client
     *  and nothing is broken-looking; turning it on is an explicit choice by someone who knows
     *  they have the jobs. */
    var assistantEnabled: Boolean
        get() = cfg.getBoolean("assistant_enabled", false)
        set(v) = cfg.edit().putBoolean("assistant_enabled", v).apply()


    /** Real model slugs we've seen active, scoped PER PROVIDER (goose hides non-featured models
     *  like z-ai/glm-5.2). Provider-scoping stops LocalAI models leaking into the OpenRouter list
     *  and vice-versa. */

    fun savedOptions(ids: List<String>): Map<String, String> =
        ids.mapNotNull { id -> cfg.getString("opt_$id", null)?.let { id to it } }.toMap()

    fun saveOption(id: String, value: String) = cfg.edit().putString("opt_$id", value).apply()

    var secretKey: String
        get() = secure.getString("key", "") ?: ""
        set(v) = secure.edit().putString("key", v).apply()

    fun hasKey(): Boolean = secretKey.isNotBlank()

    // ---- Roam (direct iroh pairing) ----
    // The iroh secret key is as sensitive as the X-Secret-Key: it IS the
    // device's identity to every paired host, so it lives in the encrypted
    // store. Peers' cards are public key material (meant to be shared), so
    // they sit in plain prefs as name -> ConnectionCard-encode string.

    var roamIdentity: String?
        get() = secure.getString("roam_identity", null)
        set(v) { secure.edit().putString("roam_identity", v).apply() }

    var roamPeers: Map<String, String>
        get() = runCatching {
            org.json.JSONObject(cfg.getString("roam_peers", "{}") ?: "{}")
                .let { o -> o.keys().asSequence().associateWith { o.getString(it) } }
        }.getOrDefault(emptyMap())
        set(v) = cfg.edit().putString("roam_peers",
            org.json.JSONObject(v).toString()).apply()
}
