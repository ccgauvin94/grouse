// SPDX-License-Identifier: AGPL-3.0-or-later

#pragma once

#include <QString>

#include <cstdint>

class QObject;

/**
 * CoreBridge — the ONLY path from QML to the wire (the thin client).
 *
 * dlopens `libgrouse_core.so` (the grouse-core cdylib built by
 * `cargo build --release -p grouse-core`) and resolves every `grouse_*`
 * symbol the C ABI exports. The core owns the ACP connection, the transcript,
 * streaming, reconnect and the roam transport. This class is deliberately NOT
 * a QObject: it is a process-scoped singleton that
 *
 *   (a) resolves the exact `extern "C"` symbols (matching core/grouse-core/
 *       src/capi.rs) and exposes them as a typed `AimsApi` table, and
 *   (b) installs the static `GrouseCoreListener` callback table, whose
 *       entries run on the core's worker thread and marshal every event onto
 *       the Qt main thread (queued QMetaObject::invokeMethod on the Manager),
 *       then dispatch to the Manager's bridge handlers — so NO QObject or UI
 *       state is ever touched off-main.
 *
 * Search order for the shared library (copied verbatim from the deleted
 * RoamTransport): env `GROUSE_CORE` -> <appdir>/../lib -> /app/lib -> system.
 * The core is dlopen'd; the CMake target does NOT link it.
 */
class CoreBridge
{
public:
    /// Every `grouse_*` symbol exported by capi.rs, as a plain C function
    /// table. Strings are malloc'd UTF-8 owned by the core; free with
    /// `grouse_string_free` (`takeString` does this).
    struct AimsApi {
        // lifecycle / stable intents
        void *(*grouse_core_create)(const void *listener, void *user_data,
                                    const char *cache_dir, char **out_err);
        void (*grouse_core_free)(void *h);
        void (*grouse_string_free)(char *s);
        void (*grouse_connect)(void *h, const char *config_json, char **out_err);
        void (*grouse_disconnect)(void *h);
        void (*grouse_new_session)(void *h, const char *recipe_id, char **out_err);
        void (*grouse_open_session)(void *h, const char *session_id);
        void (*grouse_list_sessions)(void *h);
        void (*grouse_load_cached_transcript)(void *h, const char *session_id);
        void (*grouse_flush_caches)(void *h);
        void (*grouse_send_prompt)(void *h, const char *prompt_json,
                                   const char *expect_json, char **out_err);
        void (*grouse_cancel)(void *h);
        void (*grouse_set_config_option)(void *h, const char *config_id,
                                         const char *value, char **out_err);
        void (*grouse_rename_session)(void *h, const char *id, const char *title);
        void (*grouse_archive_session)(void *h, const char *id);
        void (*grouse_unarchive_session)(void *h, const char *id);
        void (*grouse_delete_session)(void *h, const char *id);
        void (*grouse_respond_permission)(void *h, const char *tool_call_id,
                                          const char *outcome_json);
        // stable getters
        char *(*grouse_status)(void *h);
        int (*grouse_ready)(void *h);
        char *(*grouse_active_session_id)(void *h);
        char *(*grouse_sessions)(void *h);
        char *(*grouse_transcript)(void *h);
        char *(*grouse_config)(void *h);
        // roam
        void (*grouse_roam_connect)(void *h, const char *card, const char *label);
        void (*grouse_roam_disconnect)(void *h, const char *label);
        void (*grouse_roam_open_session)(void *h, const char *label, const char *session_id);
        void (*grouse_roam_new_session)(void *h, const char *label);
        void (*grouse_roam_new_session_cwd)(void *h, const char *label, const char *cwd);
        char *(*grouse_identity_generate)(void);
        char *(*grouse_identity_public_key)(const char *secret, char **out_err);
        char *(*grouse_card_fingerprint)(const char *card, char **out_err);
        void (*grouse_set_roam_identity)(void *h, const char *secret);
        // unstable intents
        void (*grouse_unstable_steer)(void *h, const char *text, const char *expected_run_id);
        void (*grouse_unstable_export_session)(void *h, const char *session_id);
        void (*grouse_unstable_session_info)(void *h, const char *id);
        void (*grouse_unstable_session_project)(void *h, const char *id, const char *project_id);
        void (*grouse_unstable_list_tools)(void *h, const char *session_id);
        void (*grouse_unstable_session_extensions_list)(void *h, const char *sid);
        void (*grouse_unstable_session_extensions_add)(void *h, const char *sid, const char *extension);
        void (*grouse_unstable_session_extensions_remove)(void *h, const char *sid, const char *name);
        void (*grouse_unstable_list_global_extensions)(void *h);
        void (*grouse_unstable_set_extension_enabled)(void *h, const char *name, int enabled);
        void (*grouse_unstable_add_extension)(void *h, const char *extension, int enabled);
        void (*grouse_unstable_sources_list)(void *h, const char *source_type);
        void (*grouse_unstable_sources_create)(void *h, const char *source_type,
                                               const char *name, const char *description,
                                               const char *content);
        void (*grouse_unstable_sources_delete)(void *h, const char *source_type, const char *path);
        void (*grouse_unstable_sources_update)(void *h, const char *source_type, const char *path,
                                               const char *name, const char *description,
                                               const char *content);
        void (*grouse_unstable_config_read)(void *h, const char *key);
        void (*grouse_unstable_config_upsert)(void *h, const char *key, const char *value);
        void (*grouse_unstable_supported_models)(void *h, const char *provider_id);
        void (*grouse_unstable_providers_list)(void *h);
        void (*grouse_unstable_recipes_list)(void *h);
        void (*grouse_unstable_recipes_schedule)(void *h, const char *id, const char *cron_schedule);
        void (*grouse_unstable_recipes_save)(void *h, const char *id, const char *recipe);
        void (*grouse_unstable_recipes_delete)(void *h, const char *id);
        void (*grouse_unstable_schedules_list)(void *h);
        void (*grouse_unstable_schedules_pause)(void *h, const char *schedule_id);
        void (*grouse_unstable_schedules_unpause)(void *h, const char *schedule_id);
        void (*grouse_unstable_schedules_run_now)(void *h, const char *schedule_id);
        void (*grouse_unstable_schedules_delete)(void *h, const char *schedule_id);
        void (*grouse_unstable_schedules_update)(void *h, const char *schedule_id, const char *cron);
        void (*grouse_unstable_working_dir_update)(void *h, const char *sid, const char *dir);
        void (*grouse_unstable_tools_call)(void *h, const char *sid, const char *name, const char *args_json);
        void (*grouse_unstable_resources_read)(void *h, const char *sid, const char *uri, const char *ext);
        void (*grouse_unstable_respond_recipe_params)(void *h, const char *action, const char *values_json);
        void (*grouse_unstable_respond_elicitation)(void *h, const char *action, const char *content_json);
    };

    /// Process-scoped singleton. First call performs the dlopen + symbol
    /// resolve; returns nullptr if the core could not be loaded. A test may
    /// substitute a stub via setInstanceForTesting() BEFORE first use.
    static CoreBridge *instance();

    /// Test seam: install an alternate CoreBridge (subclass) in place of the
    /// dlopen singleton. Must be called before any CoreBridge::instance() user
    /// (e.g. before constructing a Manager) and only for the lifetime of the
    /// test. Pass nullptr to restore the real bridge.
    static void setInstanceForTesting(CoreBridge *bridge);

    /// The resolved C function table (never null after a successful load).
    virtual const AimsApi &api() const { return m_api; }

    /// The opaque core handle returned by `grouse_core_create`.
    virtual void *handle() const { return m_handle; }
    /** The low-level listener table; used together with handle(). */
    virtual const void *listener() const { return m_listener; }

    /// True when the core library resolved and a Core handle was created.
    virtual bool isAvailable() const { return m_handle != nullptr; }

    /// Set the QObject (the Manager) that bridge events are marshalled onto.
    /// All callbacks marshal to this object's thread via queued
    /// QMetaObject::invokeMethod; the object is used as the lifetime context,
    /// so pending events are dropped if it is destroyed.
    void setTarget(QObject *target) { m_target = target; }
    QObject *target() const { return m_target; }

    /// The opaque GrouseCoreListener table (reinterpret to the C struct).
    virtual void installListener();   // populates the static table and creates the core handle

    /// Take ownership of a malloc'd C string from the core, converting to
    /// QString and freeing via grouse_string_free.
    QString takeString(char *s) const;

protected:
    CoreBridge();
    ~CoreBridge();
    bool resolve();

    AimsApi m_api{};
    void *m_handle = nullptr;
    const void *m_listener = nullptr;
    QObject *m_target = nullptr;
};

// ---------------------------------------------------------------------------
// The listener table (mirrors capi.rs `GrouseCoreListener` exactly).
// ---------------------------------------------------------------------------
extern "C" {
struct GrouseCoreListener {
    // -- stable --
    void (*on_status)(void *, const char *);
    void (*on_sessions)(void *, const char *);
    void (*on_transcript)(void *, const char *);
    void (*on_stream)(void *, const char *);
    void (*on_config)(void *, const char *);
    void (*on_permission_request)(void *, const char *);
    void (*on_session_touched)(void *, const char *, const char *, const char *);
    void (*on_projects)(void *, const char *);
    void (*on_roam_peer_status)(void *, const char *, const char *);
    void (*on_roam_sessions)(void *, const char *, const char *);
    void (*on_peer_new_session)(void *, const char *, const char *);
    void (*on_active_run)(void *, const char *, const char *);
    void (*on_commands)(void *, const char *);
    // -- unstable --
    void (*on_export)(void *, const char *);
    void (*on_recipe_params)(void *, const char *);
    void (*on_elicitation)(void *, const char *);
    void (*on_compaction_status)(void *, const char *);
    void (*on_message_usage)(void *, std::uint64_t, std::uint64_t, std::uint64_t, double);
    void (*on_app_resource)(void *, const char *, const char *);
    void (*on_recipes)(void *, const char *);
    void (*on_schedules)(void *, const char *);
    void (*on_unstable_projects)(void *, const char *);
    void (*on_skills)(void *, const char *);
    void (*on_tools)(void *, const char *, const char *);
    void (*on_extensions)(void *, const char *);
    void (*on_session_extensions)(void *, const char *, const char *);
    void (*on_config_value)(void *, const char *, const char *);
    void (*on_supported_models)(void *, const char *, const char *);
    void (*on_providers)(void *, const char *);
    void (*on_session_probe)(void *, const char *, const char *, long long);
    void (*on_tool_result)(void *, const char *, int);
    void (*on_error)(void *, const char *, const char *);
};
}
