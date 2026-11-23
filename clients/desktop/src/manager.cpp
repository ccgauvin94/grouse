// SPDX-License-Identifier: AGPL-3.0-or-later

#include "manager.h"

#include "corebridge.h"
#include "appbridge.h"
#include "markdown.h"
#include "messagelistmodel.h"
#include "myday.h"
#include "notifier.h"
#include "roamlistmodel.h"
#include "sessionlistmodel.h"

#include <cmath>
#include <type_traits>

#include <QDateTime>
#include <QDebug>
#include <QCryptographicHash>
#include <QScopeGuard>

#include <QDir>
#include <QDesktopServices>
#include <QFile>
#include <QFileDialog>
#include <QFileInfo>
#include <QGuiApplication>
#include <QStyleHints>
#include <QUrl>
#include <QClipboard>
#include <QImage>
#include <QMimeData>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QRegularExpression>
#include <QMimeDatabase>
#include <QSaveFile>
#include <QSet>
#include <QStandardPaths>
#include <QStringList>
#include <QElapsedTimer>
#include <QMetaObject>
#include <QRunnable>
#include <QThreadPool>
#include <QTimer>
#include <QUrl>



// ---------------------------------------------------------------------------
// Implementation notes
//
// This is the thin-client Manager: it owns NO wire. Every Q_INVOKABLE below
// becomes a call into the grouse-core C ABI (via CoreBridge::api()), and every
// event from the wire arrives through the CoreBridge listener table on the Qt
// main thread as a `coreOn*` handler. The core owns the connection, the
// transcript, streaming and reconnect; the Manager only renders state the core
// reports and sends intents the user triggers.
//
// The core serializes structured records/enums as JSON (serde external
// tagging). The coreOn* handlers parse that JSON and fold it into the models'
// existing QVariant shapes so the QML surface is unchanged.
// ---------------------------------------------------------------------------

Manager::Manager(QObject *parent)
    : QObject(parent)
    , m_settings(*this)
    , m_transcript(*this)
    , m_catalog(*this)
    , m_chat(*this)
{
    m_sessionsModel = new SessionListModel(this);
    m_messageModel = new MessageListModel(this);
    m_roamModel = new RoamListModel(this);

    // ONE worker thread for the core's blocking intents (open a chat, create one).
    // One, not a pool: two opens must run in CLICK ORDER, because the last paint is
    // the one that stays — a slower earlier open landing after a later one would
    // leave the wrong chat's transcript on screen.
    m_io = new QThreadPool(this);
    m_io->setMaxThreadCount(1);

    // Stall detector. Every number we have so far says the OPEN is cheap (worker 0ms)
    // while the transcript appears ~300-400ms later; what that cannot say is whether
    // the window was frozen for that time or merely empty. This can: a 100ms timer
    // that finds 250ms+ since its last tick is reporting the main thread being busy,
    // and names the chat it was busy on.
    m_stallTimer = new QTimer(this);
    m_stallTimer->setInterval(100);
    m_stallClock.start();
    connect(m_stallTimer, &QTimer::timeout, this, [this] {
        const qint64 gap = m_stallClock.restart();
        if (gap > 250)
            timingLog(QStringLiteral("main thread stalled %1ms (%2)")
                          .arg(gap)
                          .arg(m_currentSessionId.isEmpty() ? QStringLiteral("no chat")
                                                            : m_currentSessionId));
    });
    m_stallTimer->start();

    // The sidebar's collapsed groups are the user's own state, not a per-launch
    // default: restore what was last chosen and persist every toggle. Without
    // this the tree resets to "everything expanded" on each start.
    m_sessionsModel->setCollapsedSections(m_settings.sidebarCollapsed());
    connect(m_sessionsModel, &SessionListModel::collapsedChanged, this, [this] {
        m_settings.setSidebarCollapsed(m_sessionsModel->collapsedSections());
    });

    // Coalesce transcript updates while a turn streams (same rate-limit as the
    // old per-chunk path): the signal fires at most every 50ms.
    m_peekTimer = new QTimer(this);
    m_peekTimer->setSingleShot(true);
    connect(m_peekTimer, &QTimer::timeout, this, &Manager::doPeekStep);

    // A paint that never ends must not swallow the transcript: if no Window follows
    // its Reset (a load that failed mid-stream), land what arrived.
    m_paintTimer = new QTimer(this);
    m_paintTimer->setSingleShot(true);
    // 3s is the safety valve for a paint that dies before its Window. The env override
    // exists so the test that covers the valve can run in milliseconds instead of
    // spending three seconds asleep (and turning into a flake on a loaded machine).
    int flushMs = 3000;
    bool flushOk = false;
    const int flushEnv = qEnvironmentVariableIntValue("GROUSE_PAINT_FLUSH_MS", &flushOk);
    if (flushOk && flushEnv > 0)
        flushMs = flushEnv;
    m_paintTimer->setInterval(flushMs);
    connect(m_paintTimer, &QTimer::timeout, this, &Manager::flushPaint);

    m_updateTimer = new QTimer(this);
    m_updateTimer->setSingleShot(true);
    m_updateTimer->setInterval(50);
    connect(m_updateTimer, &QTimer::timeout, this, [this] {
        GROUSE_PHASE("phase messagesChanged");
        emit messagesChanged();
    });

    // Longer than the repaint coalescer on purpose: this must not fire between two
    // chunks of the same reply, only once the stream has actually gone quiet.
    m_tailTimer = new QTimer(this);
    m_tailTimer->setSingleShot(true);
    m_tailTimer->setInterval(700);
    connect(m_tailTimer, &QTimer::timeout, this, &Manager::renderTail);

    // Thread label -> session id is NOT persisted here: it arrives from the core on
    // every My Day refresh (coreOnTasks). A per-device copy is what forked a thread's
    // conversation between clients (2026-11-01: "Thread: Global entry" twice, 82 and
    // 99 messages, 21 minutes apart).

    // The thread label is derived from the CURRENT session, so it changes whenever the
    // current session does — ChatPage's panel binds to it and refreshes on it. One
    // signal-to-signal connection beats remembering to emit at every open/new site.
    connect(this, &Manager::currentSessionChanged,
            this, &Manager::currentThreadLabelChanged);

    // My Day's refresh watchdog: a call that dies before `on_tasks` must not leave
    // the page claiming it is still loading.
    m_tasksWatchdog = new QTimer(this);
    m_tasksWatchdog->setSingleShot(true);
    m_tasksWatchdog->setInterval(15000);
    connect(m_tasksWatchdog, &QTimer::timeout, this, [this] {
        if (!m_tasksLoading)
            return;
        m_tasksLoading = false;
        if (m_tasksError.isEmpty())
            m_tasksError = QStringLiteral("My Day: no reply from the server");
        emit tasksChanged();
    });

    // The core is the sole wire path. dlopen + resolve on first use.
    CoreBridge *bridge = CoreBridge::instance();
    m_bridge = bridge;
    bridge->setTarget(this);
    bridge->installListener();
    // The two things every later measurement depends on: WHICH build is running (a
    // stale bundle is indistinguishable from a bug) and where its timings go.
    if (m_bridge->isAvailable() && m_bridge->api().grouse_abi_version) {
        qInfo("Grouse desktop: core ABI %u; open timings -> %s",
              m_bridge->api().grouse_abi_version(),
              qUtf8Printable(timingLogPath()));
    } else {
        qWarning("Grouse desktop: core library NOT loaded; open timings -> %s",
                 qUtf8Printable(timingLogPath()));
    }
}

Manager::~Manager()
{
    // The bridge's event marshaller uses this Manager as its invokeMethod context,
    // but a callback already in flight can hold a raw Manager pointer. Drop the
    // target FIRST: queued events are then discarded, and the bridge is not left
    // pointing at a partly destroyed QObject while the worker tasks wind down.
    if (m_bridge)
        m_bridge->clearTarget();

    // A task still inside the core must finish before the bridge (and the core
    // library) can go away: it holds the handle and calls through the ABI table.
    // Bounded: a task parked in an RPC must not hang the quit. After this the
    // queued work is dropped, and any task still running finishes against a handle
    // that is still alive until the bridge goes (it outlives this object).
    m_chat.waitForCore();
}



void Manager::runOnCore(std::function<void()> work)
{
    m_chat.runOnCore(work);
}
QString Manager::timingLogPath()
{
    return CoreApi::timingLogPath();
}
void Manager::timingLog(const QString &line)
{
    CoreApi::timingLog(line);
}


void Manager::flushPaint()
{
    m_transcript.flushPaint();
}
void Manager::requestMessagesUpdate()
{
    m_transcript.requestMessagesUpdate();
}
QString Manager::host() const
{
    return m_settings.host();
}
QString Manager::port() const
{
    return m_settings.port();
}
QString Manager::secretKey() const
{
    return m_settings.secretKey();
}
bool Manager::useTls() const
{
    return m_settings.useTls();
}
bool Manager::acceptInvalidCerts() const
{
    return m_settings.acceptInvalidCerts();
}
bool Manager::autoConnectEnabled() const
{
    return m_settings.autoConnectEnabled();
}
bool Manager::notificationsEnabled() const
{
    return m_settings.notificationsEnabled();
}
bool Manager::configuredProvidersOnly() const
{
    return m_settings.configuredProvidersOnly();
}
bool Manager::runSummaries() const
{
    return m_settings.runSummaries();
}
bool Manager::rescaleImages() const
{
    return m_settings.rescaleImages();
}
bool Manager::advisor() const
{
    return m_settings.advisor();
}
bool Manager::debugTools() const
{
    return m_settings.debugTools();
}
QString Manager::workingDir() const
{
    return m_settings.workingDir();
}
bool Manager::roamEnabled() const
{
    return m_settings.roamEnabled();
}
void Manager::setRoamEnabled(bool v)
{
    m_settings.setRoamEnabled(v);
}
void Manager::setHost(const QString &v)
{
    m_settings.setHost(v);
}
void Manager::setPort(const QString &v)
{
    m_settings.setPort(v);
}
void Manager::setSecretKey(const QString &v)
{
    m_settings.setSecretKey(v);
}
void Manager::setUseTls(bool v)
{
    m_settings.setUseTls(v);
}
void Manager::setAcceptInvalidCerts(bool v)
{
    m_settings.setAcceptInvalidCerts(v);
}
void Manager::setAutoConnectEnabled(bool v)
{
    m_settings.setAutoConnectEnabled(v);
}
void Manager::setNotificationsEnabled(bool v)
{
    m_settings.setNotificationsEnabled(v);
}
void Manager::setConfiguredProvidersOnly(bool v)
{
    m_settings.setConfiguredProvidersOnly(v);
}
void Manager::setRunSummaries(bool v)
{
    m_settings.setRunSummaries(v);
}
void Manager::setRescaleImages(bool v)
{
    m_settings.setRescaleImages(v);
}
void Manager::syncRescaleToCore()
{
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_set_image_rescale)
        return;
    m_bridge->api().grouse_set_image_rescale(m_bridge->handle(), rescaleImages() ? 1 : 0);
}

void Manager::setDebugTools(bool v)
{
    m_settings.setDebugTools(v);
}
QString Manager::turnLog() const
{
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_turn_log)
        return QString();
    return m_bridge->takeString(m_bridge->api().grouse_turn_log(m_bridge->handle()));
}

QString Manager::turnDiag(const QString &sessionId) const
{
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_turn_diag)
        return QString();
    const QByteArray sid = sessionId.toUtf8();
    return m_bridge->takeString(
        m_bridge->api().grouse_turn_diag(m_bridge->handle(), sid.constData()));
}

void Manager::setAdvisor(bool v)
{
    m_settings.setAdvisor(v);
}
void Manager::dismissAdvisorReport()
{
    if (m_advisorReport.isEmpty() && m_advisorSession.isEmpty())
        return;
    m_advisorReport.clear();
    m_advisorSession.clear();
    emit advisorReportChanged();
}

void Manager::syncAdvisorToCore()
{
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_set_advisor)
        return;
    m_bridge->api().grouse_set_advisor(m_bridge->handle(), advisor() ? 1 : 0);
}

void Manager::syncRunSummariesToCore()
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    m_bridge->api().grouse_set_run_summaries(m_bridge->handle(), runSummaries() ? 1 : 0);
}
void Manager::setWorkingDir(const QString &v)
{
    m_settings.setWorkingDir(v);
}
QString Manager::wsUrl() const
{
    // Kept for the Connect dialog; the core builds its own WebSocket from the
    // same settings. wss is the norm (goosed serves a self-signed cert); ws is
    // only for a server that does not terminate TLS itself.
    return QStringLiteral("%1://%2:%3/acp")
        .arg(useTls() ? QStringLiteral("wss") : QStringLiteral("ws"),
             host().trimmed(), port().trimmed());
}

bool Manager::devChat() const
{
    if (m_currentSessionId.isEmpty())
        return false;
    for (const auto &value : m_sessions) {
        const QVariantMap row = value.toMap();
        if (row.value("sessionId").toString() == m_currentSessionId)
            return row.value("mode").toString() == QLatin1String("dev");
    }
    return false;
}

QString Manager::currentSessionTitle() const
{
    if (!m_currentSessionTitle.isEmpty())
        return m_currentSessionTitle;
    if (m_currentSessionId.isEmpty())
        return QStringLiteral("New Chat");
    // A roam display id is an internal handle (`roam:<peer>:<id>`), not a name:
    // an untitled peer chat reads as peer + its own session id rather than
    // dumping the transport namespace into the window title and header.
    static const QString roamPrefix = QStringLiteral("roam:");
    if (m_currentSessionId.startsWith(roamPrefix)) {
        const QString rest = m_currentSessionId.mid(roamPrefix.size());
        const int sep = rest.lastIndexOf(QLatin1Char(':'));
        if (sep > 0)
            return rest.left(sep) + QStringLiteral(" · ") + rest.mid(sep + 1);
        return rest;
    }
    return m_currentSessionId;
}

QObject *Manager::sessionsModel() const { return m_sessionsModel; }
QObject *Manager::messageModel() const { return m_messageModel; }

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

QString Manager::activeSessionId() const
{
    if (!m_bridge || !m_bridge->isAvailable())
        return QString();
    return m_bridge->takeString(m_bridge->api().grouse_active_session_id(m_bridge->handle()));
}

QString Manager::roamPeerForSession(const QString &sessionId)
{
    if (!sessionId.startsWith(QLatin1String("roam:")))
        return {};
    const QString rest = sessionId.mid(5);
    const int sep = rest.indexOf(QLatin1Char(':'));
    return sep > 0 ? rest.left(sep) : QString();
}

QString Manager::scopedKey(const QString &sessionId, const QString &localKey)
{
    // The display id already carries the peer (`roam:<peer>:<id>`), so it is the
    // complete owner; '|' matches the key the phone builds for the same card.
    return sessionId + QLatin1Char('|') + localKey;
}

QString Manager::fileCardKey(const QString &sessionId, const QString &path) const
{
    return scopedKey(sessionId, path);
}

QString Manager::sessionPeerLabel(const QString &sessionId) const
{
    if (sessionId.isEmpty())
        return {};
    if (m_bridge && m_bridge->isAvailable() && m_bridge->api().grouse_session_owner) {
        const QByteArray sid = sessionId.toUtf8();
        char *raw = m_bridge->api().grouse_session_owner(m_bridge->handle(), sid.constData());
        const QString json = m_bridge->takeString(raw);
        if (!json.isEmpty()) {
            const QJsonObject owner = QJsonDocument::fromJson(json.toUtf8()).object();
            if (owner.value(QStringLiteral("kind")).toString() == QLatin1String("roam"))
                return owner.value(QStringLiteral("label")).toString();
            // The core answered: a MAIN id is main even when its text looks like
            // a roam handle (a vanished peer's id, a session the server reused).
            return {};
        }
    }
    // No core in this process (unit tests, an older library): the namespace is
    // the only hint available, and it is still better than attributing a peer's
    // turn to the main wire.
    return roamPeerForSession(sessionId);
}

bool Manager::mainWireConnected() const
{
    return m_chat.mainWireConnected();
}

void Manager::bindSessionAfterAttach()
{
    m_chat.bindSessionAfterAttach();
}

bool Manager::sessionIsOpen() const
{
    // The UI's idea of the current session must agree with the core's bound one.
    // They diverge exactly when a chat is on screen without a session behind it
    // — a `session/load` the server rejected leaves NO session bound, while its
    // cached transcript keeps painting (CONTRACT §3.2, grouse#19).
    if (m_currentSessionId.isEmpty())
        return false;
    return activeSessionId() == m_currentSessionId;
}

// ---------------------------------------------------------------------------
// ServerConfig JSON (input to grouse_connect)
// ---------------------------------------------------------------------------

QString Manager::serverConfigJson()
{
    return CoreApi::serverConfigJson(*this);
}
// ---------------------------------------------------------------------------
// Prompt JSON (input to grouse_send_prompt): serde Prompt { blocks: [...] }
// ---------------------------------------------------------------------------

QString Manager::promptJson(const QString &text, const QVariantList &blocks) const
{
    return CoreApi::promptJson(text, blocks);
}
// ---------------------------------------------------------------------------
// Intents (UI -> core)
// ---------------------------------------------------------------------------

void Manager::connectToServer()
{
    m_chat.connectToServer();
}
void Manager::autoConnect()
{
    m_chat.autoConnect();
}
void Manager::disconnect()
{
    m_chat.disconnect();
}
void Manager::connectRoam(const QString &card, const QString &label)
{
    m_chat.connectRoam(card, label);
}
void Manager::syncRoamIdentityToCore()
{
    m_chat.syncRoamIdentityToCore();
}
void Manager::disconnectRoam(const QString &label)
{
    m_chat.disconnectRoam(label);
}
void Manager::persistRoamCard(const QString &label, const QString &card)
{
    m_chat.persistRoamCard(label, card);
}
void Manager::forgetRoamCard(const QString &label)
{
    m_chat.forgetRoamCard(label);
}
void Manager::restoreRoamPeers()
{
    m_chat.restoreRoamPeers();
}
QVariantList Manager::formatCardPins(const QVariantMap &pins)
{
    return CoreApi::formatCardPins(pins);
}
QVariantMap Manager::parseCardPinRefusal(const QString &status, const QString &peerLabel)
{
    return CoreApi::parseCardPinRefusal(status, peerLabel);
}
QVariantList Manager::roamCardPins()
{
    return m_chat.roamCardPins();
}
bool Manager::clearRoamCardPin(const QString &peer, const QString &confirmation)
{
    return m_chat.clearRoamCardPin(peer, confirmation);
}
bool Manager::redialRoamPeer(const QString &label)
{
    return m_chat.redialRoamPeer(label);
}
void Manager::dismissRoamPinRefusal()
{
    if (m_roamPinRefusal.isEmpty())
        return;
    m_roamPinRefusal.clear();
    emit roamPinRefusalChanged();
}

void Manager::maybeShowCardPinRefusal(const QString &status, const QString &peerLabel)
{
    const QVariantMap parsed = parseCardPinRefusal(status, peerLabel);
    if (parsed.isEmpty() || parsed.value("message") == m_roamPinRefusal.value("message"))
        return;                     // one dialog per refusal, not one per status burst
    m_roamPinRefusal = parsed;
    emit roamPinRefusalChanged();
}

void Manager::openRoamSession(const QString &label, const QString &sessionId, const QString &cwd)
{
    m_chat.openRoamSession(label, sessionId, cwd);
}
void Manager::newRoamSession(const QString &label)
{
    m_chat.newRoamSession(label);
}
void Manager::newRoamSessionIn(const QString &label, const QString &cwd)
{
    m_chat.newRoamSessionIn(label, cwd);
}
void Manager::newRoamSessionWithMode(const QString &label, const QString &cwd,
                                     const QString &mode)
{
    m_chat.newRoamSessionWithMode(label, cwd, mode);
}
void Manager::toggleRoamPeer(const QString &label)
{
    m_roamModel->togglePeer(label);
}

void Manager::setActiveTab(const QString &tab)
{
    m_chat.setActiveTab(tab);
}
QString Manager::roamIdentity()
{
    return m_chat.roamIdentity();
}
QString Manager::roamPublicKey() const
{
    return m_chat.roamPublicKey();
}
QString Manager::roamCard() const
{
    return m_chat.roamCard();
}
void Manager::copyToClipboard(const QString &s)
{
    QGuiApplication::clipboard()->setText(s);
}

QString Manager::highlightCode(const QString &code, const QString &lang,
                               const QString &keywordColor,
                               const QString &stringColor,
                               const QString &commentColor,
                               const QString &numberColor)
{
    return ::highlightCode(code, lang, keywordColor, stringColor, commentColor,
                           numberColor);
}

QObject *Manager::roamModel() const { return m_roamModel; }

void Manager::testConnection()
{
    m_chat.testConnection();
}
void Manager::openSession(const QString &sessionId)
{
    m_chat.openSession(sessionId);
}
QString Manager::sessionStamp(const QString &sessionId) const
{
    return m_chat.sessionStamp(sessionId);
}
void Manager::refreshSession()
{
    m_chat.refreshSession();
}
void Manager::reloadSessionFromStart()
{
    m_chat.reloadSessionFromStart();
}
void Manager::resetReloadProgress()
{
    if (!m_reloading && m_replayLoaded == 0 && m_replayTotal == 0)
        return;
    m_reloading = false;
    m_replayLoaded = 0;
    m_replayTotal = 0;
    emit reloadProgressChanged();
}

void Manager::resetContextUsage()
{
    if (m_contextUsed == 0 && m_contextSize == 0)
        return;
    m_contextUsed = 0;
    m_contextSize = 0;
    emit contextChanged();
}

void Manager::coreOnReplayProgress(const QString &sessionId, int loaded, int total, bool done)
{
    // Only the chat on screen owns the bar: a background replay (a peer, or a
    // chat opened behind this one) must not paint its progress here.
    if (!sessionId.isEmpty() && sessionId != m_currentSessionId)
        return;
    m_replayLoaded = loaded;
    m_replayTotal = total;
    m_reloading = !done;
    emit reloadProgressChanged();
}

void Manager::refreshSessionWith(int full)
{
    m_chat.refreshSessionWith(full);
}

void Manager::newChat()
{
    m_chat.newChat();
}
void Manager::newChatInMode(const QString &mode)
{
    m_chat.newChatInMode(mode);
}
void Manager::beginChat()
{
    m_chat.beginChat();
}
void Manager::sendPrompt(const QString &text, const QVariantList &images)
{
    m_chat.sendPrompt(text, images);
}
void Manager::dispatchSend(const QString &text, const QVariantList &blocks,
                           const QStringList &files)
{
    m_chat.dispatchSend(text, blocks, files);
}
bool Manager::wireUpForCurrentChat() const
{
    return m_chat.wireUpForCurrentChat();
}
void Manager::releaseTurnForLostWire(const QString &lostSessionId)
{
    m_chat.releaseTurnForLostWire(lostSessionId);
}
void Manager::setActiveRunId(const QString &runId)
{
    m_chat.setActiveRunId(runId);
}
void Manager::enqueue(const PendingSend &p)
{
    m_chat.enqueue(p);
}

int Manager::queuedCount() const
{
    return m_chat.queuedCount();
}
QVariant Manager::queuedSends() const
{
    return m_chat.queuedSends();
}
void Manager::steerQueued(int id)
{
    m_chat.steerQueued(id);
}
void Manager::cancelQueued(int id)
{
    m_chat.cancelQueued(id);
}
void Manager::restoreTurnState(const QString &sessionId)
{
    m_chat.restoreTurnState(sessionId);
}
void Manager::flushQueue()
{
    m_chat.flushQueue();
}


QStringList Manager::pickAttachmentFiles()
{
    return m_chat.pickAttachmentFiles();
}
QStringList Manager::clipboardAttachmentFiles()
{
    return m_chat.clipboardAttachmentFiles();
}
QVariantList Manager::buildAttachmentBlocks(const QVariantList &paths)
{
    return m_chat.buildAttachmentBlocks(paths);
}
QStringList Manager::attachmentPaths(const QVariantList &paths)
{
    return m_chat.attachmentPaths(paths);
}


QVariantMap Manager::attachmentCard(const QString &path) const
{
    return m_transcript.attachmentCard(path);
}
QVariantMap Manager::serverFileCard(const QString &name, const QString &mime, qint64 bytes) const
{
    return m_transcript.serverFileCard(name, mime, bytes);
}
void Manager::cancelTurn()
{
    m_chat.cancelTurn();
}
void Manager::compactConversation()
{
    m_chat.compactConversation();
}
void Manager::exportSessionTo(const QString &sessionId, const QString &filePath)
{
    if (!m_bridge || !m_bridge->isAvailable() || filePath.isEmpty())
        return;
    m_pendingExportPath = filePath;
    const QByteArray sid = sessionId.toUtf8();
    m_bridge->api().grouse_unstable_export_session(m_bridge->handle(), sid.constData());
}

void Manager::unarchiveSession(const QString &sessionId)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QByteArray sid = sessionId.toUtf8();
    m_bridge->api().grouse_unarchive_session(m_bridge->handle(), sid.constData());
}

void Manager::respondPermission(const QString &requestId, const QString &optionId)
{
    const QString id = requestId.isEmpty() ? m_permToolCallId : requestId;
    const int index = requestQueueIndex(m_permissionQueue, id);
    if (index < 0)
        return;   // a late dialog answer must not deny a DIFFERENT request
    m_permissionQueue.removeAt(index);
    syncPermissionFront();

    if (m_bridge && m_bridge->isAvailable()) {
        const QByteArray rid = id.toUtf8();
        const QByteArray outcome = optionId.isEmpty()
            ? QByteArrayLiteral("\"Cancelled\"")
            : QJsonDocument(QJsonObject{{"Selected", QJsonObject{{"option_id", optionId}}}})
                  .toJson(QJsonDocument::Compact);
        // The queue key is the core's permission id (today: tool_call_id, the
        // key `respond_permission` routes by; the record also carries the owner
        // and display session so the prompt can name the right chat/wire).
        m_bridge->api().grouse_respond_permission(m_bridge->handle(), rid.constData(), outcome.constData());
    }
}

void Manager::setConfigOption(const QString &id, const QString &value)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    char *err = nullptr;
    const QByteArray cid = id.toUtf8();
    const QByteArray v = value.toUtf8();
    m_bridge->api().grouse_set_config_option(m_bridge->handle(), cid.constData(), v.constData(), &err);
    if (err)
        m_bridge->api().grouse_string_free(err);
}

void Manager::refreshSessions()
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    // The CORE's merged snapshot is the sidebar's list (audit §3): one call, one
    // set of SessionSummary rows for the main wire and every roam peer. The old
    // path listed main + each peer separately and re-derived owners by prefix,
    // which is why a list fetched while a peer chat was open grouped the local
    // chats under that peer until the next refresh. `coreOnRoamSessions` still
    // fills the separate ROAM ENDPOINT model — that one is connection management,
    // not the conversation list.
    const CoreBridge::AimsApi &api = m_bridge->api();
    if (api.grouse_all_sessions)
        api.grouse_all_sessions(m_bridge->handle());
    else
        api.grouse_list_sessions(m_bridge->handle());
}

void Manager::refreshProjects()
{
    if (m_bridge && m_bridge->isAvailable())
        m_bridge->api().grouse_unstable_sources_list(m_bridge->handle(), "project");
}

void Manager::createProject(const QString &name)
{
    QString n = name.trimmed();
    if (n.isEmpty() || n.size() > 64)
        return;
    for (const QChar &c : n) {
        if (!c.isLower() && !c.isDigit() && c != QLatin1Char('-'))
            return;
    }
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QByteArray type = QByteArrayLiteral("project");
    const QByteArray nm = n.toUtf8();
    const QByteArray empty = QByteArray();
    m_bridge->api().grouse_unstable_sources_create(m_bridge->handle(), type.constData(),
                                                   nm.constData(), empty.constData(), empty.constData());
}

void Manager::deleteProject(const QString &nameOrPath)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    QString path;
    for (const auto &v : m_projects) {
        const QVariantMap p = v.toMap();
        if (p.value("id").toString() == nameOrPath || p.value("name").toString() == nameOrPath) {
            path = p.value("path").toString();
            break;
        }
    }
    if (!path.isEmpty()) {
        const QByteArray type = QByteArrayLiteral("project");
        const QByteArray p = path.toUtf8();
        m_bridge->api().grouse_unstable_sources_delete(m_bridge->handle(), type.constData(), p.constData());
    }
}

void Manager::moveSessionToProject(const QString &sessionId, const QString &projectId)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QByteArray sid = sessionId.toUtf8();
    const QByteArray pid = projectId.isEmpty() ? QByteArray() : projectId.toUtf8();
    m_bridge->api().grouse_unstable_session_project(m_bridge->handle(), sid.constData(),
                                                    projectId.isEmpty() ? nullptr : pid.constData());
    // The C call is synchronous (block_on), so the server has committed the move
    // by the time it returns. Re-read sessions so the sidebar regroups the thread
    // under its new project — the model keys each section off the session's
    // projectId from session/list, and session_project only re-lists project
    // sources, never sessions. (Android hides this by patching its list locally.)
    refreshSessions();
}

void Manager::newChatInProject(const QString &projectId)
{
    m_pendingProjectFiling = projectId;
    newChat();
}

// ---------------------------------------------------------------------------
// Thread chats (CONTRACT §3.6)
// ---------------------------------------------------------------------------

QString Manager::assistantProject() const
{
    // Read from the CORE, never from a literal here: the core is what filters these
    // sessions out of every chat list (session::in_assistant_project), and a client
    // that spelled the slug differently would file chats nothing would ever find.
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_assistant_project)
        return QString();
    return m_bridge->takeString(m_bridge->api().grouse_assistant_project());
}

QString Manager::currentThreadLabel() const
{
    if (m_currentSessionId.isEmpty())
        return QString();
    for (auto it = m_threadSessions.cbegin(); it != m_threadSessions.cend(); ++it) {
        if (it.value() == m_currentSessionId)
            return it.key();
    }
    // Nothing else. The core used to answer this too, and its answer could be the label of
    // a chat that was never opened — which painted one area's cards above every later
    // conversation. The map is idle-by-construction: a row only has a label while the chat
    // on screen IS that thread's.
    return QString();
}

void Manager::openThreadChat(const QString &label)
{
    const QString trimmed = label.trimmed();
    if (trimmed.isEmpty())
        return;
    // The CORE opens it, creating the conversation the first time: the label -> session
    // map lives on the server (assistant-state `thread_sessions`), so a second device
    // lands in the SAME chat. Each client used to keep its own map, which is how one
    // label ended up with two conversations.
    closeMyDay();
    // A label the mirror already carries opens NOW, through the ordinary chat-open path
    // (turn state, tools, title). Asking the core and waiting for its answer left the
    // window on the previous chat for a round trip — the first version of this had the
    // core bind the chat itself, which the ready handler refused precisely because it was
    // an id nobody had asked to display.
    const auto known = m_threadSessions.constFind(trimmed);
    timingLog(QStringLiteral("thread click \"%1\" -> %2")
                  .arg(trimmed,
                       (known != m_threadSessions.constEnd() && !known.value().isEmpty())
                           ? known.value() : QStringLiteral("(not in the mirror: asking the core)")));
    if (known != m_threadSessions.constEnd() && !known.value().isEmpty()) {
        openSession(known.value());
        return;
    }
    if (m_bridge && m_bridge->isAvailable() && m_bridge->api().grouse_open_thread) {
        const QByteArray l = trimmed.toUtf8();
        m_bridge->api().grouse_open_thread(m_bridge->handle(), l.constData());
    }
    // The composer is left EMPTY. The chat opens with its own opening line (the core
    // writes it when it mints the conversation) and the panel above paints the thread's
    // rows, so a pre-typed prompt only put words in the box that nobody wrote.
    emit draftRequested(QString());
}

void Manager::openTaskSession(const QString &sessionId, const QString &thread)
{
    // The session that did the work has the history; the thread chat is the
    // fallback when the worker outlived its session (a scheduled leg is deleted
    // by the nightly sweep, its task row is not).
    if (!sessionId.isEmpty())
        openSession(sessionId);
    else if (!thread.isEmpty())
        openThreadChat(thread);
}

QString Manager::assistantSessionId() const
{
    // The always-on thread is an ordinary chat titled "Assistant" (the server pins
    // the name with `user_set_name`), which is also how notify-mcp finds it. Newest
    // wins if several were ever made.
    QString best;
    QString bestStamp;
    for (const auto &v : m_sessions) {
        const QVariantMap s = v.toMap();
        if (s.value(QStringLiteral("title")).toString().trimmed() != QLatin1String("Assistant"))
            continue;
        const QString id = s.value(QStringLiteral("sessionId")).toString();
        // A peer can have an "Assistant" too; the always-on local thread must be
        // resolved through the core's owner answer, not the id's spelling.
        if (id.isEmpty() || !sessionPeerLabel(id).isEmpty())
            continue;
        const QString stamp = s.value(QStringLiteral("updatedAt")).toString();
        if (best.isEmpty() || stamp > bestStamp) {
            best = id;
            bestStamp = stamp;
        }
    }
    return best;
}

void Manager::openAssistant()
{
    const QString id = assistantSessionId();
    if (id.isEmpty()) {
        setStatus(QStringLiteral("no Assistant thread on this server yet"));
        return;
    }
    closeMyDay();
    openSession(id);
}

void Manager::askAssistant(const QString &text)
{
    if (text.trimmed().isEmpty())
        return;
    const QString id = assistantSessionId();
    if (id.isEmpty()) {
        setStatus(QStringLiteral("no Assistant thread on this server yet"));
        return;
    }
    closeMyDay();
    if (m_currentSessionId != id)
        openSession(id);
    // `send` routes by the on-screen chat, so the thread has to be current first;
    // a prompt that cannot go out yet is parked by the core and flushed on Ready.
    dispatchSend(text.trimmed(), {});
}

void Manager::setDraft(const QString &text)
{
    emit draftRequested(text);
}

void Manager::refreshRecipes()
{
    m_catalog.refreshRecipes();
}
void Manager::runRecipe(const QString &id)
{
    m_catalog.runRecipe(id);
}
void Manager::scheduleRecipe(const QString &id, const QString &cron)
{
    m_catalog.scheduleRecipe(id, cron);
}
void Manager::deleteRecipe(const QString &id)
{
    m_catalog.deleteRecipe(id);
}
void Manager::saveRecipe(const QString &id, const QString &recipeJson)
{
    m_catalog.saveRecipe(id, recipeJson);
}
void Manager::setSchedulePaused(const QString &scheduleId, bool paused)
{
    m_catalog.setSchedulePaused(scheduleId, paused);
}
void Manager::runScheduleNow(const QString &scheduleId)
{
    m_catalog.runScheduleNow(scheduleId);
}
void Manager::renameSession(const QString &sessionId, const QString &title)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QByteArray sid = sessionId.toUtf8();
    const QByteArray t = title.toUtf8();
    m_bridge->api().grouse_rename_session(m_bridge->handle(), sid.constData(), t.constData());
}

void Manager::archiveSession(const QString &sessionId)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QByteArray sid = sessionId.toUtf8();
    m_bridge->api().grouse_archive_session(m_bridge->handle(), sid.constData());
}

void Manager::deleteSession(const QString &sessionId)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QByteArray sid = sessionId.toUtf8();
    m_bridge->api().grouse_delete_session(m_bridge->handle(), sid.constData());
    if (sessionId == m_currentSessionId) {
        resetContextUsage();
        m_currentSessionId.clear();
        m_currentSessionTitle.clear();
        m_messageModel->clear();
        m_tools.clear();
        m_currentIndex = -1;
        m_landing = true;
        emit landingChanged();
        emit messagesChanged();
        emit currentSessionChanged();
    emit pinnedAppsChanged();
        emit toolsChanged();
    }
}

// ---- skills ----------------------------------------------------------------

void Manager::refreshSkills()
{
    m_catalog.refreshSkills();
}
void Manager::saveSkill(const QString &path, const QString &name,
                        const QString &description, const QString &content)
{
    m_catalog.saveSkill(path, name, description, content);
}
void Manager::saveProject(const QString &path, const QString &name,
                          const QString &description, const QString &content)
{
    if (!m_bridge || !m_bridge->isAvailable() || path.isEmpty())
        return;
    const QByteArray type = QByteArrayLiteral("project");
    const QByteArray p = path.toUtf8();
    const QByteArray nm = name.toUtf8();
    const QByteArray d = description.toUtf8();
    const QByteArray c = content.toUtf8();
    m_bridge->api().grouse_unstable_sources_update(m_bridge->handle(), type.constData(),
                                                   p.constData(), nm.constData(), d.constData(), c.constData());
}

void Manager::deleteSkill(const QString &path)
{
    m_catalog.deleteSkill(path);
}
void Manager::publishPushEndpoint(const QString &url)
{
    if (url.isEmpty())
        return;
    // One key per client so two devices never overwrite each other, and no Grouse
    // code reads it back: this exists solely so an operator's own sender can find
    // the desktop's endpoint (docs/NOTIFICATIONS.md).
    setServerConfig(QStringLiteral("GROUSE_PUSH_ENDPOINT_DESKTOP"), url);
}

QString Manager::pushParse(const QString &raw) const
{
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_push_parse)
        return QString();
    const QByteArray r = raw.toUtf8();
    return m_bridge->takeString(m_bridge->api().grouse_push_parse(r.constData()));
}

QString Manager::pushDecide(const QString &envelopeJson, const QString &contextJson) const
{
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_push_decide)
        return QString();
    const QByteArray e = envelopeJson.toUtf8();
    const QByteArray c = contextJson.toUtf8();
    return m_bridge->takeString(m_bridge->api().grouse_push_decide(e.constData(), c.constData()));
}

QString Manager::pushDecideRun(const QString &envelopeJson, const QString &contextJson,
                               const QString &announcedRunId) const
{
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_push_decide_run)
        return pushDecide(envelopeJson, contextJson);
    const QByteArray e = envelopeJson.toUtf8();
    const QByteArray c = contextJson.toUtf8();
    const QByteArray r = announcedRunId.isNull() ? QByteArray() : announcedRunId.toUtf8();
    return m_bridge->takeString(
        m_bridge->api().grouse_push_decide_run(e.constData(), c.constData(),
                                               announcedRunId.isNull() ? nullptr : r.constData()));
}

void Manager::setServerConfig(const QString &key, const QString &value)
{
    m_catalog.setServerConfig(key, value);
}
void Manager::readServerConfig(const QString &key)
{
    m_catalog.readServerConfig(key);
}
void Manager::refreshSupportedModels(const QString &providerId)
{
    m_catalog.refreshSupportedModels(providerId);
}
void Manager::refreshToolGroups()
{
    m_catalog.refreshToolGroups();
}
void Manager::discoverToolGroup(const QString &extName)
{
    m_catalog.discoverToolGroup(extName);
}
void Manager::setSessionExtensionEnabled(const QString &extName, bool enabled)
{
    m_catalog.setSessionExtensionEnabled(extName, enabled);
}
void Manager::setSessionToolEnabled(const QString &extName, const QString &toolName, bool on)
{
    m_catalog.setSessionToolEnabled(extName, toolName, on);
}
QVariant Manager::globalExtensions() const
{
    return m_catalog.globalExtensions();
}
void Manager::refreshGlobalExtensions()
{
    m_catalog.refreshGlobalExtensions();
}
void Manager::setGlobalExtensionEnabled(const QString &extKey, bool enabled)
{
    m_catalog.setGlobalExtensionEnabled(extKey, enabled);
}
void Manager::setGlobalToolEnabled(const QString &extName, const QString &toolName, bool on)
{
    m_catalog.setGlobalToolEnabled(extName, toolName, on);
}
const Manager::ExtDef *Manager::extDef(const QString &key) const
{
    return m_catalog.extDef(key);
}
const Manager::ExtDef *Manager::globalExtDef(const QString &key) const
{
    return m_catalog.globalExtDef(key);
}
void Manager::setSessionTools(const QString &extName, const QStringList &allowed)
{
    m_catalog.setSessionTools(extName, allowed);
}
void Manager::publishToolGroups()
{
    m_catalog.publishToolGroups();
}
void Manager::loadToolCache(const QString &sessionId)
{
    m_catalog.loadToolCache(sessionId);
}
void Manager::saveToolCache(const QString &sessionId) const
{
    m_catalog.saveToolCache(sessionId);
}
// ---------------------------------------------------------------------------
// CoreBridge event handlers (main-thread entry points — see corebridge.cpp)
//
// Rendering contract (matches the core's emission discipline in
// core/grouse-core/src/transcript.rs): the core emits BOTH an on_stream chunk
// AND an on_transcript Append/Update for the same text/tool event. To avoid
// double-rendering, this Manager renders
//   * user/agent/thought/error bubbles from on_transcript (authoritative), and
//   * tool / chart / MCP-App bubbles + usage / run-ended from on_stream.
// The chunk-level text handlers below are kept for API compatibility but are
// NOT driven by on_stream for text (the transcript carries full bubbles).
// ---------------------------------------------------------------------------

static QJsonObject parseObj(const QString &json)
{
    return QJsonDocument::fromJson(json.toUtf8()).object();
}
static QJsonArray parseArr(const QString &json)
{
    return QJsonDocument::fromJson(json.toUtf8()).array();
}

void Manager::coreOnStatus(const QString &json)
{
    m_chat.onStatus(json);
}
namespace {
// The project key as the server holds it: the short name ("hacking"). Android's
// ProjectSummary.toInfo() derives exactly this from the <name>.md basename of
// sources/list, and a session's project id arrives in either form — the short
// name (older writes) or the full path (newer ones). Both must collapse to one
// group or the sidebar shows the project twice.
QString projectKey(const QString &raw)
{
    QString k = raw.section(QLatin1Char('/'), -1);
    if (k.endsWith(QLatin1String(".md")))
        k.chop(3);
    return k;
}
} // namespace

void Manager::coreOnSessions(const QString &json)
{
    GROUSE_PHASE("phase sessions");
    QVariantList sessions;
    const QJsonArray arr = parseArr(json);
    for (const auto &el : arr) {
        const QJsonObject o = el.toObject();
        QVariantMap m;
        m["sessionId"] = o.value("id").toString();
        m["id"] = o.value("id").toString();
        m["title"] = o.value("title").toString();
        m["updatedAt"] = o.value("updated_at").toString();
        m["lastMessageAt"] = o.value("updated_at").toString();
        m["snippet"] = o.value("last_message_snippet").toString();
        m["projectId"] = projectKey(o.value("project_id").toString());
        m["messageCount"] = o.value("message_count").toVariant();
        m["hasRecipe"] = o.value("has_recipe").toBool();
        m["archived"] = o.value("archived").toBool();
        // The core DERIVES this (`grouse.mode`: chat | dev): the server returns no tags
        // on a row, so the core asks which sessions match the value and stamps it.
        m["mode"] = o.value("mode").toString();
        // This handler now receives the CORE'S MERGED list: main + peer sessions,
        // each with its `roam:<peer>:<id>` display id. Ask the core which wire owns
        // the row rather than parsing the prefix or — worse — using the peer whose
        // CHAT happens to be on screen (that bug turned the whole local list into a
        // single `peer: <label>` group, 2026-11-20). SessionListModel still owns the
        // grouping/collapse policy; it only needs an honest `peer` field.
        const QString peer = sessionPeerLabel(m["sessionId"].toString());
        if (!peer.isEmpty()) {
            m["peer"] = peer;
            // A peer's title used to arrive only in coreOnRoamSessions. The merged
            // list carries it too, so opening a peer chat by cached id has a title
            // even before the endpoint's own list lands.
            const QString title = m["title"].toString();
            if (!title.isEmpty())
                m_roamTitles.insert(m["sessionId"].toString(), title);
        }
        sessions << m;
    }
    onSessions(sessions);
}

void Manager::coreOnConfig(const QString &json)
{
    QVariantList config;
    const QJsonArray arr = parseArr(json);
    for (const auto &el : arr) {
        const QJsonObject o = el.toObject();
        QVariantMap m{{"id", o.value("id").toString()},
                      {"name", o.value("name").toString()},
                      {"currentValue", o.value("value").toString()}};
        QVariantList choices;
        for (const auto &c : o.value("choices").toArray()) {
            const QJsonObject co = c.toObject();
            choices << QVariantMap{{"value", co.value("value").toString()},
                                   {"name", co.value("name").toString()}};
        }
        m["choices"] = choices;
        config << m;
    }
    onConfig(config);
}

void Manager::coreOnAdvisorReport(const QString &sid, const QString &text)
{
    // A review is about the TURN, not the chat: it is shown beside that turn and
    // dropped when the reader moves on (a new turn, another chat, a dismissal).
    // Keeping it in the transcript would send it back to the model as history the
    // next time the chat is prompted — the advisor would end up talking to itself.
    if (text.trimmed().isEmpty())
        return;
    m_advisorSession = sid;
    m_advisorReport = text;
    emit advisorReportChanged();
}

QVariantMap Manager::parsePermissionRequest(const QString &json)
{
    return CoreApi::parsePermissionRequest(json);
}
int Manager::requestQueueIndex(const QVariantList &queue, const QString &requestId)
{
    return CoreApi::requestQueueIndex(queue, requestId);
}
QString Manager::requestOwnerLabel(const QVariant &owner, const QString &sessionId)
{
    return CoreApi::requestOwnerLabel(owner, sessionId);
}
void Manager::syncPermissionFront(bool notify)
{
    if (m_permissionQueue.isEmpty()) {
        m_permToolCallId.clear();
        m_permTitle.clear();
        m_permOptions.clear();
        return;
    }
    const QVariantMap req = m_permissionQueue.first().toMap();
    m_permToolCallId = req.value("requestId").toString();
    const QString title = req.value("title").toString();
    const QString where = req.value("ownerLabel").toString();
    const QString sid = req.value("sessionId").toString();
    // Name BOTH the wire and the chat: a tool call id means nothing to the
    // person, and the screen may be showing a different chat entirely.
    QString ownerText = where;
    if (!sid.isEmpty() && !ownerText.contains(sid))
        ownerText = ownerText.isEmpty() ? sid : QStringLiteral("%1 · %2").arg(where, sid);
    m_permTitle = ownerText.isEmpty() ? title : QStringLiteral("%1 — %2").arg(title, ownerText);
    m_permOptions = req.value("options").toList();
    // The dialog that just answered closes on the same click. Publishing the
    // next request synchronously would make its onClosed deny that next one.
    if (notify)
        QTimer::singleShot(0, this, [this]() { emit permissionRequested(); });
}

void Manager::coreOnPermission(const QString &json, const QString &owner,
                               const QString &requestId)
{
    QJsonObject root = parseObj(json);
    const bool wrapped = root.value(QStringLiteral("PermissionRequest")).isObject();
    QJsonObject payload = wrapped
        ? root.value(QStringLiteral("PermissionRequest")).toObject() : root;
    if (!owner.isEmpty() && !payload.contains(QStringLiteral("owner")))
        payload.insert(QStringLiteral("owner"), owner);
    if (!requestId.isEmpty() && !payload.contains(QStringLiteral("request_id")))
        payload.insert(QStringLiteral("request_id"), requestId);
    if (wrapped)
        root.insert(QStringLiteral("PermissionRequest"), payload);
    else
        root = payload;
    const QVariantMap req = parsePermissionRequest(
        root.isEmpty() ? json
                       : QString::fromUtf8(QJsonDocument(root).toJson(QJsonDocument::Compact)));
    const QString id = req.value("requestId").toString();
    if (id.isEmpty())
        return;                     // nothing to answer by id
    if (requestQueueIndex(m_permissionQueue, id) >= 0)
        return;                     // redelivery must not replace a parked request
    const bool raiseDialog = m_permissionQueue.isEmpty();
    m_permissionQueue << req;
    syncPermissionFront(raiseDialog);

    if (notificationsEnabled() && !Notifier::appVisible())
        Notifier::send(QStringLiteral("Grouse needs approval"),
                       QStringLiteral("Allow “%1”?").arg(m_permTitle));
}

QString Manager::permissionToolCallId() const
{
    return m_permToolCallId;
}

QString Manager::permissionOwner() const
{
    return m_permissionQueue.isEmpty() ? QString()
        : m_permissionQueue.first().toMap().value("ownerLabel").toString();
}

int Manager::permissionCount() const
{
    return m_permissionQueue.size();
}

void Manager::notifyTurnFinished()
{
    if (!notificationsEnabled())
        return;
    // The shared policy decides: decode nothing (this turn was watched live, not pushed),
    // but ask the same question — is the user looking? is it ours to announce? — and use
    // the same wording the phone would.
    const QJsonObject envelope{{QStringLiteral("kind"), QStringLiteral("Turn")},
                               {QStringLiteral("session_id"), m_currentSessionId},
                               {QStringLiteral("text"), lastAssistantText()}};
    const QString peer = sessionPeerLabel(m_currentSessionId);
    const QJsonObject ctx{{QStringLiteral("app_visible"), Notifier::appVisible()},
                          {QStringLiteral("armed_session"), m_currentSessionId},
                          {QStringLiteral("armed_owner"), peer.isEmpty() ? QJsonValue::Null : QJsonValue(peer)},
                          {QStringLiteral("session_title"), m_currentSessionTitle},
                          {QStringLiteral("announce_any_turn"), true}};
    const QJsonObject d = parseObj(pushDecideRun(
        QString::fromUtf8(QJsonDocument(envelope).toJson(QJsonDocument::Compact)),
        QString::fromUtf8(QJsonDocument(ctx).toJson(QJsonDocument::Compact)), QString()));
    if (d.value(QStringLiteral("show")).toBool()) {
        Notifier::send(d.value(QStringLiteral("summary")).toString(),
                       d.value(QStringLiteral("body")).toString());
        // Remember it: the operator's sender pushes for this same turn end, and the
        // shared policy suppresses that second announcement.
        m_announcedTurnSession = m_currentSessionId;
        m_announcedTurnAtMs = QDateTime::currentMSecsSinceEpoch();
    }
}

int Manager::announcedTurnSecsAgo() const
{
    if (m_announcedTurnSession.isEmpty() || m_announcedTurnAtMs == 0)
        return -1;
    return int((QDateTime::currentMSecsSinceEpoch() - m_announcedTurnAtMs) / 1000);
}

void Manager::coreOnSessionTouched(const QString &sid, const QString &title, const QString &u)
{
    Q_UNUSED(u);
    // The core performs its own debounced resync of the active session. The UI
    // only needs to refresh the sidebar so order/title/status reflect the touch.
    // A touch on a session we are NOT looking at is the one case the window can't
    // show: another client, or a scheduled run, did something. Announce it there.
    // A client-local event, not a push payload, so it bypasses the shared policy: the
    // phone renders it as a sidebar badge instead (it has a session list to badge; the
    // desktop has a sidebar and no badge model).
    if (sid != m_currentSessionId && notificationsEnabled() && !Notifier::appVisible())
        Notifier::send(title.isEmpty() ? QStringLiteral("Session updated") : title,
                       QStringLiteral("Changed by another client or a scheduled run."));
    refreshSessions();
}

void Manager::coreOnProjects(const QString &json)
{
    QVariantList projects;
    const QJsonArray arr = parseArr(json);
    for (const auto &el : arr) {
        const QJsonObject o = el.toObject();
        const QString path = o.value("path").toString();
        const QString name = o.value("name").toString();
        // Group/combo key = the short name form (see projectKey); "path" stays
        // the full sources/list path for delete/create round-trips.
        const QString id = projectKey(path).isEmpty() ? name : projectKey(path);
        // The project's content IS its instructions file (projects/<name>.md);
        // goose feeds it to sessions filed under the project. The working root
        // is not a field — it lives as a `root:` line inside the content
        // (Android's parseProjects does the same extraction).
        const QString content = o.value("content").toString();
        QString root;
        const QStringList contentLines = content.split(QLatin1Char('\n'));
        for (const QString &line : contentLines) {
            const QString t = line.trimmed();
            if (t.startsWith(QLatin1String("root:"))) {
                root = t.mid(5).trimmed();
                break;
            }
        }
        projects << QVariantMap{{"id", id},
                                {"name", name},
                                {"path", path},
                                {"description", o.value("description").toString()},
                                {"content", content},
                                {"root", root},
                                {"writable", o.value("writable").toBool(true)}};
    }
    onProjects(projects);
}

void Manager::coreOnRoamPeerStatus(const QString &label, const QString &status)
{
    // Roam reports the pin mismatch here with the PEER LABEL, which the generic
    // status envelope may not carry. This is the dialog's source of truth.
    maybeShowCardPinRefusal(status, label);
    const bool down = status == QStringLiteral("disconnected") || status.startsWith(QStringLiteral("error:"));
    m_roamModel->setPeerStatus(label, status, !down);
    emit wireUpChanged();
    // A settled dial failure is terminal: the turn it owned ended with it.
    if (down && label == m_activePeerLabel)
        releaseTurnForLostWire(m_currentSessionId);
    // The roam row elides the status at 130px, hiding the actual dial failure.
    // Surface errors in the main status bar so the real cause is readable.
    if (status.startsWith(QStringLiteral("error:")))
        setStatus(status);
}

void Manager::coreOnRoamSessions(const QString &label, const QString &json)
{
    QVariantList sessions;
    const QJsonArray arr = parseArr(json);
    for (const auto &el : arr) {
        const QJsonObject o = el.toObject();
        QVariantMap m;
        m["sessionId"] = o.value("id").toString();
        m["id"] = o.value("id").toString();
        m["title"] = o.value("title").toString();
        m["updatedAt"] = o.value("updated_at").toString();
        m["lastMessageAt"] = o.value("updated_at").toString();
        // Remember the title: a peer's session/list is the ONLY channel that
        // carries one (this side has no session/info for a peer), and
        // openRoamSession() needs it to label the window.
        const QString roamSid = m.value("sessionId").toString();
        const QString roamTitle = m.value("title").toString();
        if (!roamSid.isEmpty() && !roamTitle.isEmpty())
            m_roamTitles.insert(roamSid, roamTitle);
        // The roam card shows the snippet, falling back to "N msg" — carry
        // both. This handler used to drop them, so every roam card read
        // "0 msg" no matter what the peer's session/list carried.
        m["snippet"] = o.value("last_message_snippet").toString();
        m["messageCount"] = o.value("message_count").toVariant();
        m["projectId"] = projectKey(o.value("project_id").toString());
        m["hasRecipe"] = o.value("has_recipe").toBool();
        m["archived"] = o.value("archived").toBool();
        // The effective mode the core stamped on this row ("chat" when the peer's
        // per-mode walk claimed nothing) — the roam sidebar's `</>` marker.
        m["mode"] = o.value("mode").toString();
        m["peer"] = label;
        sessions << m;
    }
    // Newest first. The core hands the peer's list over verbatim; without a
    // sort here the card order is whatever the peer's session/list emitted.
    // PHP-stable: sort by parsed updatedAt desc, falls back to the raw string
    // when a timestamp does not parse (all-tie keeps the delivered order).
    std::stable_sort(sessions.begin(), sessions.end(),
        [](const QVariant &a, const QVariant &b) {
            const auto ta = QDateTime::fromString(a.toMap().value("updatedAt").toString(), Qt::ISODate);
            const auto tb = QDateTime::fromString(b.toMap().value("updatedAt").toString(), Qt::ISODate);
            const quint64 ea = ta.isValid() ? ta.toMSecsSinceEpoch() : 0;
            const quint64 eb = tb.isValid() ? tb.toMSecsSinceEpoch() : 0;
            return ea > eb;
        });
    // Newest first (comment above the sort).
    m_roamModel->setPeerSessions(label, sessions);
    // The open chat's own title can arrive late (the peer titles a session after
    // its first turn) or change on a rename — follow it while it is on screen.
    const QString current = m_roamTitles.value(m_currentSessionId);
    if (!current.isEmpty() && current != m_currentSessionTitle) {
        m_currentSessionTitle = current;
        emit currentSessionChanged();
    }
}

void Manager::coreOnPeerNewSession(const QString &label, const QString &sid)
{
    refreshSessions();
    if (label != m_activePeerLabel || sid.isEmpty())
        return;
    // The peer reports its RAW id; the core routes a `roam:<label>:<id>` display id
    // to the owning peer. Setting the raw id here bound NOTHING (the main wire tried
    // to open a session it never created) and left the fresh chat's title and turn
    // state attributed to the wrong wire. Go through the ordinary opener so core
    // routing, the window title and the turn owner all agree (Android:3560).
    const QString prefixed = QStringLiteral("roam:%1:%2").arg(label, sid);
    openSession(prefixed);
}

void Manager::coreOnActiveRun(const QString &sid, const QString &runId)
{
    m_chat.onActiveRunChanged(sid, runId);
}
void Manager::coreOnCommands(const QString &json)
{
    QStringList commands;
    for (const auto &c : parseArr(json))
        commands << c.toString();
    onCommands(commands);
}

void Manager::coreOnSessionOpened(const QString &sessionId)
{
    m_chat.onSessionOpened(sessionId);
}
void Manager::coreOnSessionOpenFailed(const QString &sessionId, const QString &message)
{
    m_chat.onSessionOpenFailed(sessionId, message);
}
// ---------------------------------------------------------------------------
// My Day + thread chats (CONTRACT §3.6)
// ---------------------------------------------------------------------------

void Manager::setMyDayVisible(bool v)
{
    if (m_myDayVisible == v)
        return;
    m_myDayVisible = v;
    emit myDayVisibleChanged();
}

void Manager::openMyDay()
{
    // A second press is the refresh gesture: the lists change inside goose's own
    // scheduled sessions, so there is nothing to subscribe to and re-reading is
    // the only way to see them move.
    setMyDayVisible(true);
    paintCachedTasks();
    refreshTasks();
}

/// Paint the board this machine last saw, off the core's disk cache. Called on
/// every open BEFORE the refresh: the refresh is four round trips, and this is
/// the only thing that fills the page when there is no wire at all — which is
/// exactly when the last board is worth the most.
void Manager::paintCachedTasks()
{
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_tasks_paint)
        return; // an older core: nothing cached, the page behaves as it always did
    // The cache read (and the on_tasks callback it emits) is core work: on the
    // main thread it stalls the page the user just opened. Same single worker as
    // every other blocking intent, so ordering with refreshTasks is preserved.
    const CoreBridge::AimsApi api = m_bridge->api();
    void *h = m_bridge->handle();
    runOnCore([api, h] { api.grouse_tasks_paint(h); });
}

void Manager::closeMyDay()
{
    setMyDayVisible(false);
}

void Manager::requestContextReport()
{
    if (!m_bridge || !m_bridge->isAvailable()
        || !m_bridge->api().grouse_unstable_context_report) {
        return;
    }
    const QString sid = activeSessionId();
    if (sid.isEmpty())
        return;
    // Whatever was on screen belongs to the previous chat: clear it so the sheet cannot
    // paint one chat's breakdown under another chat's donut. A server without the method
    // simply never answers, and nothing is emitted.
    if (!m_contextReport.isEmpty() || !m_contextReportTotal.isEmpty()) {
        m_contextReport.clear();
        m_contextReportTotal.clear();
        emit contextReportChanged();
    }
    const QByteArray id = sid.toUtf8();
    m_bridge->api().grouse_unstable_context_report(m_bridge->handle(), id.constData());
}

void Manager::coreOnContextReport(const QString &sid, const QString &json)
{
    // Only the chat on screen: a breakdown belongs to one donut. (The core already drops a
    // peer's report for a session that is not displayed; this is the same rule one level up,
    // for a report that arrives after the user has moved on.)
    if (sid != activeSessionId())
        return;
    QVariantList segments;
    const QJsonDocument doc = QJsonDocument::fromJson(json.toUtf8());
    const QJsonArray rows = doc.object().value(QStringLiteral("segments")).toArray();
    for (const QJsonValue &v : rows) {
        const QJsonObject o = v.toObject();
        QVariantMap seg;
        seg.insert(QStringLiteral("category"), o.value(QStringLiteral("category")).toString());
        seg.insert(QStringLiteral("label"), o.value(QStringLiteral("label")).toString());
        seg.insert(QStringLiteral("tokens"),
                   static_cast<qlonglong>(o.value(QStringLiteral("tokenCount")).toDouble()));
        seg.insert(QStringLiteral("preview"), o.value(QStringLiteral("contentPreview")).toString());
        QVariantList parts;
        for (const QJsonValue &p : o.value(QStringLiteral("parts")).toArray()) {
            const QJsonObject po = p.toObject();
            QVariantMap part;
            part.insert(QStringLiteral("label"), po.value(QStringLiteral("label")).toString());
            part.insert(QStringLiteral("tokens"),
                        static_cast<qlonglong>(po.value(QStringLiteral("tokenCount")).toDouble()));
            part.insert(QStringLiteral("preview"),
                        po.value(QStringLiteral("contentPreview")).toString());
            parts.append(part);
        }
        seg.insert(QStringLiteral("parts"), parts);
        segments.append(seg);
    }
    const QJsonObject root = doc.object();
    QVariantMap total;
    total.insert(QStringLiteral("total"),
                 static_cast<qlonglong>(root.value(QStringLiteral("totalTokens")).toDouble()));
    total.insert(QStringLiteral("limit"),
                 static_cast<qlonglong>(root.value(QStringLiteral("model"))
                                            .toObject()
                                            .value(QStringLiteral("contextLimit"))
                                            .toDouble()));
    total.insert(QStringLiteral("pending"),
                 root.value(QStringLiteral("autoCompactionPending")).toBool());
    m_contextReport = segments;
    m_contextReportTotal = total;
    emit contextReportChanged();
}

QVariantList buildActivityLanes(const QJsonArray &lanes)
{
    QVariantList running, finished;
    // Walk backwards: the core emits lanes in transcript order (oldest first), and
    // both groups must read newest first. Nothing here mutates the input snapshot.
    for (qsizetype i = lanes.size() - 1; i >= 0; --i) {
        const QJsonObject laneJson = lanes.at(i).toObject();
        const QString id = laneJson.value(QStringLiteral("id")).toString();
        if (id.isEmpty())
            continue;
        QVariantMap lane = laneJson.toVariantMap();
        // The core's `running` is the only trustworthy liveness signal. A stale
        // `status` / `outcome` can read "in_progress" forever after a dropped turn,
        // so grouping on either of those would keep a dead lane at the top.
        const bool isRunning = laneJson.value(QStringLiteral("running")).toBool(false);
        lane.insert(QStringLiteral("section"),
                    isRunning ? QStringLiteral("running") : QStringLiteral("finished"));
        lane.insert(QStringLiteral("finished"), !isRunning);
        if (isRunning)
            running.append(lane);
        else
            finished.append(lane);
    }
    running.append(finished);
    return running;
}

void Manager::refreshActivity()
{
    GROUSE_PHASE("phase activity");
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_activity) {
        if (!m_activity.isEmpty()) {
            m_activity.clear();
            emit activityChanged();
        }
        return;
    }
    const QString json =
        m_bridge->takeString(m_bridge->api().grouse_activity(m_bridge->handle()));
    const QJsonDocument doc = QJsonDocument::fromJson(json.toUtf8());
    // The steps stay a QVariantList of maps (QJsonValue::toVariant already does that),
    // so a delegate row can still expand into its sub-agent calls. Comparison below is
    // against the FINAL ordered/enriched list: reordering must repaint, an unchanged
    // snapshot must not (this runs on every tool item that lands).
    const QVariantList lanes = buildActivityLanes(doc.array());
    // A no-op refresh must not repaint: this is called on every tool item that lands.
    if (lanes == m_activity)
        return;
    m_activity = lanes;
    emit activityChanged();
}

void Manager::refreshTasks()
{
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_tasks_refresh) {
        m_tasksLoading = false;
        m_tasksError = QStringLiteral("My Day: the core has no task surface");
        emit tasksChanged();
        return;
    }
    if (!m_online) {
        // Offline there is nothing to ask. The page's empty state already says the
        // lists load once the server is reachable, so this is not an error — just
        // stop claiming to load.
        m_tasksLoading = false;
        emit tasksChanged();
        return;
    }
    m_tasksLoading = true;
    m_tasksError.clear();
    emit tasksChanged();
    m_bridge->api().grouse_tasks_refresh(m_bridge->handle());
    if (m_tasksWatchdog)
        m_tasksWatchdog->start();
}

QString Manager::dueLabel(const QString &iso) const
{
    return MyDay::dueLabel(iso, QDate::currentDate());
}

QString Manager::wakeLabel(const QString &iso) const
{
    return MyDay::wakeLabel(iso, QDateTime::currentDateTime());
}

QString Manager::threadName(const QString &label) const
{
    return MyDay::threadName(label);
}

QString Manager::artifactsLabel(const QStringList &paths) const
{
    return MyDay::artifactsLabel(paths);
}

bool Manager::rowSnoozed(const QVariantMap &row) const
{
    return MyDay::isSnoozed(row, QDateTime::currentDateTime());
}

QString Manager::rowMeta(const QVariantMap &row, bool snoozed) const
{
    return MyDay::metaLine(row, snoozed, QDate::currentDate(), QDateTime::currentDateTime());
}

QString Manager::stampLabel(const QString &iso, const QString &prefix) const
{
    return MyDay::stampLabel(iso, prefix, QDateTime::currentDateTime());
}

void Manager::snoozeTaskToMorning(const QString &id)
{
    // UTC with a Z suffix: `nextMorning` is LOCAL 05:00, and the server reads a naive
    // stamp as UTC — sending "2026-10-31T05:00:00" from Denver would snooze until
    // 23:00 the previous evening, which is to say not at all.
    const QDateTime wake = MyDay::nextMorning(QDateTime::currentDateTime());
    snoozeTask(id, wake.toUTC().toString(Qt::ISODate), QStringLiteral("snoozed from My Day"));
}

void Manager::wakeTask(const QString &id)
{
    // Waking is a snooze whose `until` has already passed: the server's own wake
    // pass (rank/maintain) flips it back to open, and there is no separate tool.
    snoozeTask(id, QDateTime::currentDateTimeUtc().toString(Qt::ISODate),
               QStringLiteral("brought back from My Day"));
}

void Manager::closeTask(const QString &id, const QString &state, const QString &resolution)
{
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_task_close)
        return;
    const QByteArray i = id.toUtf8();
    const QByteArray s = state.toUtf8();
    const QByteArray r = resolution.toUtf8();
    m_bridge->api().grouse_task_close(m_bridge->handle(), i.constData(), s.constData(),
                                      r.constData());
}

void Manager::snoozeTask(const QString &id, const QString &until, const QString &reason)
{
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_task_snooze)
        return;
    const QByteArray i = id.toUtf8();
    const QByteArray u = until.toUtf8();
    const QByteArray r = reason.toUtf8();
    m_bridge->api().grouse_task_snooze(m_bridge->handle(), i.constData(), u.constData(),
                                       r.constData());
}

void Manager::reactTask(const QString &id, const QString &reaction)
{
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_task_react)
        return;
    const QByteArray i = id.toUtf8();
    const QByteArray r = reaction.toUtf8();
    m_bridge->api().grouse_task_react(m_bridge->handle(), i.constData(), r.constData());
}

void Manager::coreOnFile(const QString &json)
{
    m_transcript.applyFile(json);
}
void Manager::fetchFile(const QString &sessionId, const QString &path,
                        const QString &name, const QString &mime,
                        const QString &sha256)
{
    m_transcript.fetchFile(sessionId, path, name, mime, sha256);
}
void Manager::coreOnThreadReady(const QString &label, const QString &sessionId)
{
    Q_UNUSED(label);
    if (sessionId.isEmpty())
        return;
    // Same reason as `openThreadChat`'s fast path: the chat is bound here, by the ordinary
    // opener. The answer can only arrive for a label the mirror did not have — a thread
    // being created — so there is nothing to reconcile beyond opening it.
    openSession(sessionId);
}

void Manager::coreOnTasks(const QString &json)
{
    // Verbatim, for a page that renders nothing to be compared against the wire;
    // the decoded lists are what the UI binds. myday.cpp is the one place that
    // knows the envelope's shape, and its unit test pins the translation.
    m_tasksJson = json;
    const MyDay::Lists lists = MyDay::parse(json);
    m_myDayOpen = lists.open;
    m_myDayDone = lists.done;
    m_myDayHanded = lists.handed;
    m_tasksError = lists.error;
    // The thread map, replaced wholesale: mirrored from the core, never written here.
    m_threadSessions.clear();
    for (auto it = lists.threads.cbegin(); it != lists.threads.cend(); ++it) {
        const QString sid = it.value().toString();
        if (!it.key().isEmpty() && !sid.isEmpty())
            m_threadSessions.insert(it.key(), sid);
    }
    emit currentThreadLabelChanged();
    if (lists.cached) {
        // A cached board: the rows are the last answer this machine got, so paint
        // them but keep saying "refreshing" — and keep the watchdog armed, because
        // the live envelope has not arrived (and may never, offline).
        emit tasksChanged();
        return;
    }
    m_tasksLoading = false;
    if (m_tasksWatchdog)
        m_tasksWatchdog->stop();
    emit tasksChanged();
}

void Manager::coreOnExport(const QString &data)
{
    onExportResult(data);
}

namespace {
// The C ABI's owner argument is either a bare label or a SessionOwner JSON
// record. Both collapse to the same QVariant shape parsePermissionRequest uses.
QVariant ownerFromWire(const QString &raw)
{
    const QString s = raw.trimmed();
    if (s.isEmpty())
        return {};
    if (s.startsWith(QLatin1Char('{'))) {
        const QVariant v = QJsonDocument::fromJson(s.toUtf8()).object().toVariant();
        if (!v.toMap().isEmpty())
            return v;
    }
    return s;
}
} // namespace

void Manager::coreOnRecipeParams(const QString &owner, const QString &requestId,
                                 const QString &parameters)
{
    // A recipe started with declared parameters (via `_meta.recipeId`) cannot
    // run until they are supplied. The core PARKED the request, so dropping it
    // here (what the empty stub did) left the turn hanging until the server
    // timed it out — the form is not optional.
    QVariantList fields;
    for (const auto &el : parseArr(parameters)) {
        const QJsonObject o = el.toObject();
        const QString key = o.value("key").toString();
        if (key.isEmpty())
            continue;
        QVariantList options;
        for (const auto &opt : o.value("options").toArray()) {
            const QString v = opt.toString();
            options << QVariantMap{{"value", v}, {"label", v}};
        }
        // The form model has no `default` slot, so it rides in the description:
        // visible and copyable, but it cannot prefill.
        QString desc = o.value("description").toString();
        const QString def = o.value("default").toString();
        if (!def.isEmpty())
            desc = desc.isEmpty() ? QStringLiteral("default: %1").arg(def)
                                  : QStringLiteral("%1 (default: %2)").arg(desc, def);
        fields << QVariantMap{
            {"name", key},
            {"type", o.value("input_type").toString(QStringLiteral("string"))},
            {"title", key},
            {"description", desc},
            {"options", options},
            {"required", o.value("requirement").toString() == QLatin1String("required")},
        };
    }
    const QString where = requestOwnerLabel(ownerFromWire(owner), QString());
    const QString base = QStringLiteral("Recipe parameters");
    QVariantMap form;
    form["title"] = where.isEmpty() ? base : QStringLiteral("%1 — %2").arg(base, where);
    form["message"] = QStringLiteral("This recipe needs values before it can start.");
    form["recipeParams"] = true;
    form["owner"] = where;
    form["requestId"] = requestId.isEmpty()
        ? QStringLiteral("legacy-%1").arg(m_formQueue.size()) : requestId;
    form["wireRequestId"] = requestId;
    form["fields"] = fields;
    queueForm(form);
}

void Manager::coreOnElicitation(const QString &owner, const QString &requestId,
                                const QString &schema)
{
    // elicitation/create, form mode: a tool wants structured input mid-turn.
    // Same shape as Android's ElicitationSheet — booleans, choices, typed text.
    const QJsonObject o = parseObj(schema);
    const QJsonObject props = o.value("properties").toObject();
    QSet<QString> required;
    for (const auto &r : o.value("required").toArray())
        required.insert(r.toString());
    QVariantList fields;
    for (auto it = props.begin(); it != props.end(); ++it) {
        const QJsonObject f = it.value().toObject();
        QVariantList options;
        // Single-select arrives either as untagged `enum` values or as titled
        // `oneOf` [{const, title}] options; both collapse to {value, label}.
        for (const auto &el : f.value("oneOf").toArray()) {
            const QJsonObject eo = el.toObject();
            const QString v = eo.value("const").toString();
            if (!v.isEmpty())
                options << QVariantMap{{"value", v},
                                       {"label", eo.value("title").toString(v)}};
        }
        if (options.isEmpty())
            for (const auto &el : f.value("enum").toArray()) {
                const QString v = el.toString();
                options << QVariantMap{{"value", v}, {"label", v}};
            }
        fields << QVariantMap{
            {"name", it.key()},
            {"type", f.value("type").toString(QStringLiteral("string"))},
            {"title", f.value("title").toString(it.key())},
            {"description", f.value("description").toString()},
            {"options", options},
            {"required", required.contains(it.key())},
        };
    }
    const QString where = requestOwnerLabel(ownerFromWire(owner), QString());
    const QString base = o.value("title").toString(QStringLiteral("Input requested"));
    QVariantMap form;
    form["title"] = where.isEmpty() ? base : QStringLiteral("%1 — %2").arg(base, where);
    form["message"] = o.value("description").toString();
    form["recipeParams"] = false;
    form["owner"] = where;
    form["requestId"] = requestId.isEmpty()
        ? QStringLiteral("legacy-%1").arg(m_formQueue.size()) : requestId;
    form["wireRequestId"] = requestId;
    form["fields"] = fields;
    queueForm(form);
}

void Manager::notifyFormQueue()
{
    if (m_formNotifyPending)
        return;
    m_formNotifyPending = true;
    QTimer::singleShot(0, this, [this]() {
        m_formNotifyPending = false;
        emit pendingFormChanged();
    });
}

void Manager::queueForm(const QVariantMap &form)
{
    // The core parks ONE responder per request id now. A redelivered request
    // must not replace an unanswered form, and a second request of the same
    // family must queue behind it rather than evict it.
    const QString id = form.value("requestId").toString();
    if (requestQueueIndex(m_formQueue, id) >= 0)
        return;
    const bool raiseDialog = m_formQueue.isEmpty();
    m_formQueue << form;
    // A form arriving behind an open dialog must NOT change `Mgr.pendingForm`:
    // QML would rebind the fields the user is filling and answer the wrong id.
    if (raiseDialog)
        notifyFormQueue();
}

QVariant Manager::pendingForm() const
{
    return m_formQueue.isEmpty() ? QVariantMap{} : m_formQueue.first();
}

void Manager::answerForm(const QVariantMap &values, const QString &action)
{
    if (m_formQueue.isEmpty())
        return;
    answerFormFor(m_formQueue.first().toMap().value("requestId").toString(), values, action);
}

void Manager::answerFormFor(const QString &requestId, const QVariantMap &values,
                            const QString &action)
{
    const int index = requestQueueIndex(m_formQueue, requestId);
    if (index < 0)
        return;                     // a stale dialog must not answer the next form
    const QVariantMap form = m_formQueue.takeAt(index).toMap();
    if (index == 0)
        notifyFormQueue();          // only a front answer exposes the next form
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const bool accepted = action == QLatin1String("accept");
    const QByteArray rid = form.value("wireRequestId").toString().toUtf8();
    if (form.value("recipeParams").toBool()) {
        // recipe/request-params takes {action:"submit"|"cancel", values:{k:string}}:
        // every value is a STRING there (the server type is HashMap<String,String>)
        // and there is no decline — a refusal is a cancel.
        QJsonObject vals;
        if (accepted)
            for (auto it = values.begin(); it != values.end(); ++it)
                vals.insert(it.key(), it.value().toString());
        const QByteArray js = QJsonDocument(vals).toJson(QJsonDocument::Compact);
        const QByteArray act = (accepted ? QStringLiteral("submit") : QStringLiteral("cancel")).toUtf8();
        CoreApi::respondRecipeIntent(m_bridge->api().grouse_unstable_respond_recipe_params,
                            m_bridge->handle(), rid, act, js);
        return;
    }
    // elicitation/create: accept carries the typed content object, decline and
    // cancel carry none.
    QJsonObject content;
    if (accepted) {
        for (auto it = values.begin(); it != values.end(); ++it) {
            const QVariant v = it.value();
            switch (v.typeId()) {
            case QMetaType::Bool: content.insert(it.key(), v.toBool()); break;
            case QMetaType::Int:
            case QMetaType::LongLong: content.insert(it.key(), v.toLongLong()); break;
            case QMetaType::Double: content.insert(it.key(), v.toDouble()); break;
            default: content.insert(it.key(), v.toString()); break;
            }
        }
    }
    const QByteArray js = accepted
        ? QJsonDocument(content).toJson(QJsonDocument::Compact) : QByteArray();
    const QString act = accepted ? QStringLiteral("accept")
        : action == QLatin1String("cancel") ? QStringLiteral("cancel")
                                            : QStringLiteral("decline");
    CoreApi::respondElicitationIntent(m_bridge->api().grouse_unstable_respond_elicitation,
                             m_bridge->handle(), rid, act.toUtf8(), js);
}

void Manager::coreOnCompactionStatus(const QString &message)
{
    m_chat.onCompactionStatus(message);
}
void Manager::coreOnMessageUsage(std::uint64_t outTok, std::uint64_t elapsedMs,
                                 std::uint64_t ttftMs, double cost)
{
    QVariantMap usage;
    usage["outputTokens"] = qint64(outTok);
    usage["elapsedMs"] = qint64(elapsedMs);
    usage["timeToFirstTokenMs"] = qint64(ttftMs);
    usage["cost"] = cost;
    onMessageUsage(usage);
}

void Manager::coreOnAppResource(const QString &key, const QString &html)
{
    onAppResource(key, html);
}

void Manager::coreOnRecipes(const QString &json)
{
    QVariantList list;
    for (const auto &el : parseArr(json))
        list << el.toObject().toVariantMap();
    onRecipes(list);
}

void Manager::coreOnSchedules(const QString &json)
{
    QVariantList list;
    for (const auto &el : parseArr(json))
        list << el.toObject().toVariantMap();
    onSchedules(list);
}

void Manager::coreOnUnstableProjects(const QString &json)
{
    coreOnProjects(json);
}

void Manager::coreOnSkills(const QString &json)
{
    QVariantList list;
    for (const auto &el : parseArr(json))
        list << el.toObject().toVariantMap();
    onSkills(list);
}

void Manager::coreOnTools(const QString &sid, const QString &json)
{
    Q_UNUSED(sid);
    QVariantList names;
    for (const auto &el : parseArr(json)) {
        if (el.isObject())
            names << el.toObject().value("name").toString();
        else
            names << el.toString();
    }
    onTools(names);
}

void Manager::coreOnExtensions(const QString &json)
{
    QVariantList list;
    for (const auto &el : parseArr(json))
        list << el.toObject().toVariantMap();
    onExtensions(list);
}

void Manager::coreOnSessionExtensions(const QString &sid, const QString &json)
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

void Manager::coreOnConfigValue(const QString &key, const QString &value)
{
    onServerConfigValue(key, value);
}

void Manager::coreOnSupportedModels(const QString &provider, const QString &json)
{
    QStringList models;
    for (const auto &el : parseArr(json))
        models << (el.isObject() ? el.toObject().value("name").toString() : el.toString());
    onSupportedModels(provider, models);
}

QStringList Manager::configuredProvidersFromJson(const QString &json)
{
    return CatalogController::configuredProvidersFromJson(json);
}
void Manager::coreOnProviders(const QString &json)
{
    m_catalog.onProviders(json);
}
void Manager::refreshProviders()
{
    m_catalog.refreshProviders();
}
QVariantList Manager::filterProviders(const QVariantList &choices, const QStringList &configured,
                                      bool configuredOnly, const QString &current)
{
    return CatalogController::filterProviders(choices, configured, configuredOnly, current);
}
QVariantList Manager::filterConfiguredProviders(const QVariantList &choices, const QString &current) const
{
    return m_catalog.filterConfiguredProviders(choices, current);
}
void Manager::coreOnSessionProbe(const QString &sid, const QString &u, qint64 n)
{
    Q_UNUSED(sid); Q_UNUSED(u); Q_UNUSED(n);
    // The core owns resync probing; nothing to do client-side.
}

void Manager::coreOnToolResult(const QString &text, int isError)
{
    Q_UNUSED(text); Q_UNUSED(isError);
}

void Manager::coreOnError(const QString &method, const QString &message)
{
    // The METHOD is what identifies the failing call in a bug report; the UI
    // shows the message alone. Dropping it here is why a session-id failure took
    // a server-side probe to attribute (grouse#20).
    qWarning("grouse: core error from %s: %s",
             qUtf8Printable(method), qUtf8Printable(message));
    onError(message, false);
}



// ---------------------------------------------------------------------------
// The item stream (on_item): the core's single transcript channel
// (docs/TRANSCRIPT_MODEL.md). The desktop renders from this alone now; the
// legacy on_transcript/on_stream handlers below exist only for the two things
// that are not items (usage and run-ended).
// ---------------------------------------------------------------------------

QVariantList Manager::attachmentsForItem(const QString &id, const QString &text, int count)
{
    return m_transcript.attachmentsForItem(id, text, count);
}
QVariantMap Manager::rowFromItem(const QJsonObject &item)
{
    return m_transcript.rowFromItem(item);
}
void Manager::coreOnItem(const QString &json)
{
    m_transcript.applyItem(json);
}
// ---------------------------------------------------------------------------
// The two non-item families: context-window usage and turn end. Every
// transcript row arrives on on_item (docs/TRANSCRIPT_MODEL.md).
// ---------------------------------------------------------------------------

void Manager::coreOnUsage(qint64 used, qint64 size, double cost, const QString &currency)
{
    onUsage(int(used), int(size), cost, currency);
}

void Manager::coreOnRunEnded(const QString &sessionId, const QString &stopReason)
{
    m_chat.onRunEnded(sessionId, stopReason);
}
// ---------------------------------------------------------------------------
// Model-update handlers (called by the coreOn* entry points above)
// ---------------------------------------------------------------------------

void Manager::finishTail(int lookback)
{
    m_transcript.finishTail(lookback);
}
void Manager::renderTail()
{
    m_transcript.renderTail();
}
void Manager::loadOlder(int count)
{
    m_transcript.loadOlder(count);
}
namespace {
// appKey lengths prefix the cache key: an id containing the separator cannot
// make two different (app, session) pairs collide.
QString appCachePrefix(const QString &appKey)
{
    return QString::number(appKey.size()) + QLatin1Char(':') + appKey;
}

QString appCacheKey(const QString &appKey, const QString &sessionId)
{
    return appCachePrefix(appKey) + sessionId;
}
} // namespace

void Manager::onAppResource(const QString &appKey, const QString &html)
{
    if (!html.isEmpty())
        m_appHtml.insert(appKey, html);
    if (!html.isEmpty() && !m_appUrls.isEmpty()) {
        // Re-fetched template: invalidate EVERY session's URL for this app and
        // revoke their tokens, so an old tab cannot keep posting into a chat.
        // An already-rendered inline view keeps the first snapshot — reload on
        // demand, not churn under the user.
        const QString prefix = appCachePrefix(appKey);
        const QStringList keys = m_appUrls.keys();
        for (const QString &key : keys) {
            if (!key.startsWith(prefix))
                continue;
            if (m_appBridge)
                m_appBridge->revokeToken(m_appTokens.value(key));
            m_appTokens.remove(key);
            m_appUrls.remove(key);
        }
    }
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
    requestMessagesUpdate();
}

namespace { bool g_inlineApps = false; }

QString Manager::appViewUrl(const QString &appKey)
{
    const QString html = m_appHtml.value(appKey);
    if (html.isEmpty())
        return {};
    const QString cacheKey = appCacheKey(appKey, m_currentSessionId);
    const QString existing = m_appUrls.value(cacheKey);
    if (!existing.isEmpty() && m_appBridge && m_appBridge->isTokenValid(m_appTokens.value(cacheKey)))
        return existing;   // stable URL per (app, chat); a refetch or idle
                           // expiry mints a fresh one instead
    if (!existing.isEmpty()) {
        m_appUrls.remove(cacheKey);
        m_appTokens.remove(cacheKey);
    }
    if (!m_appBridge) {
        m_appBridge = new AppBridgeServer(this);
        connect(m_appBridge, &AppBridgeServer::appMessage, this, &Manager::onAppMessage);
    }
    if (!(m_appBridge->running() || m_appBridge->start()))
        return {};
    QString detail, output;
    for (int i = m_messageModel->count() - 1; i >= 0; --i) {
        const QVariantMap m = m_messageModel->row(i);
        if (m.value("role").toString() == QLatin1String("mcpapp")
            && m.value("appKey").toString() == appKey) {
            detail = m.value("detail").toString();
            output = m.value("output").toString();
            break;
        }
    }
    // The bridge mints the 128-bit token and binds it to (appKey, session):
    // a second chat showing the same app gets a DIFFERENT token, so ui/message
    // can never land in the first chat. Tokens expire and can be revoked when
    // the template is refetched.
    const bool dark = QGuiApplication::styleHints()->colorScheme() == Qt::ColorScheme::Dark;
    const QString token = m_appBridge->registerApp(appKey, m_currentSessionId, html, detail, output,
                                                   dark ? QStringLiteral("dark") : QStringLiteral("light"));
    const QString url = m_appBridge->urlFor(token);
    m_appTokens.insert(cacheKey, token);
    m_appUrls.insert(cacheKey, url);
    return url;
}

void Manager::openAppInHtml(const QString &appKey)
{
    // Serve through the loopback bridge so the tab gets a host (ui/initialize
    // answered, ui/message lands back in this chat). The flatpak shares the
    // host network namespace, so the browser reaches 127.0.0.1 fine.
    // There is deliberately NO file:// fallback: the template is server-
    // supplied HTML and must never be written to a predictable temp path.
    const QString url = appViewUrl(appKey);
    if (!url.isEmpty())
        QDesktopServices::openUrl(QUrl(url));
}

int Manager::appBridgePort() const
{
    return m_appBridge ? m_appBridge->port() : 0;
}

bool Manager::linkAllowed(const QString &url) const
{
    return markdownUrlAllowed(url);
}

bool Manager::openLocalFile(const QString &localPath)
{
    // Only a real local file the core already downloaded may be handed to the
    // desktop. Relative paths and symlinks are refused; QML no longer calls
    // openUrlExternally with a file: URL built from server-supplied text.
    if (localPath.isEmpty())
        return false;
    const QFileInfo info(localPath);
    if (!info.isAbsolute() || !info.isFile() || info.isSymlink())
        return false;
    const QString canonical = info.canonicalFilePath();
    if (canonical.isEmpty() || !QFileInfo(canonical).isFile())
        return false;
    return QDesktopServices::openUrl(QUrl::fromLocalFile(canonical));
}

bool Manager::inlineAppsEnabled() const
{
    return g_inlineApps;
}

void Manager::setInlineAppsSupported(bool on)
{
    g_inlineApps = on;
}

bool Manager::inlineAppsSupported()
{
    return g_inlineApps;
}

// ---- per-session MCP-App dock ------------------------------------------------
namespace {
const QString kPinsKey = QStringLiteral("pinned_apps");
}

QVariant Manager::pinnedApps() const
{
    if (m_currentSessionId.isEmpty())
        return QVariantMap{};
    return m_settings.cardPinsFor(m_currentSessionId);
}

void Manager::pinApp(const QString &appKey, const QString &slot)
{
    if (m_currentSessionId.isEmpty() || appKey.isEmpty())
        return;
    const QString key = slot == QLatin1String("bottom") ? QStringLiteral("bottom")
                                                        : QStringLiteral("top");
    QVariantMap pins = pinnedApps().toMap();
    pins[key] = appKey;
    QVariantMap all = m_settings.cardPinsFor(m_currentSessionId);
    all[m_currentSessionId] = pins;
    m_settings.setCardPinsFor(m_currentSessionId, all);
    emit pinnedAppsChanged();
    // The docked pane is a live view; make sure its template is on hand (a pin
    // restored on a cold start has no transcript row that fetched it).
    if (!m_appHtml.contains(appKey) && m_bridge && m_bridge->isAvailable()) {
        const int sep = appKey.indexOf(QLatin1Char('|'));
        if (sep > 0) {
            const QByteArray sid = m_currentSessionId.toUtf8();
            const QByteArray uri = appKey.mid(sep + 1).toUtf8();
            const QByteArray ext = appKey.left(sep).toUtf8();
            m_bridge->api().grouse_unstable_resources_read(m_bridge->handle(),
                sid.constData(), uri.constData(), ext.constData());
        }
    }
}

void Manager::unpinApp(const QString &slot)
{
    if (m_currentSessionId.isEmpty())
        return;
    const QString key = slot == QLatin1String("bottom") ? QStringLiteral("bottom")
                                                        : QStringLiteral("top");
    QVariantMap pins = pinnedApps().toMap();
    pins[key] = QString();
    QVariantMap all = m_settings.cardPinsFor(m_currentSessionId);
    all[m_currentSessionId] = pins;
    m_settings.setCardPinsFor(m_currentSessionId, all);
    emit pinnedAppsChanged();
}

void Manager::setPinRatio(const QString &slot, qreal ratio)
{
    if (m_currentSessionId.isEmpty())
        return;
    const QString key = slot == QLatin1String("bottom") ? QStringLiteral("bottomRatio")
                                                        : QStringLiteral("topRatio");
    QVariantMap pins = pinnedApps().toMap();
    pins[key] = qBound(0.15, ratio, 0.85);
    QVariantMap all = m_settings.cardPinsFor(m_currentSessionId);
    all[m_currentSessionId] = pins;
    m_settings.setCardPinsFor(m_currentSessionId, all);
    emit pinnedAppsChanged();
}

bool Manager::appPinned(const QString &appKey) const
{
    return !pinnedSlot(appKey).isEmpty();
}

QString Manager::pinnedSlot(const QString &appKey) const
{
    if (appKey.isEmpty())
        return QString();
    const QVariantMap pins = pinnedApps().toMap();
    if (pins.value(QStringLiteral("top")).toString() == appKey)
        return QStringLiteral("top");
    if (pins.value(QStringLiteral("bottom")).toString() == appKey)
        return QStringLiteral("bottom");
    return QString();
}

void Manager::onAppMessage(const QString &sessionId, const QString &text)
{
    // "post straight into this chat": re-open the app's chat first if the user
    // moved on (sendPrompt is queued until ready regardless, but the open makes
    // the message visible where it belongs).
    if (!sessionId.isEmpty() && sessionId != m_currentSessionId)
        openSession(sessionId);
    sendPrompt(text);
}
void Manager::onMessageUsage(const QVariantMap &usage)
{
    const QString label = formatUsage(usage);
    if (label.isEmpty())
        return;
    for (int i = m_messageModel->count() - 1; i >= 0; --i) {
        QVariantMap m = m_messageModel->row(i);
        if (m.value("role").toString() == "agent") {
            m["usage"] = label;
            m_messageModel->update(i, m);
            break;
        }
    }
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
    m_chat.onActiveRunChanged(sessionId, runId);
}
void Manager::onCompactionStatus(const QString &message)
{
    m_chat.onCompactionStatus(message);
}

void Manager::onUsage(int used, int size, double cost, const QString &currency)
{
    Q_UNUSED(cost);
    Q_UNUSED(currency);
    m_contextUsed = used;
    m_contextSize = size;
    emit contextChanged();
}

void Manager::onSessions(const QVariantList &sessions)
{
    m_sessions = sessions;
    m_sessionsModel->setSessions(sessions);
    emit sessionsChanged();
}

void Manager::onProjects(const QVariantList &projects)
{
    m_projects = projects;
    m_sessionsModel->setProjects(projects);
    emit projectsChanged();
    emit sessionsChanged();
}

void Manager::onRecipes(const QVariantList &recipes)
{
    m_catalog.onRecipes(recipes);
}
void Manager::onSchedules(const QVariantList &schedules)
{
    m_catalog.onSchedules(schedules);
}
void Manager::onConfig(const QVariantList &config)
{
    m_config = config;
    emit configChanged();
}

void Manager::onTools(const QVariantList &tools)
{
    m_catalog.onTools(tools);
}
QList<Manager::ExtDef> Manager::parseExtDefs(const QString &json)
{
    return m_catalog.parseExtDefs(json);
}
void Manager::onExtensions(const QVariantList &extensions)
{
    m_catalog.onExtensions(extensions);
}
void Manager::coreOnExtensionCatalog(const QString &sid, const QString &json)
{
    m_catalog.onExtensionCatalog(sid, json);
}

void Manager::onSessionExtensions(const QStringList &names, const QSet<QString> &restricted)
{
    m_catalog.onSessionExtensions(names, restricted);
}
void Manager::buildPeekQueue()
{
    m_catalog.buildPeekQueue();
}
void Manager::doPeekStep()
{
    m_catalog.doPeekStep();
}
void Manager::persistCatalogs()
{
    m_catalog.persistCatalogs();
}
void Manager::onPermission(const QString &toolCallId, const QString &title,
                           const QString &detail, const QVariantList &options)
{
    Q_UNUSED(detail);
    m_permToolCallId = toolCallId;
    m_permTitle = title;
    m_permOptions = options;
    emit permissionRequested();
    // The dialog is waiting behind whatever the user is looking at. The phone has
    // had this notification all along; the desktop silently blocked instead.
    if (notificationsEnabled() && !Notifier::appVisible())
        Notifier::send(QStringLiteral("Grouse needs approval"),
                       QStringLiteral("Allow “%1”?").arg(title));
}

QString Manager::lastAssistantText() const
{
    for (int row = m_messageModel->rowCount() - 1; row >= 0; --row) {
        const QModelIndex idx = m_messageModel->index(row, 0);
        if (idx.data(MessageListModel::RoleRole).toString() != QLatin1String("assistant"))
            continue;
        const QString text = idx.data(MessageListModel::TextRole).toString().trimmed();
        if (text.isEmpty())
            continue;
        return text.size() > 180 ? text.left(180) + QStringLiteral("…") : text;
    }
    return QStringLiteral("Turn finished.");
}

void Manager::onError(const QString &text, bool background)
{
    if (!background) {
        QVariantMap m{{"id", m_seq++}, {"role", "error"}, {"text", text},
                      {"html", QStringLiteral("<div>") + text + QStringLiteral("</div>")}};
        m_messageModel->append(m);
        emit messagesChanged();
    }
    setStatus(text);
}

void Manager::onSkills(const QVariantList &skills)
{
    m_catalog.onSkills(skills);
}
void Manager::onServerConfigValue(const QString &key, const QString &value)
{
    m_catalog.onServerConfigValue(key, value);
}
void Manager::onSupportedModels(const QString &providerId, const QStringList &models)
{
    m_catalog.onSupportedModels(providerId, models);
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

void Manager::onSessionTouched(const QString &sid, const QString &title, const QString &updatedAt)
{
    Q_UNUSED(sid); Q_UNUSED(title); Q_UNUSED(updatedAt);
    refreshSessions();
}

void Manager::onReady(const QString &sessionId)
{
    // The core drives readiness via on_status; this is a compatibility hook.
    Q_UNUSED(sessionId);
}

QVariant Manager::toolGroups() const
{
    return m_catalog.toolGroups();
}
