// SPDX-License-Identifier: AGPL-3.0-or-later

#include "catalogcontroller.h"
#include "manager.h"
#include "settingsstore.h"
#include "coreapi.h"

#include <QDebug>
#include <QDir>
#include <QFile>
#include <QJsonArray>
#include <QJsonDocument>
#include <QStandardPaths>
#include <QTimer>

#include <utility>

namespace {
QJsonArray parseArr(const QString &json)
{
    return QJsonDocument::fromJson(json.toUtf8()).array();
}
} // namespace

CatalogController::CatalogController(Manager &manager)
    : m_manager(manager)
    , m_bridge(manager.m_bridge)
    , m_currentSessionId(manager.m_currentSessionId)
    , m_activePeerLabel(manager.m_activePeerLabel)
    , m_pendingRecipeId(manager.m_pendingRecipeId)
    , m_pendingProjectFiling(manager.m_pendingProjectFiling)
    , m_discoveringExt(manager.m_discoveringExt)
    , m_discoveringFull(manager.m_discoveringFull)
    , m_discoveringAttached(manager.m_discoveringAttached)
    , m_tools(manager.m_tools)
    , m_configuredProviders(manager.m_configuredProviders)
    , m_recipes(manager.m_recipes)
    , m_schedules(manager.m_schedules)
    , m_skills(manager.m_skills)
    , m_supportedModels(manager.m_supportedModels)
    , m_serverConfig(manager.m_serverConfig)
    , m_gooseSecretKeys(manager.m_gooseSecretKeys)
    , m_extDefs(manager.m_extDefs)
    , m_catalogDefs(manager.m_catalogDefs)
    , m_sessionExts(manager.m_sessionExts)
    , m_sessionRestricted(manager.m_sessionRestricted)
    , m_peekQueue(manager.m_peekQueue)
    , m_peekFailed(manager.m_peekFailed)
    , m_peekTimer(manager.m_peekTimer)
    , m_toolCatalog(manager.m_toolCatalog)
    , m_prompting(manager.m_prompting)
    , m_compacting(manager.m_compacting)
    , m_activeRunId(manager.m_activeRunId)
{
}

void CatalogController::clearSessionState()
{
    m_tools.clear();
    m_extDefs.clear();
    m_catalogDefs.clear();
    m_sessionExts.clear();
    m_sessionRestricted.clear();
    m_peekQueue.clear();
    m_peekFailed.clear();
}

void CatalogController::timingLog(const QString &line) { CoreApi::timingLog(line); }
void CatalogController::newChat() { m_manager.newChat(); }
QByteArray CatalogController::toolCatalogs() const { return m_manager.m_settings.toolCatalogs(); }
void CatalogController::setToolCatalogs(const QByteArray &json) { m_manager.m_settings.setToolCatalogs(json); }
bool CatalogController::sessionIsOpen() const { return m_manager.sessionIsOpen(); }
bool CatalogController::configuredProvidersOnly() const { return m_manager.configuredProvidersOnly(); }

QVariantList CatalogController::filterProviders(const QVariantList &choices, const QStringList &configured,
                                      bool configuredOnly, const QString &current)
{
    QVariantList keep;
    if (!configuredOnly || configured.isEmpty()) {
        keep = choices;
    } else {
        for (const QVariant &choice : choices) {
            const QString value = choice.toMap().value(QStringLiteral("value")).toString();
            // `current` is kept unconditionally: a disabled/off-catalog pick still has to
            // show in its own combo, or it looks like the app switched provider by itself.
            if (value == current || configured.contains(value))
                keep.append(choice);
        }
        // Never hand back an empty model: a stale inventory that matches nothing (or an
        // all-unconfigured server) degrades to the full list instead of a blank combo.
        if (keep.isEmpty())
            keep = choices;
    }
    // Two choices can carry the SAME display label. An exact (name, value) pair is
    // nothing but a duplicate — it picks the same provider twice — so it is dropped;
    // a shared label with DIFFERENT values (a CLI-detected entry beside its API
    // twin) keeps both, because dropping either would make one unreachable, and the
    // later ones carry their id so they can be told apart.
    QVariantList unique;
    QHash<QString, int> labelAt;
    for (const QVariant &choice : keep) {
        QVariantMap entry = choice.toMap();
        const QString label = entry.value(QStringLiteral("name")).toString();
        if (label.isEmpty()) {
            unique.append(entry);
            continue;
        }
        const QString value = entry.value(QStringLiteral("value")).toString();
        const QString exactKey = label + QLatin1Char('\n') + value;
        if (labelAt.contains(exactKey))
            continue;   // same label AND same value: a true duplicate
        if (labelAt.contains(label)) {
            entry[QStringLiteral("name")] = value.isEmpty()
                                                ? label
                                                : label + QStringLiteral(" (") + value + QLatin1Char(')');
            labelAt.insert(label + QLatin1Char('\n') + value, unique.size());
        } else {
            labelAt.insert(label, unique.size());
            labelAt.insert(exactKey, unique.size());
        }
        unique.append(entry);
    }
    return unique;
}

QStringList CatalogController::configuredProvidersFromJson(const QString &json)
{
    // goose's inventory of providers. `configured` alone is too broad: goose also marks
    // local endpoints and detected CLIs usable with nothing set. `hasCredentials` (the
    // core's stricter signal — a provider config field is actually set) keeps only the
    // providers the user actually set up. When the core doesn't send it (older .so) fall
    // back to `configured` so nothing regresses.
    QStringList configured;
    for (const auto &el : parseArr(json)) {
        const QJsonObject o = el.toObject();
        const QString id = o.value("providerId").toString();
        if (id.isEmpty() || !o.value("configured").toBool())
            continue;
        const bool hasCredentials = o.contains("hasCredentials")
                                        ? o.value("hasCredentials").toBool()
                                        : true;
        if (hasCredentials)
            configured << id;
    }
    return configured;
}

void CatalogController::onProviders(const QString &json)
{
    const QStringList configured = configuredProvidersFromJson(json);
    // An empty parse must not empty the pickers: a server that doesn't answer this
    // leaves the previous list (and configured-providers-only degrades to "show all").
    if (configured.isEmpty() && !m_configuredProviders.isEmpty())
        return;
    m_configuredProviders = configured;
    emit m_manager.providersChanged();
}

void CatalogController::refreshProviders()
{
    if (m_bridge && m_bridge->isAvailable())
        m_bridge->api().grouse_unstable_providers_list(m_bridge->handle());
}

QVariantList CatalogController::filterConfiguredProviders(const QVariantList &choices, const QString &current) const
{
    return filterProviders(choices, m_configuredProviders, configuredProvidersOnly(), current);
}

void CatalogController::refreshRecipes()
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    m_bridge->api().grouse_unstable_recipes_list(m_bridge->handle());
    m_bridge->api().grouse_unstable_schedules_list(m_bridge->handle());
}

void CatalogController::runRecipe(const QString &id)
{
    m_pendingRecipeId = id;
    m_pendingProjectFiling.clear();
    newChat();
}

void CatalogController::scheduleRecipe(const QString &id, const QString &cron)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QByteArray rid = id.toUtf8();
    const QByteArray c = cron.isEmpty() ? QByteArray() : cron.toUtf8();
    m_bridge->api().grouse_unstable_recipes_schedule(m_bridge->handle(), rid.constData(),
                                                     cron.isEmpty() ? nullptr : c.constData());
}

void CatalogController::deleteRecipe(const QString &id)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QByteArray rid = id.toUtf8();
    m_bridge->api().grouse_unstable_recipes_delete(m_bridge->handle(), rid.constData());
}

// recipes/save replaces the WHOLE recipe: the DTO must be the complete listed
// object with only the edited keys changed (mirrors Android's recipeWith).

void CatalogController::saveRecipe(const QString &id, const QString &recipeJson)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QByteArray rid = id.toUtf8();
    const QByteArray rj = recipeJson.toUtf8();
    m_bridge->api().grouse_unstable_recipes_save(m_bridge->handle(), rid.constData(),
                                                 rj.constData());
}

void CatalogController::setSchedulePaused(const QString &scheduleId, bool paused)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QByteArray s = scheduleId.toUtf8();
    auto f = paused ? m_bridge->api().grouse_unstable_schedules_pause
                    : m_bridge->api().grouse_unstable_schedules_unpause;
    f(m_bridge->handle(), s.constData());
}

void CatalogController::runScheduleNow(const QString &scheduleId)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QByteArray s = scheduleId.toUtf8();
    m_bridge->api().grouse_unstable_schedules_run_now(m_bridge->handle(), s.constData());
}

void CatalogController::refreshSkills()
{
    if (m_bridge && m_bridge->isAvailable())
        m_bridge->api().grouse_unstable_sources_list(m_bridge->handle(), "skill");
}

void CatalogController::saveSkill(const QString &path, const QString &name,
                        const QString &description, const QString &content)
{
    if (!m_bridge || !m_bridge->isAvailable() || path.isEmpty())
        return;
    const QByteArray type = QByteArrayLiteral("skill");
    const QByteArray p = path.toUtf8();
    const QByteArray nm = name.toUtf8();
    const QByteArray d = description.toUtf8();
    const QByteArray c = content.toUtf8();
    m_bridge->api().grouse_unstable_sources_update(m_bridge->handle(), type.constData(),
                                                  p.constData(), nm.constData(), d.constData(), c.constData());
}

// The project's whole source is replaced (the same sources/update the skills
// use, type "project"); the core re-lists projects on the reply, so open
// views refresh on their own.

void CatalogController::deleteSkill(const QString &path)
{
    if (!m_bridge || !m_bridge->isAvailable() || path.isEmpty())
        return;
    const QByteArray type = QByteArrayLiteral("skill");
    const QByteArray p = path.toUtf8();
    m_bridge->api().grouse_unstable_sources_delete(m_bridge->handle(), type.constData(), p.constData());
}

// ---- server config (providers) ---------------------------------------------

void CatalogController::setServerConfig(const QString &key, const QString &value)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QByteArray k = key.toUtf8();
    const QByteArray v = value.toUtf8();
    m_bridge->api().grouse_unstable_config_upsert(m_bridge->handle(), k.constData(), v.constData());
    m_bridge->api().grouse_unstable_config_read(m_bridge->handle(), k.constData());
}

void CatalogController::readServerConfig(const QString &key)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QByteArray k = key.toUtf8();
    m_bridge->api().grouse_unstable_config_read(m_bridge->handle(), k.constData());
}

void CatalogController::refreshGooseConfig()
{
    // read-all emits one on_config_value per NON-secret key. Clear first: a
    // removed key must disappear from the editor instead of lingering as a row
    // nothing on the server supports any more.
    m_serverConfig.clear();
    emit m_manager.serverConfigChanged();
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_unstable_config_read_all)
        return;
    m_bridge->api().grouse_unstable_config_read_all(m_bridge->handle());
}

void CatalogController::readGooseConfig(const QString &key, bool isSecret)
{
    if (key.isEmpty())
        return;
    if (isSecret)
        m_gooseSecretKeys.insert(key);
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_unstable_config_read_ex)
        return;
    const QByteArray k = key.toUtf8();
    m_bridge->api().grouse_unstable_config_read_ex(m_bridge->handle(), k.constData(),
                                                   isSecret ? 1 : 0);
}

void CatalogController::setGooseConfig(const QString &key, const QString &value, bool isSecret)
{
    if (key.isEmpty())
        return;
    // The flag is recorded before the intent so the read-back (which arrives on
    // on_config_value with no secret marker) can be masked immediately.
    if (isSecret)
        m_gooseSecretKeys.insert(key);
    else
        m_gooseSecretKeys.remove(key);
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_unstable_config_upsert_ex)
        return;
    const QByteArray k = key.toUtf8();
    const QByteArray v = value.toUtf8();
    m_bridge->api().grouse_unstable_config_upsert_ex(m_bridge->handle(), k.constData(),
                                                     v.constData(), isSecret ? 1 : 0);
    readGooseConfig(key, isSecret);
}

void CatalogController::removeGooseConfig(const QString &key, bool isSecret)
{
    if (key.isEmpty())
        return;
    if (m_bridge && m_bridge->isAvailable() && m_bridge->api().grouse_unstable_config_remove) {
        const QByteArray k = key.toUtf8();
        m_bridge->api().grouse_unstable_config_remove(m_bridge->handle(), k.constData(),
                                                      isSecret ? 1 : 0);
    }
    // Deleted secrets must not come back as a masked row that can never be
    // re-read by read-all. A non-secret key is forgotten by the refresh too.
    m_gooseSecretKeys.remove(key);
    refreshGooseConfig();
}

void CatalogController::refreshSupportedModels(const QString &providerId)
{
    if (m_bridge && m_bridge->isAvailable() && !providerId.isEmpty()) {
        const QByteArray p = providerId.toUtf8();
        m_bridge->api().grouse_unstable_supported_models(m_bridge->handle(), p.constData());
    }
}

// ---- per-session tool management ------------------------------------------

void CatalogController::refreshToolGroups()
{
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_ready(m_bridge->handle()))
        return;
    // The row catalog is the GLOBAL extension list; the switches are the CURRENT
    // session's attached set; the tool checkboxes are the session's active tools.
    // All three, every time — the switches used to refresh only as a side effect
    // of an add's re-list, so opening a chat showed everything OFF until the user
    // toggled one thing (which "fixed" the rest).
    const QByteArray sid = m_currentSessionId.toUtf8();
    if (sid.isEmpty()) {
        // No chat open: the rows still have to come from somewhere, and with no session
        // to route on that is the main server's config.yaml.
        m_bridge->api().grouse_unstable_list_global_extensions(m_bridge->handle());
        return;
    }
    // The CATALOG for this chat (its own server — the peer's, for a roam chat). Asking
    // the main connection instead is what made a peer chat's sheet list the local
    // server's rows while offering nothing the chat actually lacked.
    m_bridge->api().grouse_unstable_extension_catalog(m_bridge->handle(), sid.constData());
    m_bridge->api().grouse_unstable_list_tools(m_bridge->handle(), sid.constData());
    m_bridge->api().grouse_unstable_session_extensions_list(m_bridge->handle(), sid.constData());
}

void CatalogController::discoverToolGroup(const QString &extName)
{
    const ExtDef *d = extDef(extName);
    if (!d || m_toolCatalog.contains(extName) || !m_bridge || !m_bridge->isAvailable()
        || m_currentSessionId.isEmpty())   // a peek without an open session can never be confirmed
        return;
    QJsonObject unfiltered = d->raw;
    unfiltered.insert("available_tools", QJsonArray());
    m_discoveringExt = extName;
    // Peek works for DETACHED rows too (that's the point: read the tool list
    // without paying its context cost). goose only enumerates tools of a
    // RUNNING extension, so the peek attaches it for one list round-trip —
    // and detaches it again unless it was already in the session. A leading
    // remove is only sent for an attached row (remove of a never-attached one
    // is a server-side "not found" error).
    m_discoveringAttached = m_sessionExts.contains(extName);
    const QByteArray sid = m_currentSessionId.toUtf8();
    const QByteArray ext = QJsonDocument(unfiltered).toJson(QJsonDocument::Compact);
    if (m_discoveringAttached)
        m_bridge->api().grouse_unstable_session_extensions_remove(m_bridge->handle(), sid.constData(),
                                                                  d->key.toUtf8().constData());
    m_bridge->api().grouse_unstable_session_extensions_add(m_bridge->handle(), sid.constData(),
                                                           ext.constData());
}

void CatalogController::setSessionExtensionEnabled(const QString &extName, bool enabled)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const ExtDef *d = extDef(extName);
    if (!d)
        return;
    const QByteArray sid = m_currentSessionId.toUtf8();
    if (enabled) {
        if (!m_sessionExts.contains(extName))
            m_sessionExts << extName;
        // The optimistic attach carries whatever allowlist the global profile
        // has, so the restriction set must match until the server re-lists.
        if (d->raw.value(QStringLiteral("available_tools")).toArray().isEmpty())
            m_sessionRestricted.remove(extName);
        else
            m_sessionRestricted.insert(extName);
        m_bridge->api().grouse_unstable_session_extensions_add(
            m_bridge->handle(), sid.constData(),
            QJsonDocument(d->raw).toJson(QJsonDocument::Compact).constData());
    } else {
        m_sessionExts.removeAll(extName);
        m_sessionRestricted.remove(extName);
        m_bridge->api().grouse_unstable_session_extensions_remove(
            m_bridge->handle(), sid.constData(), d->key.toUtf8().constData());
    }
    publishToolGroups();
    saveToolCache(m_currentSessionId);
}

void CatalogController::setSessionToolEnabled(const QString &extName, const QString &toolName, bool on)
{
    const ExtDef *d = extDef(extName);
    if (!d)
        return;
    // Acting on a session needs one open: these RPCs carry the session id, and
    // goose answers every one it cannot resolve with its own error — three of
    // them for one toggle, on a chat whose transcript is still on screen from
    // the cache (grouse#20). Browsing the catalog stays fine; only acting needs
    // a chat. Android has guarded all of these with `activeSessionId() ?: return`
    // from the start.
    if (!sessionIsOpen())
        return;
    const QString prefix = d->key + QStringLiteral("__");
    if (!m_sessionExts.contains(extName)) {
        // Ticking a tool on a DETACHED row attaches the extension restricted
        // to exactly that tool — the whole point of letting you browse lists
        // without attaching: you enable one tool, not all of them.
        if (!on || !m_bridge || !m_bridge->isAvailable())
            return;
        QJsonObject scoped = d->raw;
        scoped.insert("available_tools", QJsonArray{toolName});   // BARE name
        const QByteArray sid = m_currentSessionId.toUtf8();
        m_bridge->api().grouse_unstable_session_extensions_add(
            m_bridge->handle(), sid.constData(),
            QJsonDocument(scoped).toJson(QJsonDocument::Compact).constData());
        m_sessionExts << extName;
        m_sessionRestricted.insert(extName);   // restricted to that one tool
        publishToolGroups();
        saveToolCache(m_currentSessionId);
        return;
    }
    QSet<QString> current;
    for (const auto &t : m_tools)
        if (t.startsWith(prefix))
            current << t;
    if (on == current.contains(prefix + toolName))
        return;
    if (on) current << (prefix + toolName);
    else current.remove(prefix + toolName);
    setSessionTools(extName, current.values());
}

// ---- global (config.yaml) extensions ----------------------------------------

void CatalogController::refreshGlobalExtensions()
{
    if (m_bridge && m_bridge->isAvailable())
        m_bridge->api().grouse_unstable_list_global_extensions(m_bridge->handle());
}

void CatalogController::setGlobalExtensionEnabled(const QString &extKey, bool enabled)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    // GLOBAL means the main server's config.yaml: a row that exists only in the chat's
    // catalog (a peer's extension) must not be written here, or a peer-only key would
    // land in this box's config as a phantom.
    const ExtDef *d = globalExtDef(extKey);
    if (!d)
        return;
    m_bridge->api().grouse_unstable_set_extension_enabled(
        m_bridge->handle(), d->key.toUtf8().constData(), enabled ? 1 : 0);
}

void CatalogController::setGlobalToolEnabled(const QString &extName, const QString &toolName, bool on)
{
    const ExtDef *d = globalExtDef(extName);
    if (!d)
        return;
    QSet<QString> current(d->availableTools.constBegin(), d->availableTools.constEnd());
    // available_tools is a list of BARE tool names (server-verified), both read and written.
    if (on == current.contains(toolName))
        return;
    if (on) current << toolName;
    else current.remove(toolName);
    QJsonObject scoped = d->raw;
    QJsonArray arr;
    for (const auto &t : std::as_const(current))
        arr.append(t);
    scoped.insert("available_tools", arr);
    m_bridge->api().grouse_unstable_add_extension(
        m_bridge->handle(),
        QJsonDocument(scoped).toJson(QJsonDocument::Compact).constData(), d->enabled ? 1 : 0);
}

// ---- tool-group plumbing ---------------------------------------------------

QVariant CatalogController::globalExtensions() const
{
    QVariantList out;
    for (const auto &d : m_extDefs) {
        QVariantMap group;
        group["name"] = d.name;   // display
        group["key"] = d.key;     // identity: toggles + tool prefixes are keyed by configKey
        group["type"] = d.type;
        group["attrib"] = d.attrib;
        group["enabled"] = d.enabled;
        QVariantList tools;
        const QString prefix = d.key + QStringLiteral("__");
        const QSet<QString> allowed(d.availableTools.constBegin(), d.availableTools.constEnd());
        if (allowed.isEmpty()) {
            const QStringList full = m_toolCatalog.value(d.key);
            for (const auto &t : full)
                tools << QVariantMap{{"name", t.mid(prefix.length())}, {"on", true}};
        } else {
            // available_tools entries are BARE tool names (verified live: a prefixed
            // allowlist matches nothing and silently disables the whole extension).
            for (const auto &t : d.availableTools)
                tools << QVariantMap{{"name", t}, {"on", true}};
        }
        group["tools"] = tools;
        out << group;
    }
    return out;
}

QVariant CatalogController::toolGroups() const
{
    QVariantList out;
    const QSet<QString> active(m_tools.constBegin(), m_tools.constEnd());
    const QSet<QString> enabled(m_sessionExts.constBegin(), m_sessionExts.constEnd());
    // Every identity here is the extension KEY; `name` is carried per group for display.
    QStringList allKeys;
    for (const auto &d : (m_catalogDefs.isEmpty() ? m_extDefs : m_catalogDefs))
        allKeys << d.key;
    for (const auto &n : m_sessionExts)
        if (!allKeys.contains(n))
            allKeys << n;

    // Some goose versions expose tools before extension profiles: group the
    // active names directly so the panel stays useful.
    if (allKeys.isEmpty() && !m_tools.isEmpty()) {
        QHash<QString, QVariantList> grouped;
        QStringList groupNames;
        for (const auto &tool : m_tools) {
            const int sep = tool.indexOf(QStringLiteral("__"));
            const QString group = sep > 0 ? tool.left(sep) : QStringLiteral("Built-in");
            const QString child = sep > 0 ? tool.mid(sep + 2) : tool;
            if (!grouped.contains(group))
                groupNames << group;
            grouped[group] << QVariantMap{{"name", child}, {"on", true}};
        }
        for (const auto &groupName : std::as_const(groupNames)) {
            out << QVariantMap{{"name", groupName},
                               {"key", groupName},
                               {"attrib", groupName != QStringLiteral("Built-in")},
                               {"enabled", true},
                               {"known", true},
                               {"expandable", grouped.value(groupName).size() >= 2},
                               {"tools", grouped.value(groupName)}};
        }
        return out;
    }

    for (const auto &key : std::as_const(allKeys)) {
        QVariantMap group;
        const ExtDef *d = extDef(key);
        group["name"] = d ? d->name : key;
        group["key"] = key;
        const bool attrib = d && d->attrib;
        group["attrib"] = attrib;
        const bool attached = enabled.contains(key);
        group["enabled"] = attached;
        const QString prefix = key + QStringLiteral("__");
        QVariantList tools;
        const bool cached = m_toolCatalog.contains(key);
        QStringList pool = cached ? m_toolCatalog.value(key) : QStringList();
        if (pool.isEmpty()) {
            for (const auto &t : m_tools)
                if (t.startsWith(prefix))
                    pool << t;
        }
        // Is the sub-tool LIST fully known without a peek? Either yes (cached
        // from a discovery this run — the catalog is kept across chats) or by
        // derivation: an attached row with NO session allowlist runs
        // unfiltered, so its active namespaced tools are the whole list. A
        // zero-active derivation is only trusted for non-mcp rows — an mcp
        // that has not finished starting also lists zero, and demoting it to
        // "no sub-tools" would hide a real arrow.
        const bool derivedComplete =
            attached && !m_sessionRestricted.contains(key);
        const bool derivedZeroUntrusted =
            pool.isEmpty() && d && d->type == QLatin1String("mcp");
        const bool known = cached || (derivedComplete && !derivedZeroUntrusted);
        group["known"] = known;
        // The expander is about SUB-TOOLS, not attachment: unknown rows get the
        // arrow as a PEEK (list without keeping it attached); known rows keep it
        // only at >=2 sub-tools — one-tool and bare-named rows have nothing to
        // expand.
        group["expandable"] = known ? (pool.size() >= 2) : true;
        for (const auto &t : pool) {
            tools << QVariantMap{{"name", t.mid(prefix.length())},
                                 {"on", active.contains(t)}};
        }
        group["tools"] = tools;
        out << group;
    }
    return out;
}

QList<CatalogController::ExtDef> CatalogController::parseExtDefs(const QString &json)
{
    QList<ExtDef> out;
    for (const auto &el : parseArr(json)) {
        const QVariantMap m = el.toObject().toVariantMap();
        // The server nests the definition: {"extension": {...}, "enabled": bool,
        // "configKey": "..."}. Read the nested object (with a flat fallback) so
        // names/types aren't blank — the old code read the top level, where
        // those fields don't exist, producing empty-named groups that all
        // shared one enabled state.
        QVariantMap e = m.value("extension").toMap();
        if (e.isEmpty())
            e = m;
        // Server-backed/custom extensions (mcp, bundled:false) carry their
        // identifier in configKey / server.name, not extension.name — only
        // bundled platform/builtin ones expose `name`. Resolve in that order so
        // the panel isn't full of empty-named groups that share one state.
        QString extName = e.value("name").toString();
        if (extName.isEmpty())
            extName = m.value("configKey").toString();
        if (extName.isEmpty())
            extName = e.value("server").toMap().value("name").toString();
        ExtDef d;
        d.name = extName;
        // Identity is the KEY (session list calls it extensionKey, global list
        // configKey). The display name can differ: "Extension Manager" is keyed
        // "extensionmanager", and remove/toggles only accept the key.
        d.key = m.value("configKey").toString().isEmpty() ? extName
                                                         : m.value("configKey").toString();
        d.type = e.value("type").toString();
        // mcp-backed extensions namespace their tools; mark them addable so the
        // panel offers the per-tool toggle the server's add/remove expects.
        d.attrib = e.value("attrib").toBool() || d.type == QLatin1String("mcp");
        d.enabled = m.contains("enabled")
            ? m.value("enabled").toBool()
            : e.value("enabled").toBool();
        d.raw = QJsonObject::fromVariantMap(e);
        // session/extensions/add needs a top-level `name` to identify the
        // extension; server-backed (mcp) defs carry it only in server.name, so
        // inject the resolved id — otherwise the server silently ignores the
        // add and the session-extensions re-list reverts the toggle.
        if (d.raw.value("name").toString().isEmpty() && !d.name.isEmpty())
            d.raw.insert(QStringLiteral("name"), d.name);
        const QVariantList at = m.value("availableTools").toList();
        for (const auto &a : at)
            d.availableTools << a.toString();
        out << d;
    }
    return out;
}

const CatalogController::ExtDef *CatalogController::extDef(const QString &key) const
{
    // The CHAT's server first: for a roam chat that is the peer's definition, and it is
    // the one its session/extensions/add accepts. A key only the main server knows still
    // resolves for the tool grouping and for the settings screen's rows.
    for (const auto &d : m_catalogDefs)
        if (d.key == key)
            return &d;
    return globalExtDef(key);
}

const CatalogController::ExtDef *CatalogController::globalExtDef(const QString &key) const
{
    for (const auto &d : m_extDefs)
        if (d.key == key)
            return &d;
    return nullptr;
}

void CatalogController::setSessionTools(const QString &extName, const QStringList &allowed)
{
    const ExtDef *d = extDef(extName);
    if (!d)
        return;
    if (!sessionIsOpen())
        return;
    const QStringList full = m_toolCatalog.value(extName);
    const QStringList list =
        (!full.isEmpty() && allowed.size() >= full.size()) ? QStringList() : allowed;
    QJsonObject scoped = d->raw;
    QJsonArray arr;
    // The caller passes fully-qualified `key__tool` names (they come from the session's
    // tool list); the server's available_tools matches on BARE names — a prefixed entry
    // silently filters out every tool (verified live). Strip on the way out.
    const QString strip = d->key + QStringLiteral("__");
    for (const auto &t : list)
        arr.append(t.startsWith(strip) ? t.mid(strip.length()) : t);
    scoped.insert("available_tools", arr);
    if (arr.isEmpty())
        m_sessionRestricted.remove(extName);   // empty allowlist = unfiltered
    else
        m_sessionRestricted.insert(extName);
    m_discoveringExt.clear();
    const QByteArray sid = m_currentSessionId.toUtf8();
    m_bridge->api().grouse_unstable_session_extensions_remove(
        m_bridge->handle(), sid.constData(), d->key.toUtf8().constData());
    m_bridge->api().grouse_unstable_session_extensions_add(
        m_bridge->handle(), sid.constData(),
        QJsonDocument(scoped).toJson(QJsonDocument::Compact).constData());
}

void CatalogController::publishToolGroups()
{
    emit m_manager.toolGroupsChanged();
}

void CatalogController::loadToolCache(const QString &sessionId)
{
    // The catalogue is extension knowledge, not session state: reading it back
    // is what lets "does this row have >=2 sub-tools?" survive a restart
    // instead of re-arrowing every row on first open. Only keys not already
    // learned this run are merged (live truth wins over memory).
    // Global memory first: one server-wide sweep result shared by every chat.
    const QByteArray persisted =
        toolCatalogs();
    if (!persisted.isEmpty()) {
        const QJsonObject cats = QJsonDocument::fromJson(persisted).object();
        for (auto it = cats.constBegin(); it != cats.constEnd(); ++it) {
            if (m_toolCatalog.contains(it.key()))
                continue;
            QStringList tools;
            for (const auto &v : it.value().toArray())
                tools << v.toString();
            m_toolCatalog.insert(it.key(), tools);
        }
    }
    if (sessionId.isEmpty())
        return;
    QString safe = sessionId;
    safe.replace(QLatin1Char('/'), QLatin1Char('_'));
    const QString base = QStandardPaths::writableLocation(QStandardPaths::CacheLocation);
    QFile f(base + QStringLiteral("/") + safe + QStringLiteral("-tools.json"));
    if (!f.open(QIODevice::ReadOnly))
        return;
    const QJsonObject catalogs = QJsonDocument::fromJson(f.readAll()).object()
                                   .value(QStringLiteral("catalog")).toObject();
    for (auto it = catalogs.constBegin(); it != catalogs.constEnd(); ++it) {
        if (m_toolCatalog.contains(it.key()))
            continue;
        QStringList tools;
        for (const auto &v : it.value().toArray())
            tools << v.toString();
        m_toolCatalog.insert(it.key(), tools);   // an empty list is real knowledge too
    }
}

void CatalogController::saveToolCache(const QString &sessionId) const
{
    if (sessionId.isEmpty())
        return;
    QJsonObject root;
    QJsonArray tools;
    for (const auto &tool : m_tools)
        tools.append(tool);
    root.insert("tools", tools);

    QJsonArray sessionExtensions;
    for (const auto &name : m_sessionExts)
        sessionExtensions.append(name);
    root.insert("sessionExtensions", sessionExtensions);

    QJsonArray extensions;
    for (const auto &d : m_extDefs) {
        QJsonArray availableTools;
        for (const auto &tool : d.availableTools)
            availableTools.append(tool);
        extensions.append(QJsonObject{
            {"name", d.name},
            {"type", d.type},
            {"attrib", d.attrib},
            {"availableTools", availableTools},
            {"raw", d.raw},
        });
    }
    root.insert("extensions", extensions);

    QJsonObject catalogs;
    for (auto it = m_toolCatalog.constBegin(); it != m_toolCatalog.constEnd(); ++it) {
        QJsonArray toolsForExtension;
        for (const auto &tool : it.value())
            toolsForExtension.append(tool);
        catalogs.insert(it.key(), toolsForExtension);
    }
    root.insert("catalog", catalogs);

    QString safe = sessionId;
    safe.replace(QLatin1Char('/'), QLatin1Char('_'));
    const QString base = QStandardPaths::writableLocation(QStandardPaths::CacheLocation);
    QDir().mkpath(base);
    QFile f(base + QStringLiteral("/") + safe + QStringLiteral("-tools.json"));
    if (f.open(QIODevice::WriteOnly | QIODevice::Truncate)) {
        f.write(QJsonDocument(root).toJson(QJsonDocument::Compact));
        f.close();
    }
}

void CatalogController::onSessionExtensions(const QString &sid, const QString &json)
{
    // Stale replies from a session the user already left must not clobber the
    // current sheet's switch state (Android's onSessionExtensions guards the same).
    if (!m_currentSessionId.isEmpty() && sid != m_currentSessionId)
        return;
    QStringList names;
    QSet<QString> restricted;
    for (const auto &el : parseArr(json)) {
        if (el.isObject()) {
            // Current goose WRAPS each entry: {"extension": {...}, "extensionKey": "..."}.
            // Older servers sent the extension object bare. Resolve the KEY either way
            // (extensionKey > extension.name > server.name) — remove is keyed by it, and
            // display names ("Extension Manager") do not match the tool prefixes
            // ("extensionmanager__…") that the catalog/pool grouping needs.
            const QVariantMap m = el.toObject().toVariantMap();
            QVariantMap inner = m.value("extension").toMap();
            if (inner.isEmpty())
                inner = m;
            QString key = m.value("extensionKey").toString();
            if (key.isEmpty())
                key = inner.value("name").toString();
            if (key.isEmpty())
                key = inner.value("server").toMap().value("name").toString();
            if (!key.isEmpty()) {
                names << key;
                // A non-empty session allowlist means the ACTIVE prefix is not the
                // whole list. Without one an attached extension runs unfiltered,
                // so its active tools ARE the catalogue (see toolGroups).
                if (!inner.value("available_tools").toList().isEmpty())
                    restricted.insert(key);
            }
        } else {
            const QString s = el.toString();
            if (!s.isEmpty())
                names << s;
        }
    }
    onSessionExtensions(names, restricted);
}

void CatalogController::buildPeekQueue()
{
    if (!m_peekQueue.isEmpty() || !m_discoveringExt.isEmpty())
        return;                              // a sweep or a manual peek is running
    if (m_currentSessionId.isEmpty() || !m_activePeerLabel.isEmpty())
        return;                              // only the local chat can host a peek
    for (const auto &d : (m_catalogDefs.isEmpty() ? m_extDefs : m_catalogDefs))
        // Only rows the server actually runs: a peek is a real session attach, so a
        // GLOBALLY DISABLED extension (kwin's dead tailnet URI, say) must never be
        // dialed just to learn its tool count — the server's failed client-init
        // surfaces as an error bubble in the live chat (every chat, once per
        // unknown disabled row). Manual arrow clicks still peek anything: that is
        // an explicit request, and its failure is the user's to see.
        if (d.enabled && !m_toolCatalog.contains(d.key) && !m_sessionExts.contains(d.key)
            && !m_peekFailed.contains(d.key))
            m_peekQueue << d.key;
    if (!m_peekQueue.isEmpty())
        m_peekTimer->start(1500);
}

void CatalogController::doPeekStep()
{
    GROUSE_PHASE("phase peek");
    if (m_peekQueue.isEmpty())
        return;
    // Never interfere with an active turn — INCLUDING one started elsewhere
    // (m_activeRunId is the server's own echo, so scheduled runs steering into
    // this chat gate the sweep too), compaction, a manual peek, or a lost wire.
    if (m_prompting || m_compacting || !m_activeRunId.isEmpty()
        || !m_discoveringExt.isEmpty()
        || !m_bridge || !m_bridge->isAvailable()
        || !m_bridge->api().grouse_ready(m_bridge->handle())
        || m_currentSessionId.isEmpty() || !m_activePeerLabel.isEmpty()) {
        m_peekTimer->start(2500);            // retry when the chat goes quiet
        return;
    }
    while (!m_peekQueue.isEmpty()) {
        const QString key = m_peekQueue.takeFirst();
        if (m_toolCatalog.contains(key) || m_sessionExts.contains(key) || !extDef(key))
            continue;                        // state moved on; skip stale queue item
        discoverToolGroup(key);
        break;                               // the commit in onSessionExtensions re-arms
    }
    if (!m_peekQueue.isEmpty() && m_discoveringExt.isEmpty())
        m_peekTimer->start(700);             // safety re-arm if the peek never lands
}

void CatalogController::persistCatalogs()
{
    QJsonObject o;
    for (auto it = m_toolCatalog.constBegin(); it != m_toolCatalog.constEnd(); ++it) {
        QJsonArray a;
        for (const auto &t : it.value())
            a.append(t);
        o.insert(it.key(), a);
    }
    setToolCatalogs(QJsonDocument(o).toJson(QJsonDocument::Compact));
}

void CatalogController::onRecipes(const QVariantList &recipes)
{
    m_recipes = recipes;
    emit m_manager.recipesChanged();
}

void CatalogController::onSchedules(const QVariantList &schedules)
{
    m_schedules = schedules;
    emit m_manager.schedulesChanged();
}

void CatalogController::onSkills(const QVariantList &skills)
{
    m_skills = skills;
    emit m_manager.skillsChanged();
}

void CatalogController::onTools(const QVariantList &tools)
{
    QStringList names;
    for (const auto &v : tools)
        names << v.toString();
    m_tools = names;
    if (!m_discoveringExt.isEmpty()) {
        // Stash only. The catalog commit + the restore/detach happen when the
        // session-extensions re-list (which the core always sends after the
        // peek's add, and always second) confirms whether the peeked extension
        // is REALLY attached — a transient add that failed to start the
        // extension lists nothing, indistinguishable from a genuinely
        // tool-less/bare-named one except by the attach state.
        const QString prefix = m_discoveringExt + QStringLiteral("__");
        for (const auto &n : std::as_const(names))
            if (n.startsWith(prefix))
                m_discoveringFull << n;
    } else {
        publishToolGroups();
    }
    saveToolCache(m_currentSessionId);
    emit m_manager.toolsChanged();
}

/// Parse a `config/extensions/list` payload (`session/extensions/list` carries the same
/// extension objects, keyed `extensionKey` instead of `configKey`). One parser for both
/// lists: the settings screen's defaults and the chat's own server catalog are answered
/// by the same method on different connections, and they must read identically.

void CatalogController::onExtensions(const QVariantList &extensions)
{
    QJsonArray arr;
    for (const auto &v : extensions)
        arr.append(QJsonValue::fromVariant(v));
    m_extDefs = parseExtDefs(QString::fromUtf8(QJsonDocument(arr).toJson(QJsonDocument::Compact)));
    publishToolGroups();
    saveToolCache(m_currentSessionId);
    emit m_manager.globalExtensionsChanged();
    emit m_manager.toolsChanged();
}

void CatalogController::onExtensionCatalog(const QString &sid, const QString &json)
{
    // Stale replies from a chat the user already left must not become this
    // sheet's rows (same guard as the session list).
    if (!m_currentSessionId.isEmpty() && sid != m_currentSessionId)
        return;
    m_catalogDefs = parseExtDefs(json);
    buildPeekQueue();
    publishToolGroups();
}

void CatalogController::onServerConfigValue(const QString &key, const QString &value)
{
    m_serverConfig[key] = value;
    emit m_manager.serverConfigChanged();
}

void CatalogController::onSupportedModels(const QString &providerId, const QStringList &models)
{
    Q_UNUSED(providerId);
    m_supportedModels.clear();
    for (const auto &m : models)
        m_supportedModels << m;
    emit m_manager.supportedModelsChanged();
}
