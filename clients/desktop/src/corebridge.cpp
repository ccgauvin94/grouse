// SPDX-License-Identifier: AGPL-3.0-or-later

#include "corebridge.h"

#include "manager.h"

#include <QCoreApplication>
#include <QDebug>
#include <QLibrary>
#include <QMetaObject>
#include <QObject>
#include <QRunnable>
#include <QStringList>
#include <QThreadPool>

#include <functional>
#include <utility>

// ---------------------------------------------------------------------------
// Library discovery (copied verbatim from the deleted RoamTransport).
// Resolution order: dev override (GROUSE_CORE), then the app image
// (flatpak: /app/lib; native: <appdir>/../lib), then the system search path.
// ---------------------------------------------------------------------------

static QLibrary *loadLibrary()
{
    static QLibrary *lib = [] {
        QStringList candidates;
        const QString overridePath = qEnvironmentVariable("GROUSE_CORE");
        if (!overridePath.isEmpty())
            candidates << overridePath;
        candidates << QCoreApplication::applicationDirPath() + QStringLiteral("/../lib/libgrouse_core.so")
                   << QStringLiteral("/app/lib/libgrouse_core.so")
                   << QStringLiteral("grouse_core");
        QLibrary *resolved = nullptr;
        for (const QString &c : candidates) {
            auto *l = new QLibrary(c);
            if (l->load()) {
                resolved = l;
                break;
            }
            delete l;
        }
        return resolved;
    }();
    return lib;
}

namespace {
CoreBridge *g_testInstance = nullptr;

// Plain QRunnable, not QThreadPool::start(callable) — Qt 6.7+ only, and the
// Flatpak SDK / dev distrobox do not have to agree on a version.
class CoreLambdaTask : public QRunnable
{
public:
    explicit CoreLambdaTask(std::function<void()> fn) : m_fn(std::move(fn)) {}
    void run() override { if (m_fn) m_fn(); }

private:
    std::function<void()> m_fn;
};
} // namespace

CoreBridge *CoreBridge::instance()
{
    // A test-installed stub wins; otherwise the real dlopen singleton.
    if (g_testInstance)
        return g_testInstance;
    static CoreBridge bridge;
    return &bridge;
}

void CoreBridge::setInstanceForTesting(CoreBridge *bridge)
{
    g_testInstance = bridge;
}

CoreBridge::CoreBridge() = default;
CoreBridge::~CoreBridge()
{
    if (m_handle && m_api.grouse_core_free)
        m_api.grouse_core_free(m_handle);
}

bool CoreBridge::resolve()
{
    QLibrary *lib = loadLibrary();
    if (!lib) {
        qWarning() << "CoreBridge: could not load libgrouse_core.so";
        return false;
    }
    AimsApi a{};
#define RESOLVE(name) do { a.name = reinterpret_cast<decltype(a.name)>(lib->resolve(#name)); } while (0)
    RESOLVE(grouse_abi_version);
    RESOLVE(grouse_core_create);
    RESOLVE(grouse_core_free);
    RESOLVE(grouse_string_free);
    RESOLVE(grouse_push_parse);
    RESOLVE(grouse_push_decide);
    RESOLVE(grouse_push_decide_run);
    RESOLVE(grouse_connect);
    RESOLVE(grouse_connect_attach);
    RESOLVE(grouse_disconnect);
    RESOLVE(grouse_new_session);
    RESOLVE(grouse_new_session_with_mode);
    RESOLVE(grouse_open_session);
    RESOLVE(grouse_refresh_session);
    RESOLVE(grouse_list_sessions);
    RESOLVE(grouse_load_cached_transcript);
    RESOLVE(grouse_flush_caches);
    RESOLVE(grouse_send_prompt);
    RESOLVE(grouse_send_prompt_with_paths);
    RESOLVE(grouse_fetch_file);
    RESOLVE(grouse_cancel);
    RESOLVE(grouse_set_config_option);
    RESOLVE(grouse_rename_session);
    RESOLVE(grouse_open_thread);
    RESOLVE(grouse_current_thread_label);
    RESOLVE(grouse_archive_session);
    RESOLVE(grouse_unarchive_session);
    RESOLVE(grouse_delete_session);
    RESOLVE(grouse_respond_permission);
    RESOLVE(grouse_status);
    RESOLVE(grouse_ready);
    RESOLVE(grouse_connected);
    RESOLVE(grouse_set_run_summaries);
    RESOLVE(grouse_set_image_rescale);
    RESOLVE(grouse_image_rescale);
    RESOLVE(grouse_tasks_refresh);
    // Optional (see AimsApi): resolved best-effort, and NOT part of the
    // required-symbols check below — an older core still loads and simply never
    // paints a cached board, which is the behaviour every client had before.
    RESOLVE(grouse_tasks_paint);
    RESOLVE(grouse_activity);
    RESOLVE(grouse_task_close);
    RESOLVE(grouse_task_snooze);
    RESOLVE(grouse_task_react);
    RESOLVE(grouse_assistant_project);
    RESOLVE(grouse_run_summaries);
    RESOLVE(grouse_active_session_id);
    RESOLVE(grouse_sessions);
    RESOLVE(grouse_session_owner);
    RESOLVE(grouse_all_sessions);
    RESOLVE(grouse_transcript);
    RESOLVE(grouse_transcript_rich);
    RESOLVE(grouse_item);
    RESOLVE(grouse_window);
    RESOLVE(grouse_load_older);
    RESOLVE(grouse_config);
    RESOLVE(grouse_turn_log);
    RESOLVE(grouse_turn_diag);
    RESOLVE(grouse_roam_connect);
    RESOLVE(grouse_roam_disconnect);
    RESOLVE(grouse_roam_open_session);
    RESOLVE(grouse_roam_new_session);
    RESOLVE(grouse_roam_new_session_cwd);
    RESOLVE(grouse_roam_new_session_with_mode);
    RESOLVE(grouse_identity_generate);
    RESOLVE(grouse_identity_public_key);
    RESOLVE(grouse_card_fingerprint);
    RESOLVE(grouse_set_roam_identity);
    RESOLVE(grouse_unstable_steer);
    RESOLVE(grouse_unstable_export_session);
    RESOLVE(grouse_unstable_session_info);
    RESOLVE(grouse_unstable_session_project);
    RESOLVE(grouse_unstable_list_tools);
    RESOLVE(grouse_unstable_session_extensions_list);
    RESOLVE(grouse_unstable_session_extensions_add);
    RESOLVE(grouse_unstable_session_extensions_remove);
    RESOLVE(grouse_unstable_list_global_extensions);
    RESOLVE(grouse_unstable_extension_catalog);
    RESOLVE(grouse_unstable_set_extension_enabled);
    RESOLVE(grouse_unstable_add_extension);
    RESOLVE(grouse_unstable_sources_list);
    RESOLVE(grouse_unstable_sources_create);
    RESOLVE(grouse_unstable_sources_delete);
    RESOLVE(grouse_unstable_sources_update);
    RESOLVE(grouse_unstable_config_read);
    RESOLVE(grouse_unstable_config_upsert);
    RESOLVE(grouse_unstable_supported_models);
    RESOLVE(grouse_unstable_providers_list);
    RESOLVE(grouse_unstable_recipes_list);
    RESOLVE(grouse_unstable_recipes_schedule);
    RESOLVE(grouse_unstable_recipes_save);
    RESOLVE(grouse_unstable_recipes_delete);
    RESOLVE(grouse_unstable_schedules_list);
    RESOLVE(grouse_unstable_schedules_pause);
    RESOLVE(grouse_unstable_schedules_unpause);
    RESOLVE(grouse_unstable_schedules_run_now);
    RESOLVE(grouse_unstable_schedules_delete);
    RESOLVE(grouse_unstable_schedules_update);
    RESOLVE(grouse_unstable_working_dir_update);
    RESOLVE(grouse_unstable_tools_call);
    RESOLVE(grouse_unstable_tools_call_named);
    RESOLVE(grouse_unstable_resources_read);
    RESOLVE(grouse_unstable_respond_recipe_params);
    RESOLVE(grouse_unstable_respond_elicitation);
    RESOLVE(grouse_unstable_context_report);
    RESOLVE(grouse_set_advisor);
    RESOLVE(grouse_advisor);
#undef RESOLVE

    // The listener table's layout and the symbol signatures are the contract.
    // An older core (or a stale GROUSE_CORE) is refused before any callback can
    // be invoked with the wrong arity.
    // ABI 19 changed only the JSON record shapes (`armed_owner` in NotifyContext,
    // `reply_session` in NotifyDecision) — no symbol signature or listener-table
    // layout moved — but a desktop built for 19 must refuse an older .so: its
    // decisions have no verified Reply target. PushClient reads `reply_session`.
    static constexpr unsigned kExpectedAbiVersion = 19;
    if (!a.grouse_abi_version) {
        qWarning() << "CoreBridge: core library predates the ABI version symbol; refusing it";
        return false;
    }
    if (const unsigned got = a.grouse_abi_version(); got != kExpectedAbiVersion) {
        qWarning() << "CoreBridge: core ABI" << got << "does not match the app's"
                   << kExpectedAbiVersion << "; refusing it";
        return false;
    }
    // EVERY symbol in AimsApi is required by ABI 19. Checking only a handful
    // left an otherwise accepted .so one NULL call away from a crash (the first
    // user of an unchecked Q_INVOKABLE). The size assertion makes a future
    // member without a REQUIRE line a build error instead of a runtime gap.
    static_assert(sizeof(AimsApi) / sizeof(void (*)()) == 104,
                  "AimsApi gained a symbol: add it to the resolve + REQUIRE lists");
    QStringList missing;
#define REQUIRE(name) do { if (!a.name) missing << QStringLiteral(#name); } while (0)
    REQUIRE(grouse_abi_version) REQUIRE(grouse_core_create) REQUIRE(grouse_core_free) REQUIRE(grouse_string_free);
    REQUIRE(grouse_push_parse) REQUIRE(grouse_push_decide) REQUIRE(grouse_push_decide_run) REQUIRE(grouse_connect);
    REQUIRE(grouse_connect_attach) REQUIRE(grouse_disconnect) REQUIRE(grouse_new_session) REQUIRE(grouse_new_session_with_mode);
    REQUIRE(grouse_open_session) REQUIRE(grouse_refresh_session) REQUIRE(grouse_list_sessions) REQUIRE(grouse_load_cached_transcript);
    REQUIRE(grouse_flush_caches) REQUIRE(grouse_send_prompt) REQUIRE(grouse_send_prompt_with_paths) REQUIRE(grouse_fetch_file);
    REQUIRE(grouse_cancel) REQUIRE(grouse_set_config_option) REQUIRE(grouse_rename_session) REQUIRE(grouse_open_thread);
    REQUIRE(grouse_current_thread_label) REQUIRE(grouse_archive_session) REQUIRE(grouse_unarchive_session) REQUIRE(grouse_delete_session);
    REQUIRE(grouse_respond_permission) REQUIRE(grouse_status) REQUIRE(grouse_ready) REQUIRE(grouse_connected);
    REQUIRE(grouse_set_run_summaries) REQUIRE(grouse_set_image_rescale) REQUIRE(grouse_image_rescale) REQUIRE(grouse_tasks_refresh);
    REQUIRE(grouse_activity) REQUIRE(grouse_tasks_paint) REQUIRE(grouse_task_close) REQUIRE(grouse_task_snooze);
    REQUIRE(grouse_task_react) REQUIRE(grouse_assistant_project) REQUIRE(grouse_run_summaries) REQUIRE(grouse_active_session_id);
    REQUIRE(grouse_sessions) REQUIRE(grouse_session_owner) REQUIRE(grouse_all_sessions) REQUIRE(grouse_transcript);
    REQUIRE(grouse_transcript_rich) REQUIRE(grouse_item);
    REQUIRE(grouse_window) REQUIRE(grouse_load_older) REQUIRE(grouse_config) REQUIRE(grouse_turn_log);
    REQUIRE(grouse_turn_diag) REQUIRE(grouse_roam_connect) REQUIRE(grouse_roam_disconnect) REQUIRE(grouse_roam_open_session);
    REQUIRE(grouse_roam_new_session) REQUIRE(grouse_roam_new_session_cwd) REQUIRE(grouse_roam_new_session_with_mode) REQUIRE(grouse_identity_generate);
    REQUIRE(grouse_identity_public_key) REQUIRE(grouse_card_fingerprint) REQUIRE(grouse_set_roam_identity) REQUIRE(grouse_unstable_steer);
    REQUIRE(grouse_unstable_export_session) REQUIRE(grouse_unstable_session_info) REQUIRE(grouse_unstable_session_project) REQUIRE(grouse_unstable_list_tools);
    REQUIRE(grouse_unstable_session_extensions_list) REQUIRE(grouse_unstable_session_extensions_add) REQUIRE(grouse_unstable_session_extensions_remove) REQUIRE(grouse_unstable_list_global_extensions);
    REQUIRE(grouse_unstable_extension_catalog) REQUIRE(grouse_unstable_set_extension_enabled) REQUIRE(grouse_unstable_add_extension) REQUIRE(grouse_unstable_sources_list);
    REQUIRE(grouse_unstable_sources_create) REQUIRE(grouse_unstable_sources_delete) REQUIRE(grouse_unstable_sources_update) REQUIRE(grouse_unstable_config_read);
    REQUIRE(grouse_unstable_config_upsert) REQUIRE(grouse_unstable_supported_models) REQUIRE(grouse_unstable_providers_list) REQUIRE(grouse_unstable_recipes_list);
    REQUIRE(grouse_unstable_recipes_schedule) REQUIRE(grouse_unstable_recipes_save) REQUIRE(grouse_unstable_recipes_delete) REQUIRE(grouse_unstable_schedules_list);
    REQUIRE(grouse_unstable_schedules_pause) REQUIRE(grouse_unstable_schedules_unpause) REQUIRE(grouse_unstable_schedules_run_now) REQUIRE(grouse_unstable_schedules_delete);
    REQUIRE(grouse_unstable_schedules_update) REQUIRE(grouse_unstable_working_dir_update) REQUIRE(grouse_unstable_tools_call) REQUIRE(grouse_unstable_tools_call_named);
    REQUIRE(grouse_unstable_resources_read) REQUIRE(grouse_unstable_respond_recipe_params) REQUIRE(grouse_unstable_respond_elicitation) REQUIRE(grouse_unstable_context_report);
    REQUIRE(grouse_set_advisor) REQUIRE(grouse_advisor);
#undef REQUIRE
    if (!missing.isEmpty()) {
        qWarning() << "CoreBridge: core library is missing required grouse_* symbols:"
                   << missing.join(QLatin1String(", "));
        return false;
    }
    m_api = a;
    return true;
}

QString CoreBridge::takeString(char *s) const
{
    if (!s)
        return QString();
    const QString out = QString::fromUtf8(s);
    if (m_api.grouse_string_free)
        m_api.grouse_string_free(s);
    return out;
}

// ---------------------------------------------------------------------------
// Listener table + worker->main marshalling.
//
// Every callback runs on the core's worker thread and MUST NOT touch Qt UI
// state. Each one copies its payloads into the marshalled functor and posts it
// (queued) onto the Manager's thread, using the Manager as the lifetime
// context — so Qt drops the event if the Manager is destroyed first.
// ---------------------------------------------------------------------------

namespace {

// Copy a C string into a QString (safe to do off-main; QString is implicitly
// shared and never touches UI state).
QString copyStr(const char *s)
{
    return QString::fromUtf8(s ? s : "");
}

} // namespace

template <typename Fn>
static void marshal(void *userData, QObject *kind, Fn &&fn)
{
    Q_UNUSED(kind);
    auto *bridge = static_cast<CoreBridge *>(userData);
    if (!bridge)
        return;
    // The bridge singleton outlives the Manager; the target is atomic because
    // ~Manager calls clearTarget() while core callbacks may still be in flight.
    QObject *target = bridge->target();
    if (!target)
        return;
    QMetaObject::invokeMethod(target, [bridge, target, captured = std::forward<Fn>(fn)]() mutable {
        // A callback queued just before shutdown must not touch a dying Manager.
        if (bridge->target() != target)
            return;
        std::move(captured)(static_cast<Manager *>(target));
    }, Qt::QueuedConnection);
}

extern "C" {

static void cb_on_status(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnStatus(s); }); }
static void cb_on_sessions(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnSessions(s); }); }
static void cb_on_item(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnItem(s); }); }
static void cb_on_usage(void *u, long long used, long long size, double cost, const char *currency) {
    const QString c = copyStr(currency);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnUsage(used, size, cost, c); });
}
static void cb_on_run_ended(void *u, const char *sessionId, const char *stop) {
    // The session id rides along: a completion must be routed to the chat that
    // owns the turn, not to whatever is on screen when it lands.
    const QString sid = copyStr(sessionId);
    const QString s = copyStr(stop);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnRunEnded(sid, s); });
}
static void cb_on_speak(void *u, const char *text) {
    // The core's sanitized, sentence-sized speakable text (docs/TRANSCRIPT_MODEL.md).
    // Accepted and dropped for now — the desktop TTS engine lands with the voice
    // workstream; the payload is only valid for this call, so nothing is retained.
    Q_UNUSED(u);
    Q_UNUSED(text);
}
static void cb_on_config(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnConfig(s); }); }
static void cb_on_permission(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnPermission(s); }); }
static void cb_on_session_touched(void *u, const char *a, const char *b, const char *c) {
    const QString sa = copyStr(a); const QString sb = copyStr(b); const QString sc = copyStr(c);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnSessionTouched(sa, sb, sc); });
}
static void cb_on_projects(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnProjects(s); }); }
static void cb_on_roam_peer_status(void *u, const char *a, const char *b) {
    const QString sa = copyStr(a); const QString sb = copyStr(b);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnRoamPeerStatus(sa, sb); });
}
static void cb_on_roam_sessions(void *u, const char *a, const char *b) {
    const QString sa = copyStr(a); const QString sb = copyStr(b);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnRoamSessions(sa, sb); });
}
static void cb_on_peer_new_session(void *u, const char *a, const char *b) {
    const QString sa = copyStr(a); const QString sb = copyStr(b);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnPeerNewSession(sa, sb); });
}
static void cb_on_active_run(void *u, const char *a, const char *b) {
    const QString sa = copyStr(a); const QString sb = copyStr(b);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnActiveRun(sa, sb); });
}
static void cb_on_commands(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnCommands(s); }); }
static void cb_on_session_open_failed(void *u, const char *sessionId, const char *message) {
    const QString sid = copyStr(sessionId);
    const QString msg = copyStr(message);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnSessionOpenFailed(sid, msg); });
}
static void cb_on_session_opened(void *u, const char *sessionId) {
    // Copy the payload inside the callback: the core frees the C string on return.
    const QString sid = copyStr(sessionId);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnSessionOpened(sid); });
}
static void cb_on_tasks(void *u, const char *json) {
    const QString s = copyStr(json);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnTasks(s); });
}
static void cb_on_thread_ready(void *u, const char *label, const char *sessionId) {
    const QString l = copyStr(label);
    const QString sid = copyStr(sessionId);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnThreadReady(l, sid); });
}
static void cb_on_replay_progress(void *u, const char *sessionId, long long loaded,
                                  long long total, int done) {
    const QString sid = copyStr(sessionId);
    marshal(u, nullptr, [=](Manager *m) {
        m->coreOnReplayProgress(sid, static_cast<int>(loaded), static_cast<int>(total), done != 0);
    });
}
static void cb_on_file(void *u, const char *json) {
    const QString s = copyStr(json);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnFile(s); });
}
static void cb_on_export(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnExport(s); }); }
static void cb_on_recipe_params(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnRecipeParams(s); }); }
static void cb_on_elicitation(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnElicitation(s); }); }
static void cb_on_compaction_status(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnCompactionStatus(s); }); }
static void cb_on_message_usage(void *u, std::uint64_t a, std::uint64_t b, std::uint64_t c, double d) {
    marshal(u, nullptr, [a, b, c, d](Manager *m) { m->coreOnMessageUsage(a, b, c, d); });
}
static void cb_on_unstable_usage(void *u, std::int64_t used, std::int64_t size, double cost) {
    marshal(u, nullptr, [used, size, cost](Manager *m) { m->coreOnUsage(used, size, cost, QStringLiteral("USD")); });
}
static void cb_on_app_resource(void *u, const char *a, const char *b) {
    const QString sa = copyStr(a); const QString sb = copyStr(b);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnAppResource(sa, sb); });
}
static void cb_on_recipes(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnRecipes(s); }); }
static void cb_on_schedules(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnSchedules(s); }); }
static void cb_on_unstable_projects(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnUnstableProjects(s); }); }
static void cb_on_skills(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnSkills(s); }); }
static void cb_on_tools(void *u, const char *a, const char *b) {
    const QString sa = copyStr(a); const QString sb = copyStr(b);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnTools(sa, sb); });
}
static void cb_on_extensions(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnExtensions(s); }); }
static void cb_on_session_extensions(void *u, const char *a, const char *b) {
    const QString sa = copyStr(a); const QString sb = copyStr(b);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnSessionExtensions(sa, sb); });
}
static void cb_on_extension_catalog(void *u, const char *a, const char *b) {
    const QString sa = copyStr(a); const QString sb = copyStr(b);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnExtensionCatalog(sa, sb); });
}
static void cb_on_advisor_report(void *u, const char *a, const char *b) {
    const QString sa = copyStr(a); const QString sb = copyStr(b);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnAdvisorReport(sa, sb); });
}
static void cb_on_context_report(void *u, const char *a, const char *b) {
    const QString sa = copyStr(a); const QString sb = copyStr(b);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnContextReport(sa, sb); });
}
static void cb_on_config_value(void *u, const char *a, const char *b) {
    const QString sa = copyStr(a); const QString sb = copyStr(b);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnConfigValue(sa, sb); });
}
static void cb_on_supported_models(void *u, const char *a, const char *b) {
    const QString sa = copyStr(a); const QString sb = copyStr(b);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnSupportedModels(sa, sb); });
}
static void cb_on_providers(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnProviders(s); }); }
static void cb_on_session_probe(void *u, const char *a, const char *b, long long n) {
    const QString sa = copyStr(a); const QString sb = copyStr(b);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnSessionProbe(sa, sb, n); });
}
static void cb_on_tool_result(void *u, const char *a, int b) {
    const QString sa = copyStr(a);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnToolResult(sa, b); });
}
static void cb_on_error(void *u, const char *a, const char *b) {
    const QString sa = copyStr(a); const QString sb = copyStr(b);
    marshal(u, nullptr, [=](Manager *m) { m->coreOnError(sa, sb); });
}

} // extern "C"

void CoreBridge::installListener()
{
    static GrouseCoreListener table = {
        cb_on_status,
        cb_on_sessions,
        cb_on_item,
        cb_on_usage,
        cb_on_run_ended,
        cb_on_speak,
        cb_on_config,
        cb_on_permission,
        cb_on_session_touched,
        cb_on_projects,
        cb_on_roam_peer_status,
        cb_on_roam_sessions,
        cb_on_peer_new_session,
        cb_on_active_run,
        cb_on_commands,
        cb_on_export,
        cb_on_recipe_params,
        cb_on_elicitation,
        cb_on_compaction_status,
        cb_on_message_usage,
        cb_on_unstable_usage,
        cb_on_app_resource,
        cb_on_recipes,
        cb_on_schedules,
        cb_on_unstable_projects,
        cb_on_skills,
        cb_on_tools,
        cb_on_extensions,
        cb_on_session_extensions,
        cb_on_config_value,
        cb_on_supported_models,
        cb_on_providers,
        cb_on_session_probe,
        cb_on_tool_result,
        cb_on_error,
        // Dictation is not wired on the desktop yet; the slots stay null so the
        // table is ABI-locked to the core's GrouseCoreListener.
        nullptr,   // on_dictation_config
        nullptr,   // on_transcript
        cb_on_session_open_failed,
        cb_on_tasks,
        cb_on_replay_progress,
        cb_on_file,
        cb_on_thread_ready,
        cb_on_extension_catalog,
        cb_on_context_report,
        // The area timeline and the surfacing knob are Android-side surfaces for
        // now (docs/PARITY.md marks both excluded here). The slots are carried
        // null so the table's LAYOUT still matches the core's with ABI 14 and 15:
        // the core reads this struct by its own definition, so a short table is an
        // out-of-bounds read the version gate exists to prevent.
        nullptr,   // on_thread_timeline (ABI 14)
        nullptr,   // on_sensitivity (ABI 15)
        cb_on_advisor_report,   // ABI 16
        cb_on_session_opened,   // ABI 18
    };
    m_listener = &table;

    if (m_handle)
        return;   // already installed
    if (!resolve())
        return;

    char *err = nullptr;
    void *h = m_api.grouse_core_create(&table, this, nullptr, &err);
    // Copy BEFORE freeing: the old qWarning read err after grouse_string_free.
    QString errText = takeString(err);
    if (errText.isEmpty())
        errText = QStringLiteral("unknown");
    m_handle = h;
    if (!m_handle)
        qWarning() << "CoreBridge: grouse_core_create failed:" << errText;
}

void CoreBridge::paintCachedTasksAsync() const
{
    if (!m_handle || !m_api.grouse_tasks_paint)
        return;
    // Copy the table and handle: the bridge outlives the Manager, but a task
    // queued now must not read members that could change under it.
    const AimsApi api = m_api;
    void *h = m_handle;
    QThreadPool::globalInstance()->start(new CoreLambdaTask([api, h] {
        api.grouse_tasks_paint(h);
    }));
}
