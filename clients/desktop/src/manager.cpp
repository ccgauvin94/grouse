#include "manager.h"

#include "acpclient.h"
#include "markdown.h"
#include "messagelistmodel.h"
#include "roamlistmodel.h"
#include "roamtransport.h"
#include "sessionlistmodel.h"

#include <QDir>
#include <QFile>
#include <QFileDialog>
#include <QFileInfo>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QMetaObject>
#include <QMimeDatabase>
#include <QNetworkRequest>
#include <QSet>
#include <QSslConfiguration>
#include <QSslSocket>
#include <QStandardPaths>
#include <QTimer>
#include <QUrl>
#include <QtWebSockets/QWebSocket>

Manager::Manager(QObject *parent)
    : QObject(parent)
    , m_store(QSettings::UserScope, QStringLiteral("grouse"), QStringLiteral("grouse-desktop"))
{
    m_sessionsModel = new SessionListModel(this);
    m_messageModel = new MessageListModel(this);
    m_roamModel = new RoamListModel(this);
    // Coalesces server session_info_update notifications: another client's turn
    // can bump a session repeatedly, and each bump must not trigger a full resync.
    m_touchDebounce = new QTimer(this);
    m_touchDebounce->setSingleShot(true);
    m_touchDebounce->setInterval(1500);
    connect(m_touchDebounce, &QTimer::timeout, this, &Manager::onTouchDebounced);
    m_updateTimer = new QTimer(this);
    m_updateTimer->setSingleShot(true);
    m_updateTimer->setInterval(50);
    connect(m_updateTimer, &QTimer::timeout, this, [this] { emit messagesChanged(); });
    // Auto-reconnect after an UNEXPECTED drop (not an explicit disconnect): exponential
    // backoff, capped at 6 attempts, reset once a session re-opens. Mirrors the Android
    // client's ensureConnected + turnResyncTick recovery at a desktop granularity.
    m_reconnectTimer = new QTimer(this);
    m_reconnectTimer->setSingleShot(true);
    connect(m_reconnectTimer, &QTimer::timeout, this, [this] {
        if (m_userDisconnect)
            return;
        ++m_reconnectAttempts;
        const QString cwd = m_sessionsModel->cwdFor(m_currentSessionId);
        if (!m_currentSessionId.isEmpty())
            resumeSession(m_currentSessionId, cwd.isEmpty() ? m_lastCwd : cwd);
        else
            connectToServer();
    });
}

void Manager::requestMessagesUpdate()
{
    if (m_deferMessageUpdates)
        return;
    if (!m_updateTimer->isActive())
        m_updateTimer->start();
}

QString Manager::host() const { return m_store.value("host", "192.168.1.5").toString(); }
QString Manager::port() const { return m_store.value("port", "3284").toString(); }
QString Manager::secretKey() const { return m_store.value("secret", "").toString().trimmed(); }
bool Manager::useTls() const { return m_store.value("wss", true).toBool(); }
bool Manager::autoConnectEnabled() const { return m_store.value("auto_connect", true).toBool(); }
QString Manager::workingDir() const { return m_store.value("cwd", "").toString(); }

void Manager::setHost(const QString &v) { m_store.setValue("host", v); emit settingsChanged(); }
void Manager::setPort(const QString &v) { m_store.setValue("port", v); emit settingsChanged(); }
void Manager::setSecretKey(const QString &v) { m_store.setValue("secret", v.trimmed()); emit settingsChanged(); }
void Manager::setUseTls(bool v) { m_store.setValue("wss", v); emit settingsChanged(); }
void Manager::setAutoConnectEnabled(bool v) { m_store.setValue("auto_connect", v); emit settingsChanged(); }
void Manager::setWorkingDir(const QString &v)
{
    m_store.setValue("cwd", v.trimmed().remove(QRegularExpression(QStringLiteral("/+$"))));
    emit settingsChanged();
}

QString Manager::wsUrl() const
{
    // goosed serves a self-signed TLS cert on its ACP port, so wss is the norm;
    // ws (raw) is only for a server that doesn't terminate TLS itself.
    return QStringLiteral("%1://%2:%3/acp")
        .arg(useTls() ? QStringLiteral("wss") : QStringLiteral("ws"),
             host().trimmed(), port().trimmed());
}

QString Manager::currentSessionTitle() const
{
    if (!m_currentSessionTitle.isEmpty())
        return m_currentSessionTitle;
    return m_currentSessionId.isEmpty() ? QStringLiteral("New chat") : m_currentSessionId;
}

QObject* Manager::sessionsModel() const { return m_sessionsModel; }
QObject* Manager::messageModel() const { return m_messageModel; }

void Manager::setStatus(const QString &s)
{
    if (m_status == s)
        return;
    m_status = s;
    emit statusChanged();
}

void Manager::setOnline(bool o)
{
    if (m_online == o)
        return;
    m_online = o;
    emit onlineChanged();
}

void Manager::ensureClient()
{
    if (m_client)
        return;
    m_client = new AcpClient(this);
    wireClient(m_client, -1);
}

/** Wire one ACP client's signals. peerIndex < 0 = the main connection;
 *  otherwise a roam peer (peerLabel names it). Chat/session-scoped signals
 *  only forward to the shared handlers while this client owns the active
 *  session, so the main connection and a peer can both stay live with the
 *  chat page bound to whichever session the user last opened. */
void Manager::wireClient(AcpClient *c, int peerIndex, const QString &peerLabel)
{
    const bool isMain = peerIndex < 0;
    auto active = [this, isMain, peerLabel] {
        return isMain ? m_activePeerLabel.isEmpty() : m_activePeerLabel == peerLabel;
    };

    if (isMain) {
        connect(c, &AcpClient::statusChanged, this, [this](const QString &s) {
            setStatus(s);
            // An unexpected drop (disconnected / error) after a session was live should heal
            // itself; an explicit disconnect() or a still-in-flight manual connect must not.
            if (!m_userDisconnect && !m_currentSessionId.isEmpty()
                && (s == QStringLiteral("disconnected") || s.startsWith(QLatin1String("error:"))))
                maybeReconnect();
        });
        // Global catalogs are the MAIN server's; roam peers never feed them.
        connect(c, &AcpClient::sessionsReady, this, &Manager::onSessions);
        connect(c, &AcpClient::projectsReady, this, &Manager::onProjects);
        connect(c, &AcpClient::recipesReady, this, &Manager::onRecipes);
        connect(c, &AcpClient::schedulesReady, this, &Manager::onSchedules);
        connect(c, &AcpClient::skillsReady, this, &Manager::onSkills);
        connect(c, &AcpClient::extensionsReady, this, &Manager::onExtensions);
        connect(c, &AcpClient::serverConfigValue, this, &Manager::onServerConfigValue);
        connect(c, &AcpClient::supportedModelsReady, this, &Manager::onSupportedModels);
    } else {
        connect(c, &AcpClient::statusChanged, this, [this, peerLabel](const QString &s) {
            const bool down = s == QStringLiteral("disconnected")
                || s.startsWith(QLatin1String("error:"));
            m_roamModel->setPeerStatus(peerLabel, s, !down);
        });
        connect(c, &AcpClient::sessionsReady, this, [this, peerLabel](const QVariantList &sessions) {
            for (RoamPeer &p : m_roamPeers) {
                if (p.label == peerLabel) {
                    p.sessions = sessions;
                    break;
                }
            }
            m_roamModel->setPeerSessions(peerLabel, sessions);
        });
    }

    // Remote-change tracking: another client touching a session surfaces as
    // session_info_update (touched) + the cheap probe reply; the Manager
    // debounces and replays the ACTIVE session when it moved.
    connect(c, &AcpClient::sessionTouched, this,
            [this, c](const QString &sid, const QString &title, const QString &u) {
        onSessionTouched(c, sid, title, u);
    });
    connect(c, &AcpClient::sessionProbe, this,
            [this](const QString &sid, const QString &u, int n) { onSessionProbe(sid, u, n); });

    // Chat/session-scoped signals: forward only while this client is active.
    connect(c, &AcpClient::sessionReady, this, [this, active](const QString &id) { if (active()) onReady(id); });
    connect(c, &AcpClient::configReady, this, [this, active](const QVariantList &v) { if (active()) onConfig(v); });
    connect(c, &AcpClient::toolsReady, this, [this, active](const QVariantList &v) { if (active()) onTools(v); });
    connect(c, &AcpClient::sessionExtensionsReady, this, [this, active](const QStringList &v) { if (active()) onSessionExtensions(v); });
    connect(c, &AcpClient::agentChunk, this, [this, active](const QString &t, const QString &id) { if (active()) onAgentChunk(t, id); });
    connect(c, &AcpClient::userChunk, this, [this, active](const QString &t, const QString &id) { if (active()) onUserChunk(t, id); });
    connect(c, &AcpClient::thoughtChunk, this, [this, active](const QString &t) { if (active()) onThoughtChunk(t); });
    connect(c, &AcpClient::toolCall, this, [this, active](const QString &t, const QString &d, const QString &id) { if (active()) onToolCall(t, d, id); });
    connect(c, &AcpClient::toolCallUpdate, this, [this, active](const QString &id, const QString &st, const QString &o, bool l) { if (active()) onToolCallUpdate(id, st, o, l); });
    connect(c, &AcpClient::chartToolCall, this, [this, active](const QString &t, const QString &id, const QString &s) { if (active()) onChartToolCall(t, id, s); });
    connect(c, &AcpClient::mcpAppToolCall, this, [this, active](const QString &t, const QString &id, const QString &k, const QString &u, const QString &x, const QString &in) { if (active()) onMcpAppToolCall(t, id, k, u, x, in); });
    connect(c, &AcpClient::appResource, this, [this, active](const QString &k, const QString &h) { if (active()) onAppResource(k, h); });
    connect(c, &AcpClient::permissionRequest, this, [this, active](const QString &id, const QString &t, const QString &d, const QVariantList &o) { if (active()) onPermission(id, t, d, o); });
    connect(c, &AcpClient::usageUpdate, this, [this, active](int u, int sz, double c, const QString &cur) { if (active()) onUsage(u, sz, c, cur); });
    connect(c, &AcpClient::exportResult, this, [this, active](const QString &d) { if (active()) onExportResult(d); });
    connect(c, &AcpClient::compactionStatus, this, [this, active](const QString &m) { if (active()) onCompactionStatus(m); });
    connect(c, &AcpClient::messageUsage, this, [this, active](const QVariantMap &u) { if (active()) onMessageUsage(u); });
    connect(c, &AcpClient::commandsReady, this, [this, active](const QStringList &v) { if (active()) onCommands(v); });
    connect(c, &AcpClient::modeChanged, this, [this, active](const QString &m) { if (active()) onModeChanged(m); });
    connect(c, &AcpClient::activeRunChanged, this, [this, active](const QString &s, const QString &r) { if (active()) onActiveRunChanged(s, r); });
    connect(c, &AcpClient::runEnded, this, [this, active] {
        if (!active())
            return;
        finalizeCurrentMessage();
        m_prompting = false;
        m_compacting = false;
        m_activeRunId.clear();
        emit promptingChanged();
        emit compactingChanged();
        flushQueue();
    });
    connect(c, &AcpClient::error, this, [this, active](const QString &t, bool b) { if (active()) onError(t, b); });
}

void Manager::onSessionTouched(AcpClient *owner, const QString &sid, const QString &title,
                               const QString &updatedAt)
{
    // Keep the sidebar's updatedAt map fresh — it drives openSession's cache check.
    if (!updatedAt.isEmpty())
        m_sessionUpdatedAt.insert(sid, updatedAt);
    m_lastTouchedSid = sid;
    m_lastTouchedClient = owner;
    m_touchDebounce->start();
}

void Manager::onTouchDebounced()
{
    const QString sid = m_lastTouchedSid;
    if (sid.isEmpty())
        return;
    if (sid == m_currentSessionId) {
        // The active chat moved remotely. A resync cycle is already running
        // (its follow-up probes will catch the rest) — don't stack another.
        if (m_resyncTicks > 0)
            return;
        m_resyncTicks = 4;
        probeAndMaybeResync(sid);
    } else if (m_lastTouchedClient) {
        // Sidebar: refresh the owner's list so order/title/status pick it up.
        m_lastTouchedClient->listSessions();
    }
}

void Manager::probeAndMaybeResync(const QString &sid)
{
    if (m_prompting)
        return;   // our own turn owns the transcript; the session re-syncs on open
    AcpClient *c = activeClient();
    if (c && sid == m_currentSessionId)
        c->probeSession(sid);
}

void Manager::onSessionProbe(const QString &sid, const QString &updatedAt, int messageCount)
{
    if (sid != m_currentSessionId)
        return;   // stale probe for a session we left
    if (updatedAt.isEmpty() || messageCount < 0)
        return;   // probe failed; leave the transcript as-is
    const bool moved = updatedAt != m_syncStamp
        || (messageCount >= 0 && messageCount != m_syncCount);
    if (!moved)
        return;
    m_syncStamp = updatedAt;
    m_syncCount = messageCount;
    resyncCurrentSession();
    if (m_resyncTicks > 0) {
        --m_resyncTicks;
        // goose streams a turn only to the prompting client, so a snapshot can
        // cut a still-running turn short — re-probe a few times to catch it.
        QTimer::singleShot(8000, this, [this, sid] { probeAndMaybeResync(sid); });
    }
}

void Manager::resyncCurrentSession()
{
    const QString sid = m_currentSessionId;
    if (sid.isEmpty())
        return;
    AcpClient *c = activeClient();
    if (!c)
        return;
    // Light in-place replay: same state prep as openSession's stale path, but
    // no reconnect — the wire is already live.
    m_suppressReplay = false;
    m_deferMessageUpdates = true;
    m_messageModel->clear();
    m_currentIndex = -1;
    emit messagesChanged();
    setStatus(QStringLiteral("syncing…"));
    const QString cwd = m_sessionsModel->cwdFor(sid);
    c->setResumeSession(sid, cwd.isEmpty() ? m_lastCwd : cwd);
    c->openSessionNow();
}

void Manager::connectToServer()
{
    ensureClient();
    m_userDisconnect = false;
    if (m_online) {
        m_client->close();
    }
    const QString url = wsUrl();
    const QString key = secretKey();
    if (key.isEmpty()) {
        setStatus(QStringLiteral("no secret key"));
        return;
    }
    m_client->setDesiredOptions({});
    m_client->setDesiredCwd(workingDir());
    m_client->setResumeSession(QString(), QString());
    m_lastCwd = workingDir();
    m_client->connectTo(url, key);
}

void Manager::connectRoam(const QString &card, const QString &label)
{
    if (label.isEmpty()) {
        setStatus(QStringLiteral("roam: label required"));
        return;
    }
    // One peer per label: reconnect replaces the old one.
    for (const RoamPeer &p : m_roamPeers) {
        if (p.label == label) {
            disconnectRoam(label);
            break;
        }
    }
    const QString secret = roamIdentity();
    if (secret.isEmpty()) {
        setStatus(QStringLiteral("roam: no identity"));
        return;
    }
    auto *client = new AcpClient(this);
    client->setBrowseOnly(true);   // list the peer's sessions, don't auto-open
    m_roamPeers.append(RoamPeer{label, client});
    m_roamModel->addPeer(label);
    wireClient(client, m_roamPeers.size() - 1, label);
    client->connectRoam(secret, card, label);
}

void Manager::disconnectRoam(const QString &label)
{
    for (int i = 0; i < m_roamPeers.size(); ++i) {
        if (m_roamPeers.at(i).label == label) {
            if (m_activePeerLabel == label)
                m_activePeerLabel.clear();
            m_roamPeers.at(i).client->close();
            m_roamPeers.at(i).client->deleteLater();
            m_roamPeers.removeAt(i);
            m_roamModel->removePeer(label);
            return;
        }
    }
}

void Manager::openRoamSession(const QString &label, const QString &sessionId, const QString &cwd)
{
    for (int i = 0; i < m_roamPeers.size(); ++i) {
        if (m_roamPeers.at(i).label != label)
            continue;
        AcpClient *client = m_roamPeers.at(i).client;
        m_activePeerLabel = label;
        m_userDisconnect = false;
        m_reconnectTimer->stop();
        m_pendingQueue.clear();
        emit queuedChanged();
        m_landing = false;
        emit landingChanged();
        m_currentSessionId = sessionId;
        emit currentSessionChanged();
        for (const auto &v : m_roamPeers.at(i).sessions) {
            if (v.toMap().value("sessionId").toString() == sessionId) {
                m_currentSessionTitle = v.toMap().value("title").toString();
                break;
            }
        }
        m_tools.clear();
        m_extDefs.clear();
        m_sessionExts.clear();
        m_toolCatalog.clear();
        m_cacheDirty = false;
        publishToolGroups();
        emit toolsChanged();
        // Peer transcripts aren't cached (the cache is keyed by sessionId, which
        // could collide across machines) — always replay from the peer.
        m_suppressReplay = false;
        m_deferMessageUpdates = true;
        m_messageModel->clear();
        m_currentIndex = -1;
        emit messagesChanged();
        setStatus(QStringLiteral("loading…"));
        m_lastCwd = cwd.isEmpty() ? workingDir() : cwd;
        client->setResumeSession(sessionId, cwd);
        client->openSessionNow();
        return;
    }
    setStatus(QStringLiteral("roam: unknown peer ") + label);
}

void Manager::toggleRoamPeer(const QString &label)
{
    m_roamModel->togglePeer(label);
}

void Manager::setActiveTab(const QString &tab)
{
    // Opening a Main session clears the roam routing; the tab itself is pure UI.
    if (tab == QLatin1String("main") && !m_activePeerLabel.isEmpty()) {
        m_activePeerLabel.clear();
    }
}

AcpClient *Manager::activeClient() const
{
    if (!m_activePeerLabel.isEmpty()) {
        for (const RoamPeer &p : m_roamPeers) {
            if (p.label == m_activePeerLabel)
                return p.client;
        }
    }
    return m_client;
}

QString Manager::roamIdentity()
{
    QString secret = m_store.value("roam_identity").toString();
    if (secret.isEmpty()) {
        secret = RoamTransport::generateIdentity();
        if (!secret.isEmpty())
            m_store.setValue("roam_identity", secret);
    }
    return secret;
}

QString Manager::roamPublicKey() const
{
    const QString secret = m_store.value("roam_identity").toString();
    return secret.isEmpty() ? QString() : RoamTransport::publicKeyFor(secret);
}

QObject *Manager::roamModel() const { return m_roamModel; }

void Manager::disconnect()
{
    m_userDisconnect = true;
    m_reconnectTimer->stop();
    if (m_client)
        m_client->close();
    setOnline(false);
    m_prompting = false;
    emit promptingChanged();
    m_compacting = false;
    emit compactingChanged();
    // Back to the landing page (offline variant with Connect).
    m_landing = true;
    emit landingChanged();
    setStatus(QStringLiteral("disconnected"));
}

void Manager::testConnection()
{
    const QString key = secretKey();
    if (key.isEmpty()) {
        emit connectionTested(false, QStringLiteral("No secret key set — fill it in above."));
        return;
    }
    // A second socket, isolated from the live client: the probe must never
    // disturb an open chat or trip the reconnect logic.
    if (m_testWs)
        m_testWs->deleteLater();
    m_testWs = new QWebSocket(QString(), QWebSocketProtocol::VersionLatest, this);
    // Same trust-all TLS as the real wire (goosed uses a self-signed cert).
    m_testWs->setSslConfiguration([] {
        QSslConfiguration cfg;
        cfg.setPeerVerifyMode(QSslSocket::VerifyNone);
        return cfg;
    }());
    m_testFinished = false;
    setStatus(QStringLiteral("testing connection…"));

    connect(m_testWs, &QWebSocket::connected, this, [this] {
        // Mirror the real handshake: initialize right after opening. Any
        // reply means the endpoint is reachable, speaks ACP, and accepted the
        // secret key (a bad key fails the HTTP handshake before this).
        QJsonObject caps;
        caps.insert("protocolVersion", 1);
        QJsonObject clientCaps;
        clientCaps.insert("_meta", QJsonObject{
            {"goose", QJsonObject{{"recipeParameterRequests", true}}}});
        caps.insert("clientCapabilities", clientCaps);
        m_testWs->sendTextMessage(QString::fromUtf8(QJsonDocument(
            QJsonObject{{"jsonrpc", "2.0"}, {"id", 1},
                        {"method", "initialize"}, {"params", caps}})
            .toJson(QJsonDocument::Compact)));
    });
    connect(m_testWs, &QWebSocket::textMessageReceived, this, [this](const QString &msg) {
        if (m_testFinished)
            return;
        const QJsonDocument doc = QJsonDocument::fromJson(msg.toUtf8());
        if (doc.isObject() && doc.object().contains(QStringLiteral("id")))
            finishTest(true, QStringLiteral("Connection OK — the server responded."));
    });
    connect(m_testWs, QOverload<QAbstractSocket::SocketError>::of(&QWebSocket::errorOccurred),
            this, [this] {
        if (!m_testFinished)
            finishTest(false, QStringLiteral("Connection failed: ") + m_testWs->errorString());
    });
    connect(m_testWs, &QWebSocket::disconnected, this, [this] {
        if (!m_testFinished) {
            // Qt 6 emits disconnected (not errorOccurred) for some open
            // failures, e.g. connection refused — surface the socket's reason.
            const QString err = m_testWs->errorString();
            finishTest(false, QStringLiteral("Connection failed: %1").arg(
                err.isEmpty() ? QStringLiteral("disconnected before the server replied — "
                                               "check host, port, and secret key.")
                              : err));
        }
    });

    if (!m_testTimer) {
        m_testTimer = new QTimer(this);
        m_testTimer->setSingleShot(true);
        connect(m_testTimer, &QTimer::timeout, this, [this] {
            if (!m_testFinished)
                finishTest(false, QStringLiteral("Timed out — no reply from the server."));
        });
    }
    m_testTimer->start(8000);

    QNetworkRequest req;
    req.setUrl(QUrl(wsUrl()));
    req.setRawHeader("X-Secret-Key", key.toUtf8());
    m_testWs->open(req);
}

void Manager::finishTest(bool ok, const QString &message)
{
    m_testFinished = true;
    m_testTimer->stop();
    emit connectionTested(ok, message);
    if (m_testWs) {
        m_testWs->deleteLater();
        m_testWs = nullptr;
    }
}

void Manager::openSession(const QString &sessionId)
{
    ensureClient();
    m_userDisconnect = false;
    m_activePeerLabel.clear();   // a Main session owns the chat now
    m_reconnectTimer->stop();
    m_pendingQueue.clear();
    emit queuedChanged();
    m_landing = false;
    emit landingChanged();
    // find cwd so session/load doesn't silently rewrite working_dir
    const QString cwd = m_sessionsModel->cwdFor(sessionId);
    for (const auto &v : m_sessions) {
        if (v.toMap().value("sessionId").toString() == sessionId) {
            m_currentSessionTitle = v.toMap().value("title").toString();
            break;
        }
    }
    m_currentSessionId = sessionId;
    emit currentSessionChanged();
    m_tools.clear();
    m_extDefs.clear();
    m_sessionExts.clear();
    m_toolCatalog.clear();
    m_cacheDirty = false;
    publishToolGroups();
    emit toolsChanged();

    // Render a fresh cached transcript instantly, and only let session/load
    // rebuild us when the cache is missing or stale. This avoids pulling and
    // re-scrolling the whole history on every open.
    const QString updatedAt = m_sessionUpdatedAt.value(sessionId);
    const bool freshCache = loadCache(sessionId) && !updatedAt.isEmpty() && updatedAt == m_cachedUpdatedAt;
    if (freshCache) {
        m_currentIndex = -1;
        emit messagesChanged();
        setStatus(QStringLiteral("cached"));
        m_suppressReplay = true;
        m_deferMessageUpdates = false;
    } else {
        m_messageModel->clear();
        m_currentIndex = -1;
        emit messagesChanged();
        setStatus(QStringLiteral("loading…"));
        m_suppressReplay = false;
        m_deferMessageUpdates = true;
    }
    m_client->setResumeSession(sessionId, cwd);
    m_client->setDesiredCwd(workingDir());
    m_lastCwd = cwd.isEmpty() ? workingDir() : cwd;
    m_client->connectTo(wsUrl(), secretKey());
}

void Manager::newChat()
{
    ensureClient();
    m_userDisconnect = false;
    m_activePeerLabel.clear();   // a fresh Main chat owns the chat now
    m_reconnectTimer->stop();
    m_pendingQueue.clear();
    emit queuedChanged();
    m_landing = false;
    emit landingChanged();
    m_suppressReplay = false;
    m_deferMessageUpdates = false;
    m_currentSessionId.clear();
    m_currentSessionTitle.clear();
    m_messageModel->clear();
    m_tools.clear();
    m_extDefs.clear();
    m_sessionExts.clear();
    m_toolCatalog.clear();
    m_cacheDirty = false;
    publishToolGroups();
    m_currentIndex = -1;
    if (!m_deferMessageUpdates)
        emit messagesChanged();
    emit currentSessionChanged();
    emit toolsChanged();
    m_client->setResumeSession(QString(), QString());
    m_client->setDesiredCwd(workingDir());
    m_lastCwd = workingDir();
    m_client->connectTo(wsUrl(), secretKey());
}

void Manager::beginChat()
{
    // The landing page's staging session already exists (with the provider and
    // model the user picked there); stepping off the landing page reveals it as
    // the active chat.
    if (!m_landing)
        return;
    m_landing = false;
    emit landingChanged();
}

void Manager::sendPrompt(const QString &text, const QVariantList &images)
{
    if (!m_client || (text.trimmed().isEmpty() && images.isEmpty()))
        return;

    // Typing a first message steps off the landing page into the staging chat.
    m_landing = false;
    emit landingChanged();

    // A reply must stream in even if we opened this session from a fresh cache.
    m_suppressReplay = false;
    m_deferMessageUpdates = false;

    // Keep the attachments ON the message so the bubble renders them (image
    // thumbnails + file chips), not a placeholder. (Live-session only — a
    // replayed transcript reconstructs text, not attachments.)
    QVariantMap bubble{{"id", m_seq++}, {"role", "user"},
                       {"text", text}, {"html", markdownToHtml(text)}};
    if (!images.isEmpty()) {
        QMimeDatabase mimeDb;
        QVariantList attach;
        for (const auto &v : images) {
            const QString path = v.toString();
            if (path.isEmpty())
                continue;
            const QFileInfo fi(path);
            attach << QVariantMap{{"url", QUrl::fromLocalFile(path).toString()},
                                  {"name", fi.fileName()},
                                  {"image", mimeDb.mimeTypeForFile(path).name().startsWith(QLatin1String("image/"))}};
        }
        if (!attach.isEmpty())
            bubble["images"] = attach;
    }
    m_messageModel->append(bubble);
    m_currentIndex = m_messageModel->count() - 1;
    m_streamRole = "user";
    m_streamMsgId.clear();
    m_cacheDirty = true;
    emit messagesChanged();

    // Convert the local file paths into the ACP content blocks the wire wants.
    dispatchSend(text.trimmed(), buildAttachmentBlocks(images));
}

void Manager::dispatchSend(const QString &text, const QVariantList &blocks)
{
    if (!m_client) {
        enqueue({text, blocks});
        return;
    }
    if (activeClient() && activeClient()->ready() && !m_prompting) {
        m_prompting = true;
        emit promptingChanged();
        activeClient()->sendPrompt(text, blocks);
    } else if (activeClient() && activeClient()->ready() && m_prompting && !m_activeRunId.isEmpty() && blocks.isEmpty()) {
        // A turn is running AND we know its run id: STEER — inject into the live turn
        // instead of waiting for it to end. The server validates the id, so a run that
        // ended between typing and sending fails loudly. Images still queue (text-only).
        activeClient()->steer(text);
    } else if (activeClient() && activeClient()->ready()) {
        // A turn is running but we can't steer it: queue rather than firing a second
        // session/prompt into the same session. Flushed one-per-TurnDone.
        enqueue({text, blocks});
    } else {
        // Not connected yet (initial connect / reconnect window): queue and try to
        // open a connection (Android does the same via ensureConnected). onReady flushes.
        enqueue({text, blocks});
        if (!online() && !secretKey().isEmpty()) {
            const bool connecting = status().startsWith(QLatin1String("connecting"));
            if (!connecting)
                connectToServer();
        }
    }
}

void Manager::enqueue(const PendingSend &p)
{
    m_pendingQueue << p;
    emit queuedChanged();
}

void Manager::flushQueue()
{
    if (m_pendingQueue.isEmpty() || m_prompting)
        return;
    const PendingSend p = m_pendingQueue.takeFirst();
    emit queuedChanged();
    dispatchSend(p.text, p.images);
}

void Manager::maybeReconnect()
{
    // Exponential backoff, reset on every successful open (onReady). Give up after 6
    // attempts so a dead server surfaces as an actionable status rather than a silent loop.
    if (m_reconnectAttempts >= 6 || m_userDisconnect)
        return;
    const int delay = qMin(500 * (1 << qMin(m_reconnectAttempts, 5)), 15000);
    m_reconnectTimer->start(delay);
}

namespace {
// True for mime types that are really text even though they live under
// application/* (code, config, data). Everything else that isn't an image is
// sent as a binary blob.
bool isTextishMime(const QString &mime)
{
    if (mime.startsWith(QLatin1String("text/")))
        return true;
    static const QStringList textish = {
        QStringLiteral("application/json"), QStringLiteral("application/xml"),
        QStringLiteral("application/x-yaml"), QStringLiteral("application/yaml"),
        QStringLiteral("application/toml"), QStringLiteral("application/javascript"),
        QStringLiteral("application/x-javascript"), QStringLiteral("application/x-shellscript"),
        QStringLiteral("application/x-sh"), QStringLiteral("application/x-python"),
        QStringLiteral("application/x-tex"), QStringLiteral("application/sql"),
        QStringLiteral("application/x-diff"), QStringLiteral("application/graphql"),
    };
    return textish.contains(mime);
}
} // namespace

QStringList Manager::pickAttachmentFiles()
{
    // QFileDialog delegates to the platform theme's file dialog helper on
    // Plasma — the native Dolphin-style picker. Any file type, multi-select.
    const QString dir = m_store.value(QStringLiteral("last_attach_dir"), QDir::homePath()).toString();
    const QStringList files = QFileDialog::getOpenFileNames(
        nullptr, QStringLiteral("Attach files"), dir);
    if (!files.isEmpty()) {
        const QFileInfo info(files.first());
        m_store.setValue(QStringLiteral("last_attach_dir"), info.absolutePath());
    }
    return files;
}

QVariantList Manager::buildAttachmentBlocks(const QVariantList &paths)
{
    // ACP prompt content blocks. Images ride the `image` block (rendered by the
    // model). Everything else rides an embedded `resource` block: text content
    // is forwarded by goose as text; binary content is a schema-valid blob.
    QVariantList out;
    QMimeDatabase mimeDb;
    for (const auto &v : paths) {
        const QString path = v.toString();
        if (path.isEmpty())
            continue;
        QFile f(path);
        if (!f.open(QIODevice::ReadOnly))
            continue;
        const QByteArray data = f.readAll();
        f.close();
        const QString mime = mimeDb.mimeTypeForFile(path).name();
        const QString uri = QUrl::fromLocalFile(path).toString();
        if (mime.startsWith(QLatin1String("image/"))) {
            QString b64 = QString::fromLatin1(data.toBase64());
            b64.remove(QLatin1Char('\n')).remove(QLatin1Char('\r'));
            out << QVariantMap{{"type", "image"}, {"mimeType", mime}, {"data", b64}};
        } else if (isTextishMime(mime)) {
            out << QVariantMap{{"type", "resource"},
                               {"resource", QVariantMap{{"uri", uri}, {"mimeType", mime},
                                                        {"text", QString::fromUtf8(data)}}}};
        } else {
            QString b64 = QString::fromLatin1(data.toBase64());
            b64.remove(QLatin1Char('\n')).remove(QLatin1Char('\r'));
            out << QVariantMap{{"type", "resource"},
                               {"resource", QVariantMap{{"uri", uri}, {"mimeType", mime},
                                                        {"blob", b64}}}};
        }
    }
    return out;
}

void Manager::cancelTurn()
{
    // Android cancels the turn AND drops the queue: a stop means the user wants nothing
    // more of this conversation to send.
    m_pendingQueue.clear();
    emit queuedChanged();
    if (m_client)
        if (activeClient()) activeClient()->cancel();
}

void Manager::compactConversation()
{
    // /compact must not appear as a user bubble (Android sends it bare); it's a command,
    // not a message. compacting flips on immediately so the UI doesn't wait for the
    // server's own status echo; runEnded / a "complete" status clears it.
    AcpClient *cc = activeClient(); if (!cc || !cc->ready() || m_prompting)
        return;
    m_compacting = true;
    emit compactingChanged();
    cc->sendPrompt(QStringLiteral("/compact"));
}

void Manager::exportSessionTo(const QString &sessionId, const QString &filePath)
{
    if (!m_client || filePath.isEmpty())
        return;
    m_pendingExportPath = filePath;
    activeClient()->exportSession(sessionId);
}

void Manager::unarchiveSession(const QString &sessionId)
{
    if (m_client)
        activeClient()->unarchiveSession(sessionId);
}

void Manager::respondPermission(const QString &toolCallId, const QString &optionId)
{
    if (m_client) {
        activeClient()->respondPermission(toolCallId, optionId);
        // add a tool-role summary so the user sees what was approved
        QVariantMap m{{"id", m_seq++}, {"role", "tool"}, {"text", ""}, {"html", ""},
                      {"detail", m_permTitle}, {"status", "completed"}, {"output", "permission " + (optionId.isEmpty() ? QStringLiteral("denied") : QStringLiteral("granted"))}};
        m_messageModel->append(m);
        m_cacheDirty = true;
        emit messagesChanged();
    }
    m_permToolCallId.clear();
}

void Manager::setConfigOption(const QString &id, const QString &value)
{
    if (m_client)
        activeClient()->setConfigOption(id, value);
}

void Manager::refreshSessions()
{
    if (m_client)
        m_client->listSessions();
}

void Manager::refreshProjects()
{
    if (m_client)
        m_client->listProjects();
}

void Manager::createProject(const QString &name)
{
    if (m_client) {
        // Mirror the Android client's validation: lowercase/digits/hyphens, <= 64.
        QString n = name.trimmed();
        if (n.isEmpty() || n.size() > 64)
            return;
        for (const QChar &c : n) {
            if (!c.isLower() && !c.isDigit() && c != QLatin1Char('-'))
                return;
        }
        m_client->createProject(n, QString(), QString());
    }
}

void Manager::deleteProject(const QString &nameOrPath)
{
    if (!m_client)
        return;
    // Resolve the source path from the project id or name; sessions are unfiled
    // by the server when the project goes away, and the reply re-lists.
    QString path;
    for (const auto &v : m_projects) {
        const QVariantMap p = v.toMap();
        if (p.value("id").toString() == nameOrPath || p.value("name").toString() == nameOrPath) {
            path = p.value("path").toString();
            break;
        }
    }
    if (!path.isEmpty())
        m_client->deleteProject(path);
}

void Manager::moveSessionToProject(const QString &sessionId, const QString &projectId)
{
    if (m_client)
        activeClient()->assignSessionProject(sessionId, projectId);
}

void Manager::newChatInProject(const QString &projectId)
{
    m_pendingProjectFiling = projectId;
    newChat();
}

void Manager::refreshRecipes()
{
    // Dialogs can be opened while a reconnect/session replay is still in flight.
    // Keep the request instead of sending an RPC against a client with no session;
    // onReady() always drains this pending refresh.
    if (!m_client || !m_client->ready()) {
        m_recipeRefreshPending = true;
        return;
    }
    m_recipeRefreshPending = false;
    m_client->listRecipes();
    m_client->listSchedules();
}

void Manager::runRecipe(const QString &id)
{
    // A recipe runs by starting a fresh session with _meta.recipeId; the server
    // wires the prompt/instructions and requests any parameters (answered with
    // their defaults in AcpClient::serverRequest).
    if (m_client) {
        m_client->setDesiredRecipeId(id);
        m_pendingProjectFiling.clear();
        newChat();
    }
}

void Manager::scheduleRecipe(const QString &id, const QString &cron)
{
    if (m_client)
        m_client->scheduleRecipe(id, cron);
}

void Manager::deleteRecipe(const QString &id)
{
    if (m_client)
        m_client->deleteRecipe(id);
}

void Manager::setSchedulePaused(const QString &scheduleId, bool paused)
{
    if (m_client)
        m_client->setSchedulePaused(scheduleId, paused);
}

void Manager::runScheduleNow(const QString &scheduleId)
{
    if (m_client)
        m_client->runScheduleNow(scheduleId);
}

void Manager::renameSession(const QString &sessionId, const QString &title)
{
    if (m_client)
        activeClient()->renameSession(sessionId, title);
}

void Manager::archiveSession(const QString &sessionId)
{
    if (m_client)
        activeClient()->archiveSession(sessionId);
}

void Manager::deleteSession(const QString &sessionId)
{
    if (m_client)
        activeClient()->deleteSession(sessionId);
    // If it was the open chat, drop it and start fresh so the UI doesn't keep a
    // deleted session selected (delete's reply re-lists without it).
    if (sessionId == m_currentSessionId) {
        m_currentSessionId.clear();
        m_currentSessionTitle.clear();
        m_messageModel->clear();
        m_tools.clear();
        m_currentIndex = -1;
        m_landing = true;
        emit landingChanged();
        emit messagesChanged();
        emit currentSessionChanged();
        emit toolsChanged();
    }
}

// ---- skills ----------------------------------------------------------------

void Manager::refreshSkills()
{
    if (m_client)
        m_client->listSkills();
}

void Manager::saveSkill(const QString &path, const QString &name,
                        const QString &description, const QString &content)
{
    if (m_client && !path.isEmpty())
        m_client->updateSkill(path, name, description, content);
}

void Manager::deleteSkill(const QString &path)
{
    if (m_client && !path.isEmpty())
        m_client->deleteSkill(path);
}

void Manager::onSkills(const QVariantList &skills)
{
    m_skills = skills;
    emit skillsChanged();
}

// ---- server config (providers) ---------------------------------------------

void Manager::setServerConfig(const QString &key, const QString &value)
{
    if (!m_client)
        return;
    // Upsert then re-read to confirm: the upsert reply is empty. These are global
    // (config.yaml) values that take effect for NEW sessions/tasks only.
    m_client->upsertConfig(key, value);
    m_client->readConfig(key);
}

void Manager::readServerConfig(const QString &key)
{
    if (m_client)
        m_client->readConfig(key);
}

void Manager::refreshSupportedModels(const QString &providerId)
{
    if (m_client && !providerId.isEmpty())
        m_client->listSupportedModels(providerId);
}

void Manager::onServerConfigValue(const QString &key, const QString &value)
{
    m_serverConfig[key] = value;
    emit serverConfigChanged();
}

void Manager::onSupportedModels(const QString &providerId, const QStringList &models)
{
    Q_UNUSED(providerId);
    m_supportedModels.clear();
    for (const auto &m : models)
        m_supportedModels << m;
    emit supportedModelsChanged();
}

void Manager::onExportResult(const QString &data)
{
    if (m_pendingExportPath.isEmpty())
        return;
    if (!data.isEmpty()) {
        QFile f(m_pendingExportPath);
        if (f.open(QIODevice::WriteOnly | QIODevice::Truncate)) {
            f.write(data.toUtf8());
            f.close();
            setStatus(QStringLiteral("exported to ") + m_pendingExportPath);
        } else {
            setStatus(QStringLiteral("export failed — cannot write ") + m_pendingExportPath);
        }
    } else {
        setStatus(QStringLiteral("export failed — empty reply"));
    }
    m_pendingExportPath.clear();
}

// ---- streaming handlers ----------------------------------------------------

void Manager::appendChunk(const QString &role, const QString &text, const QString &messageId, bool thought)
{
    // New bubble when role or messageId changes.
    const bool fresh = m_currentIndex < 0
        || m_streamRole != role
        || (!messageId.isEmpty() && !m_streamMsgId.isEmpty() && messageId != m_streamMsgId);

    if (fresh) {
        m_messageModel->append(QVariantMap{{"id", m_seq++}, {"role", role},
                                        {"text", text}, {"html", QString()},
                                        {"thought", thought}});
        m_currentIndex = m_messageModel->count() - 1;
        m_streamRole = role;
        m_streamMsgId = messageId;
    } else {
        QVariantMap m = m_messageModel->row(m_currentIndex);
        const QString acc = m.value("text").toString() + text;
        m["text"] = acc;
        m["thought"] = thought;
        // Deferred + coalesced dataChanged: an immediate one per chunk made the
        // delegate rebuild the whole bubble (HTML re-parse + shaping + layout)
        // on every token — quadratic as a long reply streamed in.
        m_messageModel->updateDeferred(m_currentIndex, m);
    }
    m_cacheDirty = true;
    requestMessagesUpdate();
}

void Manager::finalizeCurrentMessage()
{
    // Render markdown only when a turn completes, not on every streaming chunk:
    // per-chunk markdown re-render made long replies/replays visibly laggy.
    if (m_currentIndex >= 0 && m_currentIndex < m_messageModel->count()) {
        QVariantMap m = m_messageModel->row(m_currentIndex);
        const QString role = m.value("role").toString();
        if (role == "agent" || role == "user") {
            m["html"] = markdownToHtml(m.value("text").toString());
            m_messageModel->update(m_currentIndex, m);
            m_cacheDirty = true;
            requestMessagesUpdate();
        }
    }
}

void Manager::onAgentChunk(const QString &text, const QString &messageId)
{
    if (m_suppressReplay)
        return;
    m_prompting = true;
    appendChunk("agent", text, messageId, false);
}

void Manager::onUserChunk(const QString &text, const QString &messageId)
{
    if (m_suppressReplay)
        return;
    appendChunk("user", text, messageId, false);
}

void Manager::onThoughtChunk(const QString &text)
{
    if (m_suppressReplay)
        return;
    appendChunk("thought", text, QString(), true);
}

void Manager::onToolCall(const QString &title, const QString &detail, const QString &toolCallId)
{
    if (m_suppressReplay)
        return;
    const QVariantMap call{{"title", title}, {"detail", detail}, {"output", QString()},
                           {"status", QStringLiteral("in_progress")}, {"toolCallId", toolCallId}};
    const int lastIndex = m_messageModel->count() - 1;
    if (lastIndex >= 0) {
        QVariantMap last = m_messageModel->row(lastIndex);
        const QString lastRole = last.value("role").toString();
        if (lastRole == QStringLiteral("tool")) {
            // Convert the first standalone call into a group when a consecutive
            // second call arrives. Subsequent calls append to the same row.
            const QVariantMap first{{"title", last.value("title")},
                                    {"detail", last.value("detail")},
                                    {"output", last.value("output")},
                                    {"status", last.value("status")},
                                    {"toolCallId", last.value("toolCallId")}};
            last["role"] = QStringLiteral("toolgroup");
            last["calls"] = QVariantList{first, call};
            m_messageModel->update(lastIndex, last);
        } else if (lastRole == QStringLiteral("toolgroup")) {
            QVariantList calls = last.value("calls").toList();
            calls << call;
            last["calls"] = calls;
            m_messageModel->update(lastIndex, last);
        } else {
            m_messageModel->append(QVariantMap{{"id", m_seq++}, {"role", "tool"}, {"text", ""}, {"html", ""},
                                               {"title", title}, {"detail", detail}, {"output", ""},
                                               {"status", "in_progress"}, {"toolCallId", toolCallId}});
        }
    } else {
        m_messageModel->append(QVariantMap{{"id", m_seq++}, {"role", "tool"}, {"text", ""}, {"html", ""},
                                           {"title", title}, {"detail", detail}, {"output", ""},
                                           {"status", "in_progress"}, {"toolCallId", toolCallId}});
    }
    m_currentIndex = -1;
    m_cacheDirty = true;
    requestMessagesUpdate();
}

void Manager::onToolCallUpdate(const QString &toolCallId, const QString &status,
                               const QString &output, bool live)
{
    if (m_suppressReplay)
        return;
    for (int i = m_messageModel->count() - 1; i >= 0; --i) {
        QVariantMap m = m_messageModel->row(i);
        const QString role = m.value("role").toString();
        // Chart / MCP-App bubbles also carry their tool's id and follow the same lifecycle.
        if ((role == "tool" || role == "chart" || role == "mcpapp")
            && m.value("toolCallId").toString() == toolCallId) {
            m["status"] = status;
            if (role == "tool" && !output.isEmpty())
                m["output"] = (live ? m.value("output").toString() : QString()) + output;
            m_messageModel->update(i, m);
            break;
        }
        if (role == QStringLiteral("toolgroup")) {
            QVariantList calls = m.value("calls").toList();
            bool found = false;
            for (int j = 0; j < calls.size(); ++j) {
                QVariantMap call = calls.at(j).toMap();
                if (call.value("toolCallId").toString() != toolCallId)
                    continue;
                call["status"] = status;
                if (!output.isEmpty())
                    call["output"] = (live ? call.value("output").toString() : QString()) + output;
                calls[j] = call;
                found = true;
                break;
            }
            if (found) {
                m["calls"] = calls;
                m_messageModel->update(i, m);
                break;
            }
        }
    }
    m_cacheDirty = true;
    requestMessagesUpdate();
}

void Manager::onChartToolCall(const QString &title, const QString &toolCallId, const QString &chartSpec)
{
    if (m_suppressReplay)
        return;
    m_messageModel->append(QVariantMap{{"id", m_seq++}, {"role", "chart"}, {"text", ""},
                                       {"title", title}, {"chartData", chartSpec},
                                       {"toolCallId", toolCallId}, {"status", "in_progress"}});
    m_currentIndex = -1;
    m_cacheDirty = true;
    requestMessagesUpdate();
}

void Manager::onMcpAppToolCall(const QString &title, const QString &toolCallId, const QString &appKey,
                               const QString &appUri, const QString &appExt, const QString &appInput)
{
    if (m_suppressReplay)
        return;
    m_messageModel->append(QVariantMap{{"id", m_seq++}, {"role", "mcpapp"}, {"text", ""},
                                       {"title", title}, {"detail", appInput}, {"appKey", appKey},
                                       {"appHtml", QString()}, {"toolCallId", toolCallId},
                                       {"status", "in_progress"}});
    m_currentIndex = -1;
    m_cacheDirty = true;
    // Fetch the server-hosted template now; the bubble swaps in the rendered view when
    // it lands (or a failed fetch marks the bubble failed instead of wedging it).
    if (activeClient()) activeClient()->readAppResource(appKey, appUri, appExt);
    requestMessagesUpdate();
}

void Manager::onAppResource(const QString &appKey, const QString &html)
{
    for (int i = m_messageModel->count() - 1; i >= 0; --i) {
        QVariantMap m = m_messageModel->row(i);
        if (m.value("role").toString() == "mcpapp" && m.value("appKey").toString() == appKey) {
            m["appHtml"] = html;
            if (html.isEmpty())
                m["status"] = "failed";
            m_messageModel->update(i, m);
            break;
        }
    }
    m_cacheDirty = true;
    requestMessagesUpdate();
}

void Manager::onCompactionStatus(const QString &message)
{
    // Substring-match goose's status lines exactly as the Android client does: a line
    // mentioning "compact" starts (or continues) a compaction; "complete"/"error" ends it.
    const QString m = message.toLower();
    if (m.contains(QLatin1String("compact")))
        m_compacting = true;
    if (m.contains(QLatin1String("complete")) || m.contains(QLatin1String("error")))
        m_compacting = false;
    emit compactingChanged();
}

void Manager::onMessageUsage(const QVariantMap &usage)
{
    // Attach the tok/s + cost stats to the currently-streaming agent bubble. The stats
    // are derived client-side from outputTokens/elapsedMs (can't divide a zero duration).
    const int out = usage.value("outputTokens").toInt();
    const qint64 elapsed = usage.value("elapsedMs").toLongLong();
    if (elapsed <= 0)
        return;
    const QString label = formatUsage(usage);
    if (label.isEmpty())
        return;
    // If we're streaming an agent bubble, tag it; otherwise the last agent bubble.
    for (int i = m_messageModel->count() - 1; i >= 0; --i) {
        QVariantMap m = m_messageModel->row(i);
        if (m.value("role").toString() == "agent") {
            m["usage"] = label;
            m_messageModel->update(i, m);
            break;
        }
    }
    m_cacheDirty = true;
}

QString Manager::formatUsage(const QVariantMap &usage) const
{
    const int out = usage.value("outputTokens").toInt();
    const qint64 elapsed = usage.value("elapsedMs").toLongLong();
    const qint64 ttft = usage.value("timeToFirstTokenMs").toLongLong();
    const double cost = usage.value("cost").toDouble();
    if (elapsed <= 0)
        return {};
    const double toksPerSec = out > 0 ? double(out) / (elapsed / 1000.0) : 0.0;
    QString label = QStringLiteral("%1 tok/s · %2 tokens · %3s TTFT")
        .arg(toksPerSec, 0, 'f', 1)
        .arg(out)
        .arg(double(ttft) / 1000.0, 0, 'f', 1);
    if (cost > 0.0)
        label += QStringLiteral(" · $%1").arg(cost, 0, 'f', 4);
    return label;
}

void Manager::onCommands(const QStringList &commands)
{
    m_availableCommands.clear();
    for (const auto &c : commands)
        m_availableCommands << c;
    emit commandsChanged();
}

void Manager::onModeChanged(const QString &modeId)
{
    // Patch the single "mode" entry in the existing config list rather than replacing
    // it: configReady carries the whole list and the provider/model selectors read it.
    for (auto &m : m_config) {
        QVariantMap entry = m.toMap();
        if (entry.value("id").toString() == QStringLiteral("mode")) {
            entry["currentValue"] = modeId;
            m = entry;
            emit configChanged();
            return;
        }
    }
    m_config << QVariantMap{{"id", "mode"}, {"name", "mode"}, {"currentValue", modeId}};
    emit configChanged();
}

void Manager::onActiveRunChanged(const QString &sessionId, const QString &runId)
{
    // Only the run of the chat on screen is steerable; a run from another client on the
    // same session must not let us inject into it.
    if (sessionId == m_currentSessionId || sessionId.isEmpty())
        m_activeRunId = runId;
    else
        m_activeRunId.clear();
}

void Manager::onUsage(int used, int size, double cost, const QString &currency)
{
    Q_UNUSED(cost);
    Q_UNUSED(currency);
    m_contextUsed = used;
    m_contextSize = size;
    emit contextChanged();
}

void Manager::onReady(const QString &sessionId)
{
    if (!sessionId.isEmpty())
        m_currentSessionId = sessionId;
    m_store.setValue("last_session", m_currentSessionId);
    if (!m_lastCwd.isEmpty())
        m_store.setValue("last_session_cwd", m_lastCwd);
    // Replay for this open is done (or was suppressed by a fresh cache).
    m_suppressReplay = false;
    m_deferMessageUpdates = false;
    // The connection is healthy again: reset the reconnect budget and clear turn state
    // that belonged to the previous wire (a new client can't see the old client's TurnDone).
    m_reconnectAttempts = 0;
    m_reconnectTimer->stop();
    m_activeRunId.clear();
    m_compacting = false;
    emit compactingChanged();
    // session/load replays transcript chunks but never emits runEnded. Render every
    // completed user/assistant row before exposing the replay as ready.
    renderMarkdownRows();
    if (!m_currentSessionId.isEmpty() && m_cacheDirty)
        saveCache(m_currentSessionId);
    if (!m_currentSessionId.isEmpty())
        loadToolCache(m_currentSessionId);
    emit messagesChanged();
    setOnline(true);
    setStatus(QStringLiteral("ready"));
    m_prompting = false;
    emit promptingChanged();
    flushQueue();
    refreshSessions();
    refreshProjects();
    m_recipeRefreshPending = false;
    refreshRecipes();
    // File a freshly-created chat into the project it was started from.
    if (!m_pendingProjectFiling.isEmpty()) {
        const QString proj = m_pendingProjectFiling;
        m_pendingProjectFiling.clear();
        if (!m_currentSessionId.isEmpty())
            moveSessionToProject(m_currentSessionId, proj);
    }
    // Tool metadata is restored from the per-session cache. The drawer performs
    // the network refresh lazily when the user opens it.
}

void Manager::autoConnect()
{
    if (!autoConnectEnabled())
        return;
    // Only auto-connect once credentials are configured; otherwise stay on the
    // landing page and let the user fill the Connect dialog.
    if (host().trimmed().isEmpty() || secretKey().isEmpty()) {
        setStatus(QStringLiteral("not configured — press Connect"));
        return;
    }
    // Cold start never resumes the last conversation. Connect and form a fresh
    // (empty) staging session; the landing page stays up so the user can pick
    // a provider + model before stepping into the chat (beginChat).
    m_landing = true;
    connectToServer();
}

void Manager::resumeSession(const QString &sessionId, const QString &cwd)
{
    ensureClient();
    m_userDisconnect = false;
    m_reconnectTimer->stop();
    m_currentSessionId = sessionId;
    m_currentSessionTitle.clear();
    m_cacheDirty = false;
    const bool cached = loadCache(sessionId);
    if (cached) {
        m_currentIndex = -1;
        m_suppressReplay = true;
        m_deferMessageUpdates = false;
        emit messagesChanged();
        setStatus(QStringLiteral("cached"));
    } else {
        m_messageModel->clear();
        m_currentIndex = -1;
        m_suppressReplay = false;
        m_deferMessageUpdates = true;
        emit messagesChanged();
        setStatus(QStringLiteral("loading…"));
    }
    emit currentSessionChanged();
    m_client->setResumeSession(sessionId, cwd);
    m_client->setDesiredCwd(workingDir());
    m_lastCwd = cwd.isEmpty() ? workingDir() : cwd;
    m_client->connectTo(wsUrl(), secretKey());
}

void Manager::onSessions(const QVariantList &sessions)
{
    m_sessions = sessions;
    m_sessionsModel->setSessions(sessions);
    m_sessionUpdatedAt.clear();
    for (const auto &v : sessions) {
        const QVariantMap m = v.toMap();
        m_sessionUpdatedAt.insert(m.value("sessionId").toString(), m.value("updatedAt").toString());
    }
    // Stamp the session's updatedAt on the cache (onReady runs before the first
    // list) but only re-serialize when the transcript actually changed — a plain
    // session/list refresh must not rewrite the whole transcript to disk.
    if (!m_currentSessionId.isEmpty() && m_cacheDirty)
        saveCache(m_currentSessionId);
    emit sessionsChanged();
}

void Manager::onProjects(const QVariantList &projects)
{
    m_projects = projects;
    // Sessions and projects refresh together so the sidebar's project groups
    // never render against a stale id->name map.
    m_sessionsModel->setProjects(projects);
    emit projectsChanged();
    emit sessionsChanged();
}

void Manager::onRecipes(const QVariantList &recipes)
{
    m_recipes = recipes;
    emit recipesChanged();
}

void Manager::onSchedules(const QVariantList &schedules)
{
    m_schedules = schedules;
    emit schedulesChanged();
}

void Manager::onConfig(const QVariantList &config)
{
    m_config = config;
    emit configChanged();
}

void Manager::onTools(const QVariantList &tools)
{
    QStringList names;
    for (const auto &v : tools)
        names << v.toString();
    m_tools = names;

    if (!m_discoveringExt.isEmpty()) {
        // A catalog read: the session was briefly re-added unfiltered, so tools now contain that
        // extension's FULL set. Record it under the extension's prefix, then restore the real
        // allowlist (which re-fires tools/list; discovering is cleared so that pass is normal).
        const QString prefix = m_discoveringExt + QStringLiteral("__");
        QStringList full;
        for (const auto &n : std::as_const(names))
            if (n.startsWith(prefix))
                full << n;
        m_toolCatalog[m_discoveringExt] = full;
        const QString target = m_discoveringExt;
        m_discoveringExt.clear();
        const ExtDef *e = extDef(target);
        if (e) {
            const QStringList allowed = e->availableTools;
            setSessionTools(target, allowed.isEmpty() ? full : allowed);
        }
    } else {
        publishToolGroups();
    }
    saveToolCache(m_currentSessionId);
    emit toolsChanged();
}

void Manager::onExtensions(const QVariantList &extensions)
{
    m_extDefs.clear();
    for (const auto &v : extensions) {
        const QVariantMap m = v.toMap();
        ExtDef d;
        d.name = m.value("name").toString();
        d.type = m.value("type").toString();
        d.attrib = m.value("attrib").toBool();
        d.enabled = m.value("enabled").toBool();
        d.raw = m.value("raw").value<QJsonObject>();
        const QVariantList at = m.value("availableTools").toList();
        for (const auto &a : at)
            d.availableTools << a.toString();
        m_extDefs << d;
    }
    publishToolGroups();
    emit globalExtensionsChanged();
    saveToolCache(m_currentSessionId);
    emit toolsChanged();
}

void Manager::onSessionExtensions(const QStringList &names)
{
    m_sessionExts = names;
    publishToolGroups();
    saveToolCache(m_currentSessionId);
}

const Manager::ExtDef *Manager::extDef(const QString &name) const
{
    for (const auto &d : m_extDefs)
        if (d.name == name)
            return &d;
    return nullptr;
}

QVariant Manager::toolGroups() const
{
    QVariantList out;
    const QSet<QString> active(m_tools.constBegin(), m_tools.constEnd());
    const QSet<QString> enabled(m_sessionExts.constBegin(), m_sessionExts.constEnd());
    // Session extensions not in the config list (couldn't be re-added) still get a group so
    // they can at least be disabled; they have no tools to show.
    QStringList allNames;
    for (const auto &d : m_extDefs)
        allNames << d.name;
    for (const auto &n : m_sessionExts)
        if (!allNames.contains(n))
            allNames << n;

    // Some goose versions expose tools before they expose extension profiles.
    // Keep the panel useful in that state by grouping the active names directly;
    // later config/session replies replace these fallback groups with toggleable
    // extension-backed groups.
    if (allNames.isEmpty() && !m_tools.isEmpty()) {
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
                               {"attrib", groupName != QStringLiteral("Built-in")},
                               {"enabled", true},
                               {"known", true},
                               {"tools", grouped.value(groupName)}};
        }
        return out;
    }

    for (const auto &name : std::as_const(allNames)) {
        QVariantMap group;
        group["name"] = name;
        const ExtDef *d = extDef(name);
        const bool attrib = d && d->attrib;
        group["attrib"] = attrib;
        group["enabled"] = enabled.contains(name);
        group["known"] = m_toolCatalog.contains(name);
        QVariantList tools;
        const QString prefix = name + QStringLiteral("__");
        QStringList pool = m_toolCatalog.value(name);
        // Active names are useful even when this extension was reported with a
        // type we cannot attribute or its catalog discovery is still pending.
        if (pool.isEmpty()) {
            for (const auto &t : m_tools)
                if (t.startsWith(prefix))
                    pool << t;
        }
        for (const auto &t : pool) {
            tools << QVariantMap{{"name", t.mid(prefix.length())},
                                 {"on", active.contains(t)}};
        }
        group["tools"] = tools;
        out << group;
    }
    return out;
}

void Manager::publishToolGroups()
{
    emit toolGroupsChanged();
}

void Manager::setSessionTools(const QString &extName, const QStringList &allowed)
{
    const ExtDef *d = extDef(extName);
    if (!d)
        return;
    // An allowlist equal to the whole catalog is the same as no allowlist; store [] so it stays
    // that way if the extension later gains tools.
    const QStringList full = m_toolCatalog.value(extName);
    const QStringList list =
        (!full.isEmpty() && allowed.size() >= full.size()) ? QStringList() : allowed;
    QJsonObject scoped = d->raw;
    QJsonArray arr;
    for (const auto &t : list)
        arr.append(t);
    scoped.insert("available_tools", arr);
    m_discoveringExt.clear();
    m_client->removeSessionExtension(extName);
    m_client->addSessionExtension(toExtensionDto(scoped));
}

void Manager::refreshToolGroups()
{
    AcpClient *c = activeClient();
    if (!c || !c->ready())
        return;
    c->listConfigExtensions();
    c->listTools();
}

void Manager::discoverToolGroup(const QString &extName)
{
    const ExtDef *d = extDef(extName);
    if (!d || m_toolCatalog.contains(extName))
        return;
    // Only mcp-backed extensions namespace their tools; there's nothing more to discover otherwise.
    QJsonObject unfiltered = d->raw;
    unfiltered.insert("available_tools", QJsonArray());
    m_discoveringExt = extName;
    m_client->removeSessionExtension(extName);
    m_client->addSessionExtension(toExtensionDto(unfiltered));  // reply triggers listTools
}

void Manager::setSessionExtensionEnabled(const QString &extName, bool enabled)
{
    if (!m_client)
        return;
    const ExtDef *d = extDef(extName);
    if (!d)
        return;  // cannot re-add a profile we have not received from Goose
    // MCP startup can take many seconds. Keep the user's choice visible while
    // Goose brings the extension up; the post-mutation session list remains the
    // authority and corrects it if the server rejects the change.
    if (enabled) {
        if (!m_sessionExts.contains(extName))
            m_sessionExts << extName;
    } else {
        m_sessionExts.removeAll(extName);
    }
    publishToolGroups();
    saveToolCache(m_currentSessionId);
    if (enabled) {
        m_client->addSessionExtension(toExtensionDto(d->raw));
    } else {
        m_client->removeSessionExtension(extName);
    }
}

void Manager::setSessionToolEnabled(const QString &extName, const QString &toolName, bool on)
{
    const ExtDef *d = extDef(extName);
    if (!d)
        return;
    const QString prefix = extName + QStringLiteral("__");
    QSet<QString> current;
    for (const auto &t : m_tools)
        if (t.startsWith(prefix))
            current << t;
    if (on == current.contains(prefix + toolName))
        return;  // no-op
    if (on) current << (prefix + toolName);
    else current.remove(prefix + toolName);
    setSessionTools(extName, current.values());
}

// ---- global (config.yaml) extensions ----------------------------------------

QVariant Manager::globalExtensions() const
{
    // Same shape as toolGroups() but driven by the GLOBAL config list: per-extension
    // enabled switch + per-tool allowlist for mcp extensions (defaults for new sessions).
    QVariantList out;
    for (const auto &d : m_extDefs) {
        QVariantMap group;
        group["name"] = d.name;
        group["type"] = d.type;
        group["attrib"] = d.attrib;
        group["enabled"] = d.enabled;
        QVariantList tools;
        const QString prefix = d.name + QStringLiteral("__");
        // Tool names in the allowlist are namespaced; strip the prefix for display. An
        // empty allowlist means "all tools" — the catalog (if a session discovered it)
        // shows them as on; otherwise the row just shows the enabled switch.
        const QSet<QString> allowed(d.availableTools.constBegin(), d.availableTools.constEnd());
        if (allowed.isEmpty()) {
            const QStringList full = m_toolCatalog.value(d.name);
            for (const auto &t : full)
                tools << QVariantMap{{"name", t.mid(prefix.length())}, {"on", true}};
        } else {
            for (const auto &t : d.availableTools) {
                if (t.startsWith(prefix))
                    tools << QVariantMap{{"name", t.mid(prefix.length())}, {"on", true}};
            }
        }
        group["tools"] = tools;
        out << group;
    }
    return out;
}

void Manager::refreshGlobalExtensions()
{
    if (m_client)
        m_client->listConfigExtensions();
}

void Manager::setGlobalExtensionEnabled(const QString &extName, bool enabled)
{
    if (!m_client)
        return;
    const ExtDef *d = extDef(extName);
    if (!d)
        return;
    m_client->setConfigExtensionEnabled(extName, enabled);
}

void Manager::setGlobalToolEnabled(const QString &extName, const QString &toolName, bool on)
{
    const ExtDef *d = extDef(extName);
    if (!d)
        return;
    const QString prefix = extName + QStringLiteral("__");
    QSet<QString> current(d->availableTools.constBegin(), d->availableTools.constEnd());
    if (on == current.contains(prefix + toolName))
        return;  // no-op
    if (on) current << (prefix + toolName);
    else current.remove(prefix + toolName);
    // Saving a global allowlist = re-adding the extension (with the modified
    // available_tools) to config.yaml; the reply re-lists the global set.
    QJsonObject scoped = d->raw;
    QJsonArray arr;
    for (const auto &t : std::as_const(current))
        arr.append(t);
    scoped.insert("available_tools", arr);
    m_client->addConfigExtension(toExtensionDto(scoped), d->enabled);
}

void Manager::onPermission(const QString &toolCallId, const QString &title,
                           const QString &detail, const QVariantList &options)
{
    Q_UNUSED(detail);
    m_permToolCallId = toolCallId;
    m_permTitle = title;
    m_permOptions = options;
    emit permissionRequested();
}

void Manager::onError(const QString &text, bool background)
{
    if (!background) {
        QVariantMap m{{"id", m_seq++}, {"role", "error"}, {"text", text}, {"html", QStringLiteral("<div>") + text + QStringLiteral("</div>")}};
        m_messageModel->append(m);
        m_cacheDirty = true;
        emit messagesChanged();
    }
    setStatus(text);
}

void Manager::renderMarkdownRows()
{
    bool changed = false;
    for (int i = 0; i < m_messageModel->count(); ++i) {
        QVariantMap message = m_messageModel->row(i);
        const QString role = message.value("role").toString();
        const QString text = message.value("text").toString();
        if ((role != QStringLiteral("agent") && role != QStringLiteral("user"))
            || text.isEmpty() || !message.value("html").toString().isEmpty())
            continue;
        message["html"] = markdownToHtml(text);
        m_messageModel->update(i, message);
        changed = true;
    }
    if (changed) {
        m_cacheDirty = true;
        requestMessagesUpdate();
    }
}

QString Manager::cacheFilePath(const QString &sessionId) const
{
    const QString base = QStandardPaths::writableLocation(QStandardPaths::CacheLocation);
    QDir().mkpath(base);
    // session ids are opaque but filesystem-safe (e.g. "20260729_115"); still
    // escape to guard against any pathological value.
    QString safe = QString(sessionId).replace(QLatin1Char('/'), QLatin1Char('_'));
    return base + QStringLiteral("/") + safe + QStringLiteral(".json");
}

bool Manager::loadCache(const QString &sessionId)
{
    QFile f(cacheFilePath(sessionId));
    if (!f.open(QIODevice::ReadOnly))
        return false;
    const QJsonDocument doc = QJsonDocument::fromJson(f.readAll());
    f.close();
    const QJsonObject root = doc.object();
    m_cachedUpdatedAt = root.value("updatedAt").toString();
    const QJsonArray arr = root.value("messages").toArray();
    if (arr.isEmpty())
        return false;
    m_messageModel->clear();
    for (const auto &el : arr) {
        const QJsonObject o = el.toObject();
        const QString role = o.value("role").toString();
        const QString text = o.value("text").toString();
        QString html = o.value("html").toString();
        if ((role == QStringLiteral("agent") || role == QStringLiteral("user"))
            && !text.isEmpty() && html.isEmpty()) {
            html = markdownToHtml(text);
            m_cacheDirty = true;
        }
        m_messageModel->append(QVariantMap{
            {"id", m_seq++},
            {"role", role},
            {"text", text},
            {"html", html},
            {"title", o.value("title").toString()},
            {"detail", o.value("detail").toString()},
            {"output", o.value("output").toString()},
            {"status", o.value("status").toString()},
            {"thought", o.value("thought").toBool()},
            {"chartData", o.value("chartData").toString()},
        });
        if (role == QStringLiteral("toolgroup")) {
            QVariantMap group = m_messageModel->row(m_messageModel->count() - 1);
            QVariantList calls;
            for (const auto &callValue : o.value("calls").toArray()) {
                const QJsonObject call = callValue.toObject();
                calls << QVariantMap{{"title", call.value("title").toString()},
                                     {"detail", call.value("detail").toString()},
                                     {"output", call.value("output").toString()},
                                     {"status", call.value("status").toString()},
                                     {"toolCallId", call.value("toolCallId").toString()}};
            }
            group["calls"] = calls;
            m_messageModel->update(m_messageModel->count() - 1, group);
        }
    }
    return true;
}

void Manager::saveCache(const QString &sessionId)
{
    if (sessionId.isEmpty() || m_messageModel->count() == 0)
        return;
    QJsonArray arr;
    for (const auto &v : m_messageModel->rows()) {
        const QVariantMap m = v;
        const QString role = m.value("role").toString();
        // History only has plain text; render markdown once here so reopening
        // from disk is both instant and nicely formatted (paid per session, not
        // per open).
        QString html = m.value("html").toString();
        if (html.isEmpty() && (role == "agent" || role == "user"))
            html = markdownToHtml(m.value("text").toString());
        QJsonObject o;
        o["role"] = role;
        o["text"] = m.value("text").toString();
        o["html"] = html;
        o["detail"] = m.value("detail").toString();
        o["title"] = m.value("title").toString();
        o["output"] = m.value("output").toString();
        o["status"] = m.value("status").toString();
        o["thought"] = m.value("thought").toBool();
        if (role == "chart")
            o["chartData"] = m.value("chartData").toString();
        if (role == "toolgroup") {
            QJsonArray calls;
            for (const auto &callValue : m.value("calls").toList()) {
                const QVariantMap call = callValue.toMap();
                calls.append(QJsonObject{
                    {"title", call.value("title").toString()},
                    {"detail", call.value("detail").toString()},
                    {"output", call.value("output").toString()},
                    {"status", call.value("status").toString()},
                    {"toolCallId", call.value("toolCallId").toString()},
                });
            }
            o["calls"] = calls;
        }
        arr.append(o);
    }
    QJsonObject root;
    root["updatedAt"] = m_sessionUpdatedAt.value(sessionId);
    root["messages"] = arr;
    QFile f(cacheFilePath(sessionId));
    if (f.open(QIODevice::WriteOnly | QIODevice::Truncate)) {
        f.write(QJsonDocument(root).toJson(QJsonDocument::Compact));
        f.close();
        m_cacheDirty = false;
    }
}

QString Manager::toolCacheFilePath(const QString &sessionId) const
{
    QString safe = sessionId;
    safe.replace(QLatin1Char('/'), QLatin1Char('_'));
    return cacheFilePath(sessionId).left(cacheFilePath(sessionId).lastIndexOf(QLatin1Char('/')) + 1)
        + safe + QStringLiteral("-tools.json");
}

bool Manager::loadToolCache(const QString &sessionId)
{
    QFile f(toolCacheFilePath(sessionId));
    if (!f.open(QIODevice::ReadOnly))
        return false;
    const QJsonObject root = QJsonDocument::fromJson(f.readAll()).object();
    f.close();

    m_tools.clear();
    for (const auto &v : root.value("tools").toArray())
        m_tools << v.toString();

    m_sessionExts.clear();
    for (const auto &v : root.value("sessionExtensions").toArray())
        m_sessionExts << v.toString();

    m_extDefs.clear();
    for (const auto &v : root.value("extensions").toArray()) {
        const QJsonObject o = v.toObject();
        ExtDef d;
        d.name = o.value("name").toString();
        d.type = o.value("type").toString();
        d.attrib = o.value("attrib").toBool();
        d.raw = o.value("raw").toObject();
        for (const auto &tool : o.value("availableTools").toArray())
            d.availableTools << tool.toString();
        if (!d.name.isEmpty())
            m_extDefs << d;
    }

    m_toolCatalog.clear();
    const QJsonObject catalogs = root.value("catalog").toObject();
    for (auto it = catalogs.constBegin(); it != catalogs.constEnd(); ++it) {
        QStringList tools;
        for (const auto &v : it.value().toArray())
            tools << v.toString();
        m_toolCatalog.insert(it.key(), tools);
    }
    publishToolGroups();
    emit toolsChanged();
    return true;
}

void Manager::saveToolCache(const QString &sessionId) const
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

    QFile f(toolCacheFilePath(sessionId));
    if (f.open(QIODevice::WriteOnly | QIODevice::Truncate)) {
        f.write(QJsonDocument(root).toJson(QJsonDocument::Compact));
        f.close();
    }
}
