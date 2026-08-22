// SPDX-License-Identifier: AGPL-3.0-or-later

#include "corebridge.h"

#include "manager.h"

#include <QCoreApplication>
#include <QDebug>
#include <QLibrary>
#include <QMetaObject>
#include <QObject>

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
    RESOLVE(grouse_core_create);
    RESOLVE(grouse_core_free);
    RESOLVE(grouse_string_free);
    RESOLVE(grouse_connect);
    RESOLVE(grouse_disconnect);
    RESOLVE(grouse_new_session);
    RESOLVE(grouse_open_session);
    RESOLVE(grouse_list_sessions);
    RESOLVE(grouse_load_cached_transcript);
    RESOLVE(grouse_flush_caches);
    RESOLVE(grouse_send_prompt);
    RESOLVE(grouse_cancel);
    RESOLVE(grouse_set_config_option);
    RESOLVE(grouse_rename_session);
    RESOLVE(grouse_archive_session);
    RESOLVE(grouse_unarchive_session);
    RESOLVE(grouse_delete_session);
    RESOLVE(grouse_respond_permission);
    RESOLVE(grouse_status);
    RESOLVE(grouse_ready);
    RESOLVE(grouse_active_session_id);
    RESOLVE(grouse_sessions);
    RESOLVE(grouse_transcript);
    RESOLVE(grouse_config);
    RESOLVE(grouse_roam_connect);
    RESOLVE(grouse_roam_disconnect);
    RESOLVE(grouse_roam_open_session);
    RESOLVE(grouse_roam_new_session);
    RESOLVE(grouse_roam_new_session_cwd);
    RESOLVE(grouse_identity_generate);
    RESOLVE(grouse_identity_public_key);
    RESOLVE(grouse_card_fingerprint);
    RESOLVE(grouse_unstable_steer);
    RESOLVE(grouse_unstable_export_session);
    RESOLVE(grouse_unstable_session_info);
    RESOLVE(grouse_unstable_session_project);
    RESOLVE(grouse_unstable_list_tools);
    RESOLVE(grouse_unstable_session_extensions_list);
    RESOLVE(grouse_unstable_session_extensions_add);
    RESOLVE(grouse_unstable_session_extensions_remove);
    RESOLVE(grouse_unstable_list_global_extensions);
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
    RESOLVE(grouse_unstable_resources_read);
    RESOLVE(grouse_unstable_respond_recipe_params);
    RESOLVE(grouse_unstable_respond_elicitation);
#undef RESOLVE

    // The core must export every stable+unstable intent we depend on. If any
    // resolved NULL, the core is not the expected version — refuse to proceed
    // so failures are loud rather than segfaults on a NULL call later.
    static_assert(sizeof(AimsApi) > 0, "AimsApi must be non-empty");
    auto check = [&](auto *p) { return p != nullptr; };
    const bool ok = check(a.grouse_core_create) && check(a.grouse_core_free)
        && check(a.grouse_string_free) && check(a.grouse_connect)
        && check(a.grouse_send_prompt) && check(a.grouse_new_session)
        && check(a.grouse_open_session) && check(a.grouse_list_sessions)
        && check(a.grouse_status) && check(a.grouse_ready);
    if (!ok) {
        qWarning() << "CoreBridge: core library is missing required grouse_* symbols";
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
    QObject *target = bridge->target();
    if (!target)
        return;
    QMetaObject::invokeMethod(target, [target, captured = std::forward<Fn>(fn)]() mutable {
        std::move(captured)(static_cast<Manager *>(target));
    }, Qt::QueuedConnection);
}

extern "C" {

static void cb_on_status(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnStatus(s); }); }
static void cb_on_sessions(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnSessions(s); }); }
static void cb_on_transcript(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnTranscript(s); }); }
static void cb_on_stream(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnStream(s); }); }
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
static void cb_on_export(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnExport(s); }); }
static void cb_on_recipe_params(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnRecipeParams(s); }); }
static void cb_on_elicitation(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnElicitation(s); }); }
static void cb_on_compaction_status(void *u, const char *json) { const QString s = copyStr(json); marshal(u, nullptr, [=](Manager *m) { m->coreOnCompactionStatus(s); }); }
static void cb_on_message_usage(void *u, std::uint64_t a, std::uint64_t b, std::uint64_t c, double d) {
    marshal(u, nullptr, [a, b, c, d](Manager *m) { m->coreOnMessageUsage(a, b, c, d); });
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
        cb_on_transcript,
        cb_on_stream,
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
    };
    m_listener = &table;

    if (m_handle)
        return;   // already installed
    if (!resolve())
        return;

    char *err = nullptr;
    void *h = m_api.grouse_core_create(&table, this, nullptr, &err);
    if (err)
        m_api.grouse_string_free(err);
    m_handle = h;
    if (!m_handle)
        qWarning() << "CoreBridge: grouse_core_create failed:" << (err ? err : "unknown");
}
