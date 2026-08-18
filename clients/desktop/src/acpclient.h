#pragma once

#include <QObject>
#include <QString>
#include <QHash>
#include <QJsonObject>
#include <QList>
#include <QMetaType>

/**
 * Convert an extension as goose REPORTED it (config/extensions/list or
 * session/extensions/list) into the shape the session add method ACCEPTS.
 * They are not the same shape; feeding the listed shape back is -32602. Needed by
 * the Manager when it re-adds an extension with a modified tool allowlist.
 */
QJsonObject toExtensionDto(const QJsonObject &raw);

class AcpTransport;
class QNetworkRequest;

/**
 * Data classes mirroring the Android client's DTOs (grouse/AcpClient.kt).
 * The server (goose serve) owns all state; these are just structural views of
 * what its ACP JSON-RPC replies contain.
 */

struct ConfigChoice {
    QString value;
    QString name;
};
Q_DECLARE_METATYPE(ConfigChoice)

struct ConfigOption {
    QString id;
    QString name;
    QString currentValue;
    QList<ConfigChoice> choices;
};
Q_DECLARE_METATYPE(ConfigOption)

struct SessionInfo {
    QString sessionId;
    QString title;
    QString updatedAt;
    QString snippet;
    QString cwd;
    QString model;
    int messageCount = 0;
};
Q_DECLARE_METATYPE(SessionInfo)

/**
 * Thin ACP (Agent Client Protocol) client over a WebSocket — the desktop twin of
 * grouse/AcpClient.kt. goosed does all the work; this relays prompts and streams
 * updates. All signals are emitted from the thread on which send()/handle() run
 * (the manager's thread). See the AGENTS.md notes in the Android repo — most of
 * the protocol subtleties (casing, session-scoped calls, silent rewrites) apply
 * unchanged here.
 */
class AcpClient : public QObject
{
    Q_OBJECT
public:
    explicit AcpClient(QObject *parent = nullptr);

    /** Config values to re-apply once a session opens (provider first for the model cascade). */
    void setDesiredOptions(const QList<QPair<QString, QString>> &opts) { m_desired = opts; }
    /** Resume this server-side session (session/load) instead of a fresh session/new. */
    void setResumeSession(const QString &sessionId, const QString &cwd) { m_resumeId = sessionId; m_resumeCwd = cwd; }
    /** cwd for a brand-new session (session/new). Must be absolute or goose rejects it. */
    void setDesiredCwd(const QString &cwd) { m_wantCwd = cwd; }

    void connectTo(const QString &url, const QString &secretKey);
    /** Connect over an iroh roam stream (direct peer). The dial happens
     *  asynchronously inside the transport; failures surface via status. */
    void connectRoam(const QString &secret, const QString &card, const QString &label);
    /** Browse mode: after initialize, list the peer's sessions instead of
     *  auto-opening one (session/new or session/load). The caller resumes a
     *  session explicitly when the user picks it. */
    void setBrowseOnly(bool browse) { m_browseOnly = browse; }
    /** Load a session on an ALREADY-CONNECTED client (roam peers): equivalent
     *  to connectTo() with setResumeSession(), minus the reconnect. */
    void openSessionNow() { loadSession(); }
    /** Cheap metadata probe: updatedAt/messageCount for a session, WITHOUT
     *  loading the conversation. Used to detect another client touching the
     *  session (goose streams a turn only to the prompting connection, so the
     *  desktop must detect and replay). */
    void probeSession(const QString &sessionId);
    void close();

    /** True once session/new or session/load completed — the only state session/prompt succeeds in. */
    bool ready() const { return !m_sessionId.isEmpty(); }
    QString sessionId() const { return m_sessionId; }

    // ---- outbound RPC -------------------------------------------------------
    void listSessions();
    /** Send a prompt. `attachments` is a list of full ACP content-block maps (image or
     *  embedded resource) produced by the Manager; each is passed through verbatim. */
    void sendPrompt(const QString &text, const QVariantList &attachments = QVariantList());
    /** Inject a message into the RUNNING turn (requires the activeRunId from session_info_update). */
    void steer(const QString &text);
    void respondPermission(const QString &toolCallId, const QString &optionId);
    void setConfigOption(const QString &configId, const QString &value);
    void cancel();
    /** Serialize a session for backup/sharing; reply arrives as exportResult(). */
    void exportSession(const QString &sessionId);
    /** List the tools active in THIS session (per-conversation). Names are `extension__tool`. */
    void listTools();
    /** List goose projects (`_goose/unstable/sources/list` with type project). */
    void listProjects();
    /** Create a project; `root` is an optional server path (stored in a `root:` content line). */
    void createProject(const QString &name, const QString &description, const QString &root);
    /** Delete a project by its source PATH (from sources/list), not its id. */
    void deleteProject(const QString &path);
    /** File a session into a project; empty projectId unfiles it (explicit JSON null). */
    void assignSessionProject(const QString &sessionId, const QString &projectId);
    /** Recipes (library) and schedules (cron table). */
    void listRecipes();
    void listSchedules();
    /** Schedule/unschedule a recipe by id. Empty cron unschedules (explicit JSON null). */
    void scheduleRecipe(const QString &id, const QString &cron);
    void deleteRecipe(const QString &id);
    void setSchedulePaused(const QString &scheduleId, bool paused);
    void runScheduleNow(const QString &scheduleId);
    void setScheduleCron(const QString &scheduleId, const QString &cron);
    /** Recipe id for the next fresh session (session/new `_meta.recipeId`). */
    void setDesiredRecipeId(const QString &id) { m_desiredRecipeId = id; }
    /** Rename a session (sets its title). Reply re-lists sessions. */
    void renameSession(const QString &sessionId, const QString &title);
    /** Archive a session: out of session/list, history stays on disk; unarchive restores it. */
    void archiveSession(const QString &sessionId);
    void unarchiveSession(const QString &sessionId);
    /** Delete a session outright (irreversible). Reply re-lists sessions. */
    void deleteSession(const QString &sessionId);
    /** List configured extensions agent-globally (the source of this session's tool profiles). */
    void listConfigExtensions();
    /** List the extensions enabled in THIS session (names only). */
    void listSessionExtensions();
    /** Enable/replace an extension for the current session. `extension` is the accept-shaped DTO. */
    void addSessionExtension(const QJsonObject &extensionData);
    /** Disable an extension in the current session (by configured name). */
    void removeSessionExtension(const QString &name);
    /** Global (config.yaml) extension controls — the defaults for NEW sessions. */
    void setConfigExtensionEnabled(const QString &name, bool enabled);
    void addConfigExtension(const QJsonObject &extensionData, bool enabled);
    /** Skills share sources/list (tagged) with projects; the tag disambiguates the reply. */
    void listSkills();
    void updateSkill(const QString &path, const QString &name, const QString &description, const QString &content);
    void deleteSkill(const QString &path);
    /** Read/upsert a global goose config value (config.yaml; takes effect for NEW sessions). */
    void readConfig(const QString &key);
    void upsertConfig(const QString &key, const QString &value);
    /** Ask a provider for its LIVE model list (reply arrives as supportedModelsReady). */
    void listSupportedModels(const QString &providerId);
    /** Fetch an MCP-App HTML template (resources/read); reply arrives as appResource. */
    void readAppResource(const QString &appKey, const QString &uri, const QString &extensionName);

signals:
    void statusChanged(const QString &text);
    void sessionReady(const QString &sessionId);
    void error(const QString &text, bool background);

    // Streaming / transcript
    void agentChunk(const QString &text, const QString &messageId);
    void thoughtChunk(const QString &text);
    void userChunk(const QString &text, const QString &messageId);
    void toolCall(const QString &title, const QString &detail, const QString &toolCallId);
    void toolCallUpdate(const QString &toolCallId, const QString &status, const QString &output, bool live);
    void runEnded(const QString &stopReason);
    /** An autovisualiser chart tool call: `data` carries the Chart.js-style spec (JSON text). */
    void chartToolCall(const QString &title, const QString &toolCallId, const QString &chartSpec);
    /** An MCP-App tool call: the server wants us to fetch + render a UI template for it. */
    void mcpAppToolCall(const QString &title, const QString &toolCallId, const QString &appKey,
                        const QString &appUri, const QString &appExt, const QString &appInput);
    /** resources/read reply for an MCP-App template (key -> template HTML; empty = failure). */
    void appResource(const QString &appKey, const QString &html);

    // Requests the server asks US to answer
    void permissionRequest(const QString &toolCallId, const QString &title,
                           const QString &detail, const QVariantList &options);

    // Lists / config
    void sessionsReady(const QVariantList &sessions);
    void configReady(const QVariantList &options);

    void usageUpdate(int used, int size, double cost, const QString &currency);
    void sessionTouched(const QString &sessionId, const QString &title, const QString &updatedAt);
    /** Reply to probeSession(): updatedAt/messageCount, or updatedAt="" count=-1 on error. */
    void sessionProbe(const QString &sessionId, const QString &updatedAt, int messageCount);
    /** Tool names active in the current session (reply to listTools), e.g. `developer__shell`. */
    void toolsReady(const QVariantList &names);
    /** Full extension profiles (reply to listConfigExtensions): {name,type,attrib,enabled,raw,availableTools}. */
    void extensionsReady(const QVariantList &extensions);
    /** Names of the extensions enabled in the current session (reply to listSessionExtensions). */
    void sessionExtensionsReady(const QStringList &names);
    /** Project list (reply to listProjects): {id,name,path,description,root}. */
    void projectsReady(const QVariantList &projects);
    /** Recipes list (reply to listRecipes). */
    void recipesReady(const QVariantList &recipes);
    /** Schedules list (reply to listSchedules). */
    void schedulesReady(const QVariantList &schedules);
    /** Skills list (reply to listSkills): {name,description,path,global,writable,content}. */
    void skillsReady(const QVariantList &skills);
    /** A single config.yaml value (reply to readConfig): key -> value. */
    void serverConfigValue(const QString &key, const QString &value);
    /** Live model names for a provider (reply to listSupportedModels). */
    void supportedModelsReady(const QString &providerId, const QStringList &models);
    /** session/export reply: the session serialized for backup/sharing. */
    void exportResult(const QString &data);
    /** Goose-custom status line (currently compaction progress/notice text). */
    void compactionStatus(const QString &message);
    /** Per-message tok/s + cost (goose MessageUsageData; camelCase on the wire). */
    void messageUsage(const QVariantMap &usage);
    /** Slash-command names the session accepts (available_commands_update). */
    void commandsReady(const QStringList &commands);
    /** The session's mode changed server-side (current_mode_update) — patch, don't clobber. */
    void modeChanged(const QString &modeId);
    /** A turn is live and steerable — `runId` names it; empty means the run ended. */
    void activeRunChanged(const QString &sessionId, const QString &runId);

private:
    AcpTransport *m_transport = nullptr;
    int m_nextId = 1;
    QHash<int, QString> m_pending; // request id -> method tag
    QList<QPair<QString, QString>> m_desired;
    QString m_resumeId;
    QString m_resumeCwd;
    QString m_sessionId;
    bool m_replaying = false;
    bool m_browseOnly = false;

    QString m_wantCwd;    // cwd for a brand-new session
    QString m_desiredRecipeId;  // recipe id for a brand-new session
    QString m_activeRunId;      // set from session_info_update's _meta.goose.activeRunId

    int rpc(const QString &method, const QJsonObject &params, const QString &tag = QString());
    void wireTransport(AcpTransport *transport);
    void send(const QJsonObject &frame);
    void onOpen();
    void onMessage(const QByteArray &data);
    void handle(const QJsonObject &obj);
    void response(int id, const QJsonObject &result, const QJsonObject &error);
    void notification(const QString &method, const QJsonObject &params);
    void standardUpdate(const QJsonObject &params);
    void gooseUpdate(const QJsonObject &params);
    void serverRequest(const QString &method, int id, const QJsonObject &params);

    void startNewSession();
    void loadSession();

    // parsers
    QVariantList parseSessions(const QJsonObject &result);
    QVariantList parseConfig(const QJsonObject &result);
    QVariantList parseExtensions(const QJsonObject &result);
    QVariantList parseProjects(const QJsonObject &result);
    QVariantList parseRecipes(const QJsonObject &result);
    QVariantList parseSchedules(const QJsonObject &result);
    QVariantList parseSkills(const QJsonObject &result);
    void applyDesired(const QVariantList &options);

    QJsonObject makeRequest(const QString &method, const QJsonObject &params, int id);
    friend class Manager;
};
