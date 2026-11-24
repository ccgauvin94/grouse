// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once

#include <QHash>
#include <QJsonObject>
#include <QSet>
#include <QString>
#include <QStringList>
#include <QVariant>
#include <QVariantList>

class CoreBridge;
class Manager;
class QTimer;

/** Server catalogs the desktop administers: skills, recipes/schedules,
 *  providers, global extensions and the current chat's tool/extension set.
 *
 *  Manager keeps every Q_PROPERTY/Q_INVOKABLE/coreOn* name; the parsing,
 *  optimistic toggles, tool-cache and background peek live here.
 */
class CatalogController
{
public:
    struct ExtDef {
        QString name;
        QString key;
        QString type;
        bool attrib = false;
        bool enabled = true;
        QStringList availableTools;
        QJsonObject raw;
    };

    explicit CatalogController(Manager &manager);

    // --- recipes / schedules / skills ----------------------------------------
    void refreshRecipes();
    void runRecipe(const QString &id);
    void scheduleRecipe(const QString &id, const QString &cron);
    void deleteRecipe(const QString &id);
    void saveRecipe(const QString &id, const QString &recipeJson);
    void setSchedulePaused(const QString &scheduleId, bool paused);
    void runScheduleNow(const QString &scheduleId);
    void refreshSkills();
    void saveSkill(const QString &path, const QString &name,
                   const QString &description, const QString &content);
    void deleteSkill(const QString &path);

    // --- providers / server config -------------------------------------------
    void refreshProviders();
    void setServerConfig(const QString &key, const QString &value);
    void readServerConfig(const QString &key);
    // --- the raw config editor (GooseConfigDialog) ---------------------------
    // These share m_serverConfig / serverConfigChanged with the legacy
    // config/read intents: the map is already the raw key -> value cache.
    // Secret keys are remembered client-side ONLY as a flag — read-all lists
    // non-secret config, so there is no other way to know a row must be masked.
    void refreshGooseConfig();
    void readGooseConfig(const QString &key, bool isSecret);
    void setGooseConfig(const QString &key, const QString &value, bool isSecret);
    void removeGooseConfig(const QString &key, bool isSecret);
    void refreshSupportedModels(const QString &providerId);
    static QStringList configuredProvidersFromJson(const QString &json);
    static QVariantList filterProviders(const QVariantList &choices,
                                        const QStringList &configured,
                                        bool configuredOnly, const QString &current);
    QVariantList filterConfiguredProviders(const QVariantList &choices,
                                           const QString &current) const;

    // --- extensions / tools --------------------------------------------------
    void refreshToolGroups();
    void discoverToolGroup(const QString &extName);
    void setSessionExtensionEnabled(const QString &extName, bool enabled);
    void setSessionToolEnabled(const QString &extName, const QString &toolName, bool on);
    void refreshGlobalExtensions();
    void setGlobalExtensionEnabled(const QString &extKey, bool enabled);
    void setGlobalToolEnabled(const QString &extName, const QString &toolName, bool on);
    QVariant globalExtensions() const;
    QVariant toolGroups() const;
    static QList<ExtDef> parseExtDefs(const QString &json);

    /** Fold a core event into the catalog state (Manager's coreOn* forwards here). */
    void onRecipes(const QVariantList &recipes);
    void onSchedules(const QVariantList &schedules);
    void onSkills(const QVariantList &skills);
    void onProviders(const QString &json);
    void onTools(const QVariantList &tools);
    void onExtensions(const QVariantList &extensions);
    void onSessionExtensions(const QStringList &names, const QSet<QString> &restricted);
    void onExtensionCatalog(const QString &sid, const QString &json);
    void onServerConfigValue(const QString &key, const QString &value);
    void onSupportedModels(const QString &providerId, const QStringList &models);

    /** Called by Manager when a conversation opens: forget per-chat leases. */
    void clearSessionState();

    // Catalog plumbing (also reachable from Manager's thin forwarders).
    void buildPeekQueue();
    void doPeekStep();
    void persistCatalogs();
    void loadToolCache(const QString &sessionId);
    void saveToolCache(const QString &sessionId) const;
    bool sessionIsOpen() const;
    bool configuredProvidersOnly() const;
    const ExtDef *extDef(const QString &key) const;
    const ExtDef *globalExtDef(const QString &key) const;
    void setSessionTools(const QString &extName, const QStringList &allowed);
    void publishToolGroups();

private:
    Manager &m_manager;
    CoreBridge *&m_bridge;
    QString &m_currentSessionId;
    QString &m_activePeerLabel;
    QString &m_pendingRecipeId;
    QString &m_pendingProjectFiling;
    QString &m_discoveringExt;
    QStringList &m_discoveringFull;
    bool &m_discoveringAttached;
    QStringList &m_tools;
    QStringList &m_configuredProviders;
    QVariantList &m_recipes;
    QVariantList &m_schedules;
    QVariantList &m_skills;
    QVariantList &m_supportedModels;
    QVariantMap &m_serverConfig;
    QSet<QString> &m_gooseSecretKeys;
    QList<ExtDef> &m_extDefs;
    QList<ExtDef> &m_catalogDefs;
    QStringList &m_sessionExts;
    QSet<QString> &m_sessionRestricted;
    QStringList &m_peekQueue;
    QSet<QString> &m_peekFailed;
    QTimer *&m_peekTimer;
    QHash<QString, QStringList> &m_toolCatalog;
    bool &m_prompting;
    bool &m_compacting;
    QString &m_activeRunId;

    // Thin forwards into Manager and the extracted settings store.
    void timingLog(const QString &line);
    void newChat();
    QByteArray toolCatalogs() const;
    void setToolCatalogs(const QByteArray &json);
};
