#pragma once

#include <QObject>
#include <QHash>
#include <QSet>
#include <QJsonObject>
#include <QSettings>
#include <QVariant>
#include <QVariantList>

class CoreBridge;
class AppBridgeServer;
class RoamListModel;
class SessionListModel;
class MessageListModel;
class QTimer;

/**
 * Process-scoped owner of the chat state, exposed to QML as a context property
 * named `Mgr`. THE THIN CLIENT: the grouse-core library owns the connection,
 * the transcript, streaming and reconnect; this object keeps no wire client.
 * Every Q_INVOKABLE / Q_PROPERTY keeps its exact QML-visible name and
 * semantics; its body is now a call into the corresponding `grouse_*` intent
 * bridged through CoreBridge (dlopen'ed libgrouse_core.so). Core events arrive
 * via the listener table, are marshalled onto the Qt main thread, and drive the
 * models with identical observable behavior to the old local ACP client.
 */
/** May the wire carrying `lostSessionId` release the in-flight turn?
 *
 *  Only the wire that OWNS the turn may: a drop in another chat, or on the main
 *  socket while a peer owns the turn, must leave it alone. With no recorded owner
 *  (the prompt was armed but its session was not stamped), ownership falls back to
 *  the chat on screen — the one whose composer is stuck. Android parity:
 *  `ConnectionManager.Companion.turnOwnerMatches`.
 */
inline bool turnOwnerMatches(const QString &promptingSessionId,
                             const QString &currentSessionId,
                             const QString &lostSessionId)
{
    const QString owner = promptingSessionId.isEmpty() ? currentSessionId
                                                       : promptingSessionId;
    return owner == lostSessionId;
}

class Manager : public QObject
{
    Q_OBJECT
    Q_PROPERTY(QString host READ host WRITE setHost NOTIFY settingsChanged)
    Q_PROPERTY(QString port READ port WRITE setPort NOTIFY settingsChanged)
    Q_PROPERTY(QString secretKey READ secretKey WRITE setSecretKey NOTIFY settingsChanged)
    Q_PROPERTY(bool useTls READ useTls WRITE setUseTls NOTIFY settingsChanged)
    Q_PROPERTY(bool autoConnectEnabled READ autoConnectEnabled WRITE setAutoConnectEnabled NOTIFY settingsChanged)
    /** Desktop notifications for events the client already knows from its own
     *  connection (turn finished, approval needed, a session touched elsewhere). */
    Q_PROPERTY(bool notificationsEnabled READ notificationsEnabled WRITE setNotificationsEnabled NOTIFY settingsChanged)
    /** Filter the provider pickers to the ones the server reports as configured. The
     *  inventory is goose's own (`providers/list`, `configured` flag); the current pick is
     *  always kept so the combo can't appear to switch provider on its own. */
    Q_PROPERTY(bool configuredProvidersOnly READ configuredProvidersOnly WRITE setConfiguredProvidersOnly NOTIFY settingsChanged)
    Q_PROPERTY(QStringList configuredProviders READ configuredProviders NOTIFY providersChanged)
    Q_PROPERTY(QString workingDir READ workingDir WRITE setWorkingDir NOTIFY settingsChanged)
    /** Roam (iroh peer transport) is opt-in: its sidebar tab and stored peers
     *  stay dormant until the user enables it in Settings. */
    Q_PROPERTY(bool roamEnabled READ roamEnabled WRITE setRoamEnabled NOTIFY settingsChanged)
    Q_PROPERTY(QString status READ status NOTIFY statusChanged)
    Q_PROPERTY(bool online READ online NOTIFY onlineChanged)
    /** Is a live wire carrying the chat ON SCREEN? For a peer-owned chat the main
     *  socket's `online` is the wrong question — it rides its own roam connection. */
    Q_PROPERTY(bool wireUpForCurrentChat READ wireUpForCurrentChat NOTIFY wireUpChanged)
    Q_PROPERTY(bool prompting READ prompting NOTIFY promptingChanged)
    Q_PROPERTY(QObject* messageModel READ messageModel CONSTANT)
    Q_PROPERTY(QObject* roamModel READ roamModel CONSTANT)
    Q_PROPERTY(QVariant sessions READ sessions NOTIFY sessionsChanged)
    Q_PROPERTY(QVariant projects READ projects NOTIFY projectsChanged)
    Q_PROPERTY(QVariant recipes READ recipes NOTIFY recipesChanged)
    Q_PROPERTY(QVariant schedules READ schedules NOTIFY schedulesChanged)
    Q_PROPERTY(QObject* sessionsModel READ sessionsModel CONSTANT)
    Q_PROPERTY(QVariant config READ config NOTIFY configChanged)
    Q_PROPERTY(QVariant tools READ tools NOTIFY toolsChanged)
    Q_PROPERTY(QVariant toolGroups READ toolGroups NOTIFY toolGroupsChanged)
    Q_PROPERTY(QVariant globalExtensions READ globalExtensions NOTIFY globalExtensionsChanged)
    Q_PROPERTY(QString currentSessionTitle READ currentSessionTitle NOTIFY currentSessionChanged)
    Q_PROPERTY(QString currentSessionId READ currentSessionId NOTIFY currentSessionChanged)
    /** True while the chat area shows the landing page (no conversation committed). */
    Q_PROPERTY(bool landingPage READ landingPage NOTIFY landingChanged)
    // --- chat parity state (mirrors the Android client's ConnectionManager) ---
    Q_PROPERTY(int queuedCount READ queuedCount NOTIFY queuedChanged)
    /** The running turn's run id for THIS session (the steer key), empty when
     *  no turn is live or the live run belongs to another session. */
    Q_PROPERTY(QString activeRunId READ activeRunId NOTIFY activeRunIdChanged)
    Q_PROPERTY(bool compacting READ compacting NOTIFY compactingChanged)
    Q_PROPERTY(int contextUsed READ contextUsed NOTIFY contextChanged)
    Q_PROPERTY(int contextSize READ contextSize NOTIFY contextChanged)
    Q_PROPERTY(QVariant availableCommands READ availableCommands NOTIFY commandsChanged)
    Q_PROPERTY(QVariant serverConfig READ serverConfig NOTIFY serverConfigChanged)
    Q_PROPERTY(QVariant supportedModels READ supportedModels NOTIFY supportedModelsChanged)
    Q_PROPERTY(QVariant skills READ skills NOTIFY skillsChanged)

public:
    /// Compiled-in QtWebEngine availability (main() sets it before the QML
    /// engine loads; the UI reads it via Q_INVOKABLE inlineAppsEnabled()).
    static void setInlineAppsSupported(bool on);
    static bool inlineAppsSupported();
    explicit Manager(QObject *parent = nullptr);

    QString host() const;
    QString port() const;
    QString secretKey() const;
    bool useTls() const;
    bool autoConnectEnabled() const;
    bool notificationsEnabled() const;
    bool configuredProvidersOnly() const;
    QStringList configuredProviders() const { return m_configuredProviders; }
    QString workingDir() const;
    bool roamEnabled() const;
    void setHost(const QString &v);
    void setPort(const QString &v);
    void setSecretKey(const QString &v);
    void setUseTls(bool v);
    void setAutoConnectEnabled(bool v);
    void setNotificationsEnabled(bool v);
    void setConfiguredProvidersOnly(bool v);
    void setWorkingDir(const QString &v);
    void setRoamEnabled(bool v);
    /** The ACP endpoint URL the configured host/port/key map to ("wss://host:port/acp"). */
    QString wsUrl() const;
    QString status() const { return m_status; }
    bool online() const { return m_online; }
    bool wireUpForCurrentChat() const;
    bool prompting() const { return m_prompting; }
    QObject* messageModel() const;
    QVariant sessions() const { return m_sessions; }
    QVariant projects() const { return m_projects; }
    QVariant recipes() const { return m_recipes; }
    QVariant schedules() const { return m_schedules; }
    QObject* sessionsModel() const;
    QVariant config() const { return m_config; }
    QVariant tools() const { return m_tools; }
    /** Grouped per-session tool view for the tools panel: ext -> [{name,on}] with enable state. */
    QVariant toolGroups() const;
    QVariant globalExtensions() const;
    QString currentSessionTitle() const;
    QString currentSessionId() const { return m_currentSessionId; }
    bool landingPage() const { return m_landing; }

    Q_INVOKABLE void connectToServer();
    Q_INVOKABLE void autoConnect();
    Q_INVOKABLE void disconnect();
    /** Connect over an iroh roam stream (direct peer, no host/port). The
     *  device identity is generated and stored on first use. The peer is
     *  ADDED alongside the main connection (both stay live); sessions appear
     *  under the peer's label in the Roam sidebar tab. */
    Q_INVOKABLE void connectRoam(const QString &card, const QString &label);
    Q_INVOKABLE void disconnectRoam(const QString &label);
    /** Re-dial + re-list every persisted roam peer (stored cards survive restart). */
    void restoreRoamPeers();
    void persistRoamCard(const QString &label, const QString &card);
    void forgetRoamCard(const QString &label);
    /** Push the advertised (QSettings) identity into the core so dials use the accepted key. */
    void syncRoamIdentityToCore();
    /** Open a session on a roam peer; the peer becomes the active connection
     *  for chat (prompt/tools/extensions route to it until a Main session is
     *  opened). */
    Q_INVOKABLE void openRoamSession(const QString &label, const QString &sessionId, const QString &cwd);
    /** Create a new chat on a roam peer (uses the config working dir). */
    Q_INVOKABLE void newRoamSession(const QString &label);
    /** Create a new chat on a roam peer in a caller-chosen dir; blank falls
     *  back to the config working dir. */
    Q_INVOKABLE void newRoamSessionIn(const QString &label, const QString &cwd);
    Q_INVOKABLE void toggleRoamPeer(const QString &label);
    /** The device's iroh identity (base64 secret), generated + persisted. */
    Q_INVOKABLE QString roamIdentity();
    /** Hex public key of the stored identity — what a host sees in peers list. */
    Q_INVOKABLE QString roamPublicKey() const;
    /** This device's shareable roam connection card (`goose+roam://` + base64url
     *  JSON: version 1, endpoint = this public key, no relay URLs — LAN-direct).
     *  A host pastes this to `roam peers accept` to reach and dial this device. */
    Q_INVOKABLE QString roamCard() const;
    /** True when the active session lives on a roam peer. */
    Q_INVOKABLE bool onRoamSession() const { return !m_activePeerLabel.isEmpty(); }
    /** Copy text to the system clipboard (QML has no OS clipboard access). */
    Q_INVOKABLE void copyToClipboard(const QString &s);
    Q_INVOKABLE void setActiveTab(const QString &tab);
    /** Sidebar model for the Roam tab (endpoint headers + sessions). */
    QObject *roamModel() const;
    /**
     * Probe the configured endpoint WITHOUT disturbing the live chat: drives a
     * real connect through the core and reports the resulting reachability +
     * secret-key auth + ACP handshake via the connectionTested signal.
     */
    Q_INVOKABLE void testConnection();
    /** Send a message. `files` is a list of LOCAL file paths; they are read, base64-encoded,
     *  and attached as content blocks (images as image blocks, everything else as embedded
     *  resources). The Prompt is built here (pure UI) and handed to grouse_send_prompt. */
    Q_INVOKABLE void sendPrompt(const QString &text, const QVariantList &files = QVariantList());
    /** Open the native KDE file picker (any file type, multi-select) and return chosen paths. */
    Q_INVOKABLE QStringList pickAttachmentFiles();
    Q_INVOKABLE void cancelTurn();
    /** Compact the conversation history (goose /compact command). */
    Q_INVOKABLE void compactConversation();
    /** Serialize a session and write it to `filePath` (JSON). */
    Q_INVOKABLE void exportSessionTo(const QString &sessionId, const QString &filePath);
    Q_INVOKABLE void respondPermission(const QString &toolCallId, const QString &optionId);
    Q_INVOKABLE void setConfigOption(const QString &id, const QString &value);
    Q_INVOKABLE void refreshSessions();
    Q_INVOKABLE void openSession(const QString &sessionId);
    Q_INVOKABLE void newChat();
    /** Step off the landing page into the staging chat (provider/model already chosen there). */
    Q_INVOKABLE void beginChat();
    Q_INVOKABLE void renameSession(const QString &sessionId, const QString &title);
    Q_INVOKABLE void archiveSession(const QString &sessionId);
    Q_INVOKABLE void unarchiveSession(const QString &sessionId);
    Q_INVOKABLE void deleteSession(const QString &sessionId);
    // --- projects -------------------------------------------------------------
    Q_INVOKABLE void refreshProjects();
    /** Ask the server for its provider inventory (which are configured, and their models). */
    Q_INVOKABLE void refreshProviders();
    Q_INVOKABLE void createProject(const QString &name);
    Q_INVOKABLE void deleteProject(const QString &nameOrPath);
    Q_INVOKABLE void moveSessionToProject(const QString &sessionId, const QString &projectId);
    Q_INVOKABLE void newChatInProject(const QString &projectId);
    // --- recipes & schedules --------------------------------------------------
    Q_INVOKABLE void refreshRecipes();
    Q_INVOKABLE void runRecipe(const QString &id);
    Q_INVOKABLE void scheduleRecipe(const QString &id, const QString &cron);
    Q_INVOKABLE void deleteRecipe(const QString &id);
    Q_INVOKABLE void saveRecipe(const QString &id, const QString &recipeJson);
    Q_INVOKABLE void setSchedulePaused(const QString &scheduleId, bool paused);
    Q_INVOKABLE void runScheduleNow(const QString &scheduleId);
    // --- per-session tool management ------------------------------------------
    Q_INVOKABLE void refreshToolGroups();
    Q_INVOKABLE void discoverToolGroup(const QString &extName);
    Q_INVOKABLE void setSessionExtensionEnabled(const QString &extName, bool enabled);
    Q_INVOKABLE void setSessionToolEnabled(const QString &extName, const QString &toolName, bool on);
    // --- global (config.yaml) extensions — defaults for NEW sessions --------------
    Q_INVOKABLE void refreshGlobalExtensions();
    Q_INVOKABLE void setGlobalExtensionEnabled(const QString &extName, bool enabled);
    Q_INVOKABLE void setGlobalToolEnabled(const QString &extName, const QString &toolName, bool on);
    // --- skills ---------------------------------------------------------------
    Q_INVOKABLE void refreshSkills();
    Q_INVOKABLE void saveSkill(const QString &path, const QString &name,
                               const QString &description, const QString &content);
    Q_INVOKABLE void deleteSkill(const QString &path);
    // --- projects -------------------------------------------------------------
    // Edit a project's instructions (its projects/<name>.md content). Whole
    // source replace via sources/update; the core re-lists on the reply.
    Q_INVOKABLE void saveProject(const QString &path, const QString &name,
                                 const QString &description, const QString &content);
    // --- server config (providers) --------------------------------------------
    Q_INVOKABLE void setServerConfig(const QString &key, const QString &value);
    /** Write this client's UnifiedPush endpoint where an operator's own sender can find
     *  it. Deliberately outside the client contract: nothing in Grouse assumes a server
     *  sender exists, and a stock goose server never pushes (docs/NOTIFICATIONS.md). */
    Q_INVOKABLE void publishPushEndpoint(const QString &url);
    /** The shared notification policy, in the core (`notify.rs`): decode a push payload,
     *  and decide whether it warrants a notification given what this client knows. JSON
     *  in, JSON out — literally the same functions the Android client calls over uniffi,
     *  so the two can't disagree about what a payload means or when to interrupt. */
    /** The last turn this client announced, so the same event arriving as a push moments
     *  later is not announced twice (core notify.rs dedupes on it). Empty when nothing was
     *  announced — a process that just started has announced nothing. */
    QString announcedTurnSession() const { return m_announcedTurnSession; }
    int announcedTurnSecsAgo() const;
    Q_INVOKABLE QString pushParse(const QString &raw) const;
    Q_INVOKABLE QString pushDecide(const QString &envelopeJson,
                                   const QString &contextJson) const;
    Q_INVOKABLE void readServerConfig(const QString &key);
    Q_INVOKABLE void refreshSupportedModels(const QString &providerId);
    Q_INVOKABLE QString permissionToolCallId() const { return m_permToolCallId; }
    Q_INVOKABLE QString permissionTitle() const { return m_permTitle; }
    Q_INVOKABLE QVariantList permissionOptions() const { return m_permOptions; }

    /// MCP-App surface. These MUST be public: QML only resolves a context-property
    /// method when the meta-object marks it public. All three were originally under
    /// `private:`, so QML reported "Property 'inlineAppsEnabled' … is not a function"
    /// (and appViewUrl/openAppInHtml silently failed) and every MCP App fell back to
    /// the browser-handoff chip.
    Q_INVOKABLE void openAppInHtml(const QString &appKey);
    Q_INVOKABLE QString appViewUrl(const QString &appKey);
    Q_INVOKABLE bool inlineAppsEnabled() const;

    /// Per-session MCP-App dock for the CURRENT chat: a map with keys
    /// "top"/"bottom" (appKey, empty when the slot is free) and
    /// "topRatio"/"bottomRatio" (pane height as a fraction of the chat column).
    /// Persisted per session in QSettings; the UI re-reads it whenever the
    /// session changes (the binding also depends on currentSessionId).
    Q_PROPERTY(QVariant pinnedApps READ pinnedApps NOTIFY pinnedAppsChanged)
    QVariant pinnedApps() const;
    /// Dock `appKey` in `slot` ("top"/"bottom"), replacing the occupant.
    Q_INVOKABLE void pinApp(const QString &appKey, const QString &slot);
    Q_INVOKABLE void unpinApp(const QString &slot);
    Q_INVOKABLE void setPinRatio(const QString &slot, qreal ratio);
    /// Is `appKey` docked (in either slot) for the current session?
    Q_INVOKABLE bool appPinned(const QString &appKey) const;
    /// "top" | "bottom" | "" — which slot holds `appKey` for the current session.
    Q_INVOKABLE QString pinnedSlot(const QString &appKey) const;

    /// The item window's pagination cursor (docs/TRANSCRIPT_MODEL.md): the id of
    /// the oldest item the core has emitted, and whether it can produce older
    /// ones. Phase 2 drives "load earlier"; the property exists so the UI can
    /// already bind to it.
    Q_PROPERTY(bool itemHasOlder READ itemHasOlder NOTIFY itemWindowChanged)
    bool itemHasOlder() const { return m_itemHasOlder; }

    /// Ask the core for older items (docs/TRANSCRIPT_MODEL.md). Outcomes arrive
    /// as `on_item` Upserts + a refreshed `Window`; there is no client buffer.
    Q_INVOKABLE void loadOlder(int count);

    int queuedCount() const { return m_pendingQueue.size(); }
    QString activeRunId() const { return m_activeRunId; }
    bool compacting() const { return m_compacting; }
    int contextUsed() const { return m_contextUsed; }
    int contextSize() const { return m_contextSize; }
    QVariant availableCommands() const { return m_availableCommands; }
    QVariant serverConfig() const { return m_serverConfig; }
    QVariant supportedModels() const { return m_supportedModels; }
    QVariant skills() const { return m_skills; }

    // ---- CoreBridge event entry points ---------------------------------------
    // Invoked (on the main thread) by the CoreBridge listener table. They are
    // the ONLY route from the wire into the Manager/models.
    void coreOnStatus(const QString &json);
    void coreOnSessions(const QString &json);
    void coreOnItem(const QString &json);
    void coreOnUsage(qint64 used, qint64 size, double cost, const QString &currency);
    void coreOnRunEnded(const QString &stopReason);
    void coreOnConfig(const QString &json);
    void coreOnPermission(const QString &json);
    void coreOnSessionTouched(const QString &sid, const QString &title, const QString &u);
    void coreOnProjects(const QString &json);
    void coreOnRoamPeerStatus(const QString &label, const QString &status);
    void coreOnRoamSessions(const QString &label, const QString &json);
    void coreOnPeerNewSession(const QString &label, const QString &sid);
    void coreOnActiveRun(const QString &sid, const QString &runId);
    void coreOnCommands(const QString &json);
    void coreOnExport(const QString &data);
    void coreOnRecipeParams(const QString &parameters);
    void coreOnElicitation(const QString &schema);
    void coreOnCompactionStatus(const QString &message);
    void coreOnMessageUsage(std::uint64_t outTok, std::uint64_t elapsedMs,
                            std::uint64_t ttftMs, double cost);
    void coreOnAppResource(const QString &key, const QString &html);
    void coreOnRecipes(const QString &json);
    void coreOnSchedules(const QString &json);
    void coreOnUnstableProjects(const QString &json);
    void coreOnSkills(const QString &json);
    void coreOnTools(const QString &sid, const QString &json);
    void coreOnExtensions(const QString &json);
    void coreOnSessionExtensions(const QString &sid, const QString &json);
    void coreOnConfigValue(const QString &key, const QString &value);
    void coreOnSupportedModels(const QString &provider, const QString &json);
    void coreOnProviders(const QString &json);
    void coreOnSessionProbe(const QString &sid, const QString &u, qint64 n);
    void coreOnToolResult(const QString &text, int isError);
    void coreOnError(const QString &method, const QString &message);

signals:
    void settingsChanged();
    void statusChanged();
    void onlineChanged();
    void wireUpChanged();
    void promptingChanged();
    void messagesChanged();
    void sessionsChanged();
    void projectsChanged();
    void recipesChanged();
    void schedulesChanged();
    void configChanged();
    void toolsChanged();
    void toolGroupsChanged();
    void globalExtensionsChanged();
    void currentSessionChanged();
    /** The current session's pinned MCP-App dock changed (pin/unpin/resize). */
    void pinnedAppsChanged();
    /** The item window's cursor changed (oldest id / has_older). */
    void itemWindowChanged();
    /** `count` older rows were prepended to the model; the view re-anchors. */
    void olderRowsPrepended(int count);

    void permissionRequested();
    void landingChanged();
    void queuedChanged();
    void activeRunIdChanged();
    /** The provider inventory arrived (or changed): pickers filter on it. */
    void providersChanged();
    void compactingChanged();
    void contextChanged();
    void commandsChanged();
    void serverConfigChanged();
    void supportedModelsChanged();
    void skillsChanged();
    /** Result of testConnection(): reachability + secret-key auth + ACP handshake. */
    void connectionTested(bool ok, const QString &message);

private:
    void setStatus(const QString &s);
    void setOnline(bool o);
    /** The final assistant text in the transcript, elided — the body of the
     *  "turn finished" notification (the phone's nudge can't carry this). */
    QString lastAssistantText() const;
    /** Ask the shared policy (core notify.rs) whether this finished turn warrants a
     *  notification, and show it. The desktop announces any turn — it is the only client
     *  here — and contributes the session title, which a push to a sleeping phone cannot. */
    void notifyTurnFinished();
    QStringList m_configuredProviders;
    QString m_announcedTurnSession;
    qint64 m_announcedTurnAtMs = 0;
    void onSessionTouched(const QString &sid, const QString &title, const QString &updatedAt);
    void onAppResource(const QString &appKey, const QString &html);
    /// One rich item (docs/TRANSCRIPT_MODEL.md) as a model row. Kinds become the
    /// delegate's roles; a text row's html is computed once, when the core's
    /// finalizing Upsert arrives.
    QVariantMap rowFromItem(const QJsonObject &item);
    /// A ui/message arrived from an app tab: post it into the chat the app came from.
    void onAppMessage(const QString &sessionId, const QString &text);
    void onReady(const QString &sessionId);
    void onSessions(const QVariantList &sessions);
    void onProjects(const QVariantList &projects);
    void onRecipes(const QVariantList &recipes);
    void onSchedules(const QVariantList &schedules);
    void onConfig(const QVariantList &config);
    void onTools(const QVariantList &tools);
    void onExtensions(const QVariantList &extensions);
    /// restricted = attached keys carrying a session-scoped available_tools
    /// allowlist (their active tool set is NOT the full catalogue).
    void onSessionExtensions(const QStringList &names, const QSet<QString> &restricted = {});
    /// Merge a cached catalogue for this session's extensions into m_toolCatalog
    /// (keys already known this run win — the file is best-effort memory).
    void loadToolCache(const QString &sessionId);
    void onPermission(const QString &toolCallId, const QString &title,
                      const QString &detail, const QVariantList &options);
    void onError(const QString &text, bool background);
    void onSkills(const QVariantList &skills);
    void onServerConfigValue(const QString &key, const QString &value);
    void onSupportedModels(const QString &providerId, const QStringList &models);
    void onExportResult(const QString &data);
    void onCompactionStatus(const QString &message);
    void onMessageUsage(const QVariantMap &usage);
    void onCommands(const QStringList &commands);
    void onModeChanged(const QString &modeId);
    void onActiveRunChanged(const QString &sessionId, const QString &runId);
    void onUsage(int used, int size, double cost, const QString &currency);
    QString formatUsage(const QVariantMap &usage) const;

    // queued-send / steering
    struct PendingSend { QString text; QVariantList images; };
    void dispatchSend(const QString &text, const QVariantList &blocks);
    void enqueue(const PendingSend &p);
    void flushQueue();
    /** Assign the steer key, emitting activeRunIdChanged only on a real change. */
    void setActiveRunId(const QString &runId);
    /** The wire that owns the in-flight turn is gone for good: release it so the
     *  send queue can drain and the UI stops claiming a turn is running (Android
     *  parity, ConnectionManager.releaseTurnForLostWire). */
    void releaseTurnForLostWire(const QString &lostSessionId);
    /** Turn local file paths into ACP prompt content blocks (image vs embedded resource). */
    QVariantList buildAttachmentBlocks(const QVariantList &paths);
    /** Coalesce messagesChanged emissions while a turn streams (see m_updateTimer). */
    void requestMessagesUpdate();
    QString activeSessionId() const;
    /** Build the ServerConfig JSON `grouse_connect` consumes (consumes m_pendingRecipeId). */
    QString serverConfigJson();
    /** Build the serde Prompt JSON `grouse_send_prompt` consumes from text + ACP blocks. */
    QString promptJson(const QString &text, const QVariantList &blocks) const;
    /** Persist tool/extension catalog state for a session (best-effort UI mirror). */
    void saveToolCache(const QString &sessionId) const;

    // streaming bubble tracker
    QString m_streamRole;
    QString m_streamMsgId;
    int m_currentIndex = -1;

    QSettings m_store;
    CoreBridge *m_bridge = nullptr;
    QString m_activePeerLabel;      // empty = main connection owns the active session
    RoamListModel *m_roamModel = nullptr;
    SessionListModel *m_sessionsModel = nullptr;
    MessageListModel *m_messageModel = nullptr;

    // testConnection state (core-driven; never touches the live chat's models)
    bool m_testPending = false;
    QString m_testSid;

    QString m_status = QStringLiteral("not connected");
    bool m_online = false;
    bool m_prompting = false;
    /// Which session owns the in-flight prompt. A wire may only release the turn it
    /// owns; empty falls back to the session on screen (the stuck composer).
    QString m_promptingSessionId;

    QVariantList m_sessions;
    QVariantList m_projects;
    QVariantList m_recipes;
    QVariantList m_schedules;
    QVariantList m_config;    /// Tool names active in the current session (`extension__tool`, per-conversation).
    QStringList m_tools;
    /// projectId to file the next freshly-created session into (newChatInProject).
    QString m_pendingProjectFiling;
    /// Recipe id for the next fresh session (runRecipe); consumed by newChat().
    QString m_pendingRecipeId;
    /// Monotonic id counter for locally-created bubbles (the core supplies its
    /// own ids for committed transcript rows; these are for UI-only rows).
    int m_seq = 0;
    /// Name of the extension whose full tool catalog is currently being discovered.
    QString m_discoveringExt;
    /// Catalog captured during a tool-group peek; committed only once the
    /// session-extensions re-list proves the transient add actually attached
    /// (a failed-to-start extension lists zero tools just like a bare-named
    /// one does — the difference is whether it is in the session).
    QStringList m_discoveringFull;
    bool m_discoveringAttached = false;

    /// One configured extension's profile, as goose listed it (raw is the add-accept input).
    struct ExtDef {
        QString name;
        QString key;                // configKey / extensionKey — the identity for
                                    // sessionExts, tool prefixes, catalogs, and remove
        QString type;
        bool attrib = false;          // mcp-backed => tools are namespaced and individually toggleable
        bool enabled = true;          // global config.yaml enabled state (config/extensions/list)
        QStringList availableTools;   // current allowlist from ext.available_tools (empty = all)
        QJsonObject raw;              // verbatim listed extension object
    };
    QList<ExtDef> m_extDefs;
    /// Extensions enabled in the CURRENT session (KEYS — extensionKey/configKey, not
    /// display names: "Extension Manager" is keyed "extensionmanager").
    QStringList m_sessionExts;
    /// Keys of the attached rows that carry a session-scoped available_tools
    /// ALLOWLIST. An attached row WITHOUT one runs its extension unfiltered, so
    /// the session's active namespaced tools ARE its full catalogue — the
    /// expander gate can then answer ">=2?" without a peek at all.
    QSet<QString> m_sessionRestricted;
    /// Background catalogue sweep: keys of detached rows never peeked yet. One
    /// transient attach→list→detach at a time, only while the chat is idle;
    /// results persist (QSettings + the per-session tool cache), so each
    /// extension's full list is learned essentially once ever. This is what
    /// replaces the permanent "…" with a real "# tools" on every row.
    QStringList m_peekQueue;
    /// Rows whose peek failed within the CURRENT session (extension would not
    /// start) — the sweep stops hammering a down server every queue pass. Reset
    /// with the session (a later chat may find it back up); a manual arrow click
    /// always retries.
    QSet<QString> m_peekFailed;
    QTimer *m_peekTimer = nullptr;
    void buildPeekQueue();
    void doPeekStep();
    void persistCatalogs();
    /// Full tool catalog per discovered extension (extName -> tool names).
    QHash<QString, QStringList> m_toolCatalog;
    /// Fetched MCP-App templates, keyed appKey ("<ext>|<uri>"), for openAppInHtml.
    QHash<QString, QString> m_appHtml;
    /// Loopback host for apps opened in the browser (see appbridge.h).
    AppBridgeServer *m_appBridge = nullptr;
    int m_appTokenSeq = 0;
    /// appKey -> bridge URL (stable per appKey so the inline view reloads the
    /// same document; token changes when the template refetches).
    QHash<QString, QString> m_appUrls;
    QHash<QString, QString> m_appTokens;

    /// Lookup by extension KEY (configKey/extensionKey), not display name.
    const ExtDef *extDef(const QString &key) const;
    void setSessionTools(const QString &extName, const QStringList &allowed);
    void publishToolGroups();

    QString m_currentSessionId;
    QString m_currentSessionTitle;
    /// True while the chat area shows the landing page (no conversation committed).
    bool m_landing = true;
    bool m_roamRestored = false;   // persisted roam peers re-armed only once (at Ready)
    /// cwd of the freshly-opened chat, remembered so auto-connect can resume it.
    QString m_lastCwd;
    /// Coalesces messagesChanged while chunks stream (see requestMessagesUpdate).
    QTimer *m_updateTimer = nullptr;

    // chat parity state
    QList<PendingSend> m_pendingQueue;      // sends that must wait for the current turn / a session
    QString m_activeRunId;                  // live run id from session_info_update (steering)
    bool m_compacting = false;
    int m_contextUsed = 0, m_contextSize = 0;
    QVariantList m_availableCommands;
    QVariantMap m_serverConfig;             // global config.yaml values (providers)
    QVariantList m_supportedModels;
    QVariantList m_skills;
    QString m_pendingExportPath;            // where to write the next session/export reply
    QString m_itemOldestId;                 // item-window cursor (docs/TRANSCRIPT_MODEL.md)
    bool m_itemHasOlder = false;
    QList<QVariantMap> m_olderPending;      // older items collected during a loadOlder
    bool m_loadingOlder = false;

    // pending permission request
    QString m_permToolCallId;
    QString m_permTitle;
    QVariantList m_permOptions;
};
