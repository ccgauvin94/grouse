// SPDX-License-Identifier: AGPL-3.0-or-later

#include "chatcontroller.h"
#include "manager.h"
#include "coreapi.h"
#include "settingsstore.h"
#include "messagelistmodel.h"
#include "roamlistmodel.h"

#include <QClipboard>
#include <QDateTime>
#include <QCryptographicHash>
#include <QDesktopServices>
#include <QDir>
#include <QFile>
#include <QFileDialog>
#include <QFileInfo>
#include <QGuiApplication>
#include <QImage>
#include <QJsonArray>
#include <QJsonDocument>
#include <QMetaObject>
#include <QMimeData>
#include <QMimeDatabase>
#include <QRunnable>
#include <QSaveFile>
#include <QStandardPaths>
#include <QThreadPool>
#include <QTimer>
#include <QUrl>

#include <utility>

namespace {
QJsonObject parseObj(const QString &json)
{
    return QJsonDocument::fromJson(json.toUtf8()).object();
}

// A plain QRunnable, not `QThreadPool::start(callable)` (Qt 6.7+ only): the flatpak
// SDK and the dev distrobox do not have to agree on a version for this to build.
class CoreTask : public QRunnable
{
public:
    explicit CoreTask(std::function<void()> fn) : m_fn(std::move(fn)) { setAutoDelete(true); }
    void run() override
    {
        if (m_fn)
            m_fn();
    }

private:
    std::function<void()> m_fn;
};
} // namespace

ChatController::ChatController(Manager &manager)
    : m_manager(manager)
    , m_bridge(manager.m_bridge)
    , m_roamModel(manager.m_roamModel)
    , m_messageModel(manager.m_messageModel)
    , m_sessions(manager.m_sessions)
    , m_roamTitles(manager.m_roamTitles)
    , m_tools(manager.m_tools)
    , m_status(manager.m_status)
    , m_online(manager.m_online)
    , m_prompting(manager.m_prompting)
    , m_roamRestored(manager.m_roamRestored)
    , m_activePeerLabel(manager.m_activePeerLabel)
    , m_promptingSessionId(manager.m_promptingSessionId)
    , m_inFlight(manager.m_inFlight)
    , m_activeRunIds(manager.m_activeRunIds)
    , m_activeRunId(manager.m_activeRunId)
    , m_compacting(manager.m_compacting)
    , m_compactionStatus(manager.m_compactionStatus)
    , m_currentSessionId(manager.m_currentSessionId)
    , m_currentSessionTitle(manager.m_currentSessionTitle)
    , m_landing(manager.m_landing)
    , m_lastCwd(manager.m_lastCwd)
    , m_wireOnlyConnect(manager.m_wireOnlyConnect)
    , m_testPending(manager.m_testPending)
    , m_pendingRecipeId(manager.m_pendingRecipeId)
    , m_pendingProjectFiling(manager.m_pendingProjectFiling)
    , m_openClock(manager.m_openClock)
    , m_openFor(manager.m_openFor)
    , m_io(manager.m_io)
    , m_pendingQueue(manager.m_pendingQueue)
    , m_sendSeq(manager.m_sendSeq)
    , m_pendingAttachments(manager.m_pendingAttachments)
{
}

ChatController::~ChatController() = default;

QString ChatController::host() const { return m_manager.host(); }
QString ChatController::port() const { return m_manager.port(); }
QString ChatController::secretKey() const { return m_manager.secretKey(); }
bool ChatController::useTls() const { return m_manager.useTls(); }
bool ChatController::autoConnectEnabled() const { return m_manager.autoConnectEnabled(); }
bool ChatController::roamEnabled() const { return m_manager.roamEnabled(); }
QString ChatController::workingDir() const { return m_manager.workingDir(); }
bool ChatController::mainWireOwnsTurnState(const QString &label) const { return Manager::mainWireOwnsTurnState(label); }
void ChatController::timingLog(const QString &line) { CoreApi::timingLog(line); }
QString ChatController::sessionPeerLabel(const QString &sid) const { return m_manager.sessionPeerLabel(sid); }
void ChatController::setStatus(const QString &s) { m_manager.setStatus(s); }
void ChatController::setOnline(bool o) { m_manager.setOnline(o); }
void ChatController::setMyDayVisible(bool v) { m_manager.setMyDayVisible(v); }
void ChatController::resetReloadProgress() { m_manager.resetReloadProgress(); }
void ChatController::resetContextUsage() { m_manager.resetContextUsage(); }
void ChatController::refreshSessions() { m_manager.refreshSessions(); }
void ChatController::refreshProjects() { m_manager.refreshProjects(); }
void ChatController::refreshRecipes() { m_manager.refreshRecipes(); }
void ChatController::refreshProviders() { m_manager.refreshProviders(); }
void ChatController::refreshToolGroups() { m_manager.refreshToolGroups(); }
void ChatController::syncRunSummariesToCore() { m_manager.syncRunSummariesToCore(); }
void ChatController::syncAdvisorToCore() { m_manager.syncAdvisorToCore(); }
void ChatController::syncRescaleToCore() { m_manager.syncRescaleToCore(); }
void ChatController::maybeShowCardPinRefusal(const QString &status, const QString &peer) { m_manager.maybeShowCardPinRefusal(status, peer); }
void ChatController::dismissRoamPinRefusal() { m_manager.dismissRoamPinRefusal(); }
void ChatController::onError(const QString &text, bool background) { m_manager.onError(text, background); }
void ChatController::notifyTurnFinished() { m_manager.notifyTurnFinished(); }
void ChatController::refreshActivity() { m_manager.refreshActivity(); }
void ChatController::finishTail(int lookback) { m_manager.finishTail(lookback); }
void ChatController::dismissAdvisorReport() { m_manager.dismissAdvisorReport(); }
void ChatController::loadToolCache(const QString &sid) { m_manager.loadToolCache(sid); }
void ChatController::publishToolGroups() { m_manager.publishToolGroups(); }
QString ChatController::serverConfigJson() { return CoreApi::serverConfigJson(m_manager); }
QString ChatController::promptJson(const QString &text, const QVariantList &blocks) const { return CoreApi::promptJson(text, blocks); }
QString ChatController::scopedKey(const QString &sid, const QString &key) const { return Manager::scopedKey(sid, key); }
QVariantMap ChatController::roamCards() const { return m_manager.m_settings.roamCards(); }
void ChatController::setRoamCards(const QVariantMap &cards) { m_manager.m_settings.setRoamCards(cards); }
QString ChatController::storedRoamIdentity() const { return m_manager.m_settings.roamIdentity(); }
QString ChatController::trimmedRoamIdentity() const { return m_manager.m_settings.trimmedRoamIdentity(); }
void ChatController::setRoamIdentity(const QString &secret) { m_manager.m_settings.setRoamIdentity(secret); }
QString ChatController::lastAttachDir() const { return m_manager.m_settings.lastAttachDir(); }
void ChatController::setLastAttachDir(const QString &dir) { m_manager.m_settings.setLastAttachDir(dir); }

void ChatController::runOnCore(std::function<void()> work)
{
    if (!m_io) {
        work();
        return;
    }
    m_io->start(new CoreTask(std::move(work)));
}

void ChatController::waitForCore()
{
    if (!m_io)
        return;
    m_io->waitForDone(5000);
    m_io->clear();
}

bool ChatController::mainWireConnected() const
{
    if (!m_bridge || !m_bridge->isAvailable())
        return false;
    const CoreBridge::AimsApi &api = m_bridge->api();
    if (api.grouse_connected)
        return api.grouse_connected(m_bridge->handle()) != 0;
    // A core without the attach ABI has only the bound-session notion; keep the
    // older library usable rather than declaring every chat offline.
    return api.grouse_ready && api.grouse_ready(m_bridge->handle()) != 0;
}



void ChatController::bindSessionAfterAttach()
{
    if (!m_bridge || !m_bridge->isAvailable() || m_currentSessionId.isEmpty())
        return;
    const CoreBridge::AimsApi api = m_bridge->api();
    void *h = m_bridge->handle();
    const QByteArray sid = m_currentSessionId.toUtf8();
    runOnCore([this, api, h, sid] {
        api.grouse_open_session(h, sid.constData());
        // A prompt parked while the wire was down must go out as soon as the
        // bind has run, not wait for an unrelated event to call flushQueue.
        QMetaObject::invokeMethod(&m_manager, [this] { flushQueue(); }, Qt::QueuedConnection);
    });
}

void ChatController::connectToServer()
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    if (secretKey().isEmpty()) {
        setStatus(QStringLiteral("no secret key"));
        return;
    }
    m_landing = false;
    emit m_manager.landingChanged();
    m_lastCwd = workingDir();
    // WIRE ONLY. This is the manual Connect, the startup auto-connect, the test
    // probe and the offline-send retry — none of them asks for a NEW CHAT. Using
    // `grouse_connect` here bound a throwaway `session/new` (littering the server
    // with empty chats) and, worse, could display that chat over a roam conversation
    // the main wire does not own. `connect_attach` brings the transport up and binds
    // nothing; the new-chat paths call `grouse_new_session` explicitly.
    // OFF THE UI THREAD: the attach still BLOCKS on the handshake, as `connect` did
    // (2026-11-21: 3.2s + 0.5s of stalls at startup).
    const CoreBridge::AimsApi api = m_bridge->api();
    void *h = m_bridge->handle();
    const QByteArray cfg = serverConfigJson().toUtf8();
    m_wireOnlyConnect = true;
    setStatus(QStringLiteral("connecting…"));
    runOnCore([this, api, h, cfg] {
        QElapsedTimer clock;
        clock.start();
        char *err = nullptr;
        if (api.grouse_connect_attach)
            api.grouse_connect_attach(h, cfg.constData(), &err);
        else
            api.grouse_connect(h, cfg.constData(), &err);  // older core: no attach ABI
        const qint64 ms = clock.elapsed();
        QString message;
        if (err) {
            message = QString::fromUtf8(err);
            api.grouse_string_free(err);
        }
        timingLog(QStringLiteral("connect %1ms%2")
                      .arg(ms)
                      .arg(message.isEmpty() ? QString() : QStringLiteral(" FAILED: ") + message));
        if (!message.isEmpty()) {
            QMetaObject::invokeMethod(&m_manager, [this, message] {
                setStatus(QStringLiteral("connect failed: ") + message);
            }, Qt::QueuedConnection);
        }
    });
}

void ChatController::autoConnect()
{
    if (!autoConnectEnabled())
        return;
    if (host().trimmed().isEmpty() || secretKey().isEmpty()) {
        setStatus(QStringLiteral("not configured — press Connect"));
        return;
    }
    m_landing = true;
    connectToServer();
    // A launch OPENS NOTHING: it connects, then stays on the landing page until a chat is
    // picked — the same rule the phone follows. `connectToServer` clears the flag (it is the
    // manual "connect and go to the chat" path), so re-assert it here rather than making the
    // two callers disagree about what pressing Connect means.
    m_landing = true;
    emit m_manager.landingChanged();
}

void ChatController::disconnect()
{
    if (m_bridge && m_bridge->isAvailable())
        m_bridge->api().grouse_disconnect(m_bridge->handle());
    m_wireOnlyConnect = false;
    setOnline(false);
    m_prompting = false;
    m_promptingSessionId.clear();
    m_inFlight.clear();
    m_activeRunIds.clear();
    emit m_manager.promptingChanged();
    m_compacting = false;
    emit m_manager.compactingChanged();
    m_landing = true;
    emit m_manager.landingChanged();
    setStatus(QStringLiteral("disconnected"));
}

void ChatController::connectRoam(const QString &card, const QString &label)
{
    syncRoamIdentityToCore();
    if (m_bridge && m_bridge->isAvailable()) {
        const QByteArray c = card.toUtf8();
        const QByteArray l = label.toUtf8();
        m_bridge->api().grouse_roam_connect(m_bridge->handle(), c.constData(), l.constData());
        m_roamModel->addPeer(label);
        persistRoamCard(label, card);
    }
}

/// Push the identity this app advertises (QSettings) into the core so the wire
/// dials with the SAME key the host was asked to accept. The core otherwise
/// keeps a separately-generated secret and every dial lands "not_allowlisted".

void ChatController::syncRoamIdentityToCore()
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QString secret = trimmedRoamIdentity();
    if (secret.isEmpty())
        return;
    const QByteArray s = secret.toUtf8();
    m_bridge->api().grouse_set_roam_identity(m_bridge->handle(), s.constData());
}

void ChatController::disconnectRoam(const QString &label)
{
    if (m_activePeerLabel == label)
        m_activePeerLabel.clear();
    if (m_bridge && m_bridge->isAvailable()) {
        const QByteArray l = label.toUtf8();
        m_bridge->api().grouse_roam_disconnect(m_bridge->handle(), l.constData());
    }
    m_roamModel->removePeer(label);
    forgetRoamCard(label);
}

void ChatController::persistRoamCard(const QString &label, const QString &card)
{
    QVariantMap cards = roamCards();
    cards.insert(label, card);
    setRoamCards(cards);
}

void ChatController::forgetRoamCard(const QString &label)
{
    QVariantMap cards = roamCards();
    if (cards.remove(label))
        setRoamCards(cards);
}

/// Re-dial + re-list every persisted roam peer so stored connections survive a
/// restart (the wires are the core's; this only re-arms them).

void ChatController::restoreRoamPeers()
{
    const QVariantMap cards = roamCards();
    if (cards.isEmpty())
        return;
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    for (auto it = cards.constBegin(); it != cards.constEnd(); ++it) {
        const QString card = it.value().toString();
        if (card.isEmpty())
            continue;
        m_roamModel->addPeer(it.key());
        const QByteArray c = card.toUtf8();
        const QByteArray l = it.key().toUtf8();
        m_bridge->api().grouse_roam_connect(m_bridge->handle(), c.constData(), l.constData());
    }
}

QVariantList ChatController::roamCardPins()
{
    QVariantMap pins;
    if (m_bridge && m_bridge->isAvailable()) {
        const QString all = CoreApi::pinGetter(m_bridge, m_bridge->api().grouse_roam_card_pins,
                                      m_bridge->handle(), nullptr);
        pins = CoreApi::pinMapFromJson(all);
        if (pins.isEmpty()) {
            // Older ABI-20 spellings expose only the singular getter: ask it for
            // every locally known peer. The PINS themselves still come from the
            // core, never from the saved card (which may be the wrong one).
            const QVariantMap cards = roamCards();
            for (auto it = cards.constBegin(); it != cards.constEnd(); ++it) {
                const QByteArray label = it.key().toUtf8();
                const QString fp = CoreApi::pinStringFromJson(CoreApi::pinGetter(
                    m_bridge, m_bridge->api().grouse_roam_card_pin,
                    m_bridge->handle(), &label));
                if (!fp.isEmpty())
                    pins.insert(it.key(), fp);
            }
        }
    }
    return CoreApi::formatCardPins(pins);
}

bool ChatController::clearRoamCardPin(const QString &peer, const QString &confirmation)
{
    const QString label = peer.trimmed();
    if (label.isEmpty() || confirmation.trimmed() != label)
        return false;               // out-of-band confirmation is a hard gate
    if (!m_bridge || !m_bridge->isAvailable())
        return false;
    const QByteArray l = label.toUtf8();
    QString error;
    if (!CoreApi::pinClearIntent(m_bridge, m_bridge->api().grouse_roam_card_pin_clear,
                        m_bridge->handle(), l, &error)) {
        setStatus(error.isEmpty()
                  ? QStringLiteral("Could not clear the pin for %1").arg(label)
                  : error);
        return false;
    }
    emit m_manager.roamCardPinsChanged();
    dismissRoamPinRefusal();
    setStatus(QStringLiteral("Pin cleared for %1 — the next dial re-enrolls this peer; "
                             "verify its card out of band first.").arg(label));
    return true;
}

bool ChatController::redialRoamPeer(const QString &label)
{
    const QString card = roamCards()
                             .value(label.trimmed()).toString();
    if (card.isEmpty())
        return false;
    connectRoam(card, label.trimmed());
    return true;
}

void ChatController::testConnection()
{
    if (!m_bridge || !m_bridge->isAvailable()) {
        emit m_manager.connectionTested(false, QStringLiteral("grouse-core not loaded."));
        return;
    }
    if (secretKey().isEmpty()) {
        emit m_manager.connectionTested(false, QStringLiteral("No secret key set — fill it in above."));
        return;
    }
    // Drive a real connect; the resulting on_status (Ready/Error) resolves the
    // probe via coreOnStatus. The core owns the connection.
    m_testPending = true;
    connectToServer();
}

void ChatController::openRoamSession(const QString &label, const QString &sessionId, const QString &cwd)
{
    setMyDayVisible(false);   // opening a conversation takes the chat area back from My Day
    resetReloadProgress();    // a reload bar belongs to the chat that started it
    m_activePeerLabel = label;
    // A parked message belongs to the chat it was typed in, so opening another chat must
    // not discard it — the same per-chat queue Android keeps. Only Stop drops one.
    m_landing = false;
    emit m_manager.landingChanged();
    resetContextUsage();
    m_currentSessionId = sessionId;
    restoreTurnState(sessionId);
    emit m_manager.queuedChanged();   // the queue view is per-chat: republish for the chat we opened
    // A peer's chat carries its human title only in the peer's session list, so
    // the window shows that instead of the transport handle it opened by.
    m_currentSessionTitle = m_roamTitles.value(sessionId);
    m_tools.clear();
    m_manager.m_extDefs.clear();
    m_manager.m_catalogDefs.clear();
    m_manager.m_sessionExts.clear();
    m_manager.m_sessionRestricted.clear();
    m_manager.m_peekQueue.clear();
    m_manager.m_peekFailed.clear();
    loadToolCache(sessionId);   // best-effort memory of last run's catalogues
    publishToolGroups();
    emit m_manager.currentSessionChanged();
    emit m_manager.pinnedAppsChanged();
    emit m_manager.toolsChanged();
    setStatus(QStringLiteral("loading…"));
    m_lastCwd = cwd.isEmpty() ? workingDir() : cwd;
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    // OFF THE UI THREAD, exactly like the local open: a peer open asks the peer, and
    // the peer's cached transcript is read and parsed on this side. This path was
    // missed when the local one moved (2026-11-20) — which is why a ROAM chat was the
    // one that still froze the window, and the one that logged nothing.
    m_openClock.start();
    m_openFor = sessionId;
    timingLog(QStringLiteral("roam open start %1 on %2").arg(sessionId, label));
    const CoreBridge::AimsApi api = m_bridge->api();
    void *h = m_bridge->handle();
    const QByteArray l = label.toUtf8();
    const QByteArray sid = sessionId.toUtf8();
    runOnCore([api, h, l, sid] {
        QElapsedTimer clock;
        clock.start();
        api.grouse_roam_open_session(h, l.constData(), sid.constData());
        CoreApi::timingLog(QStringLiteral("roam open %1 worker %2ms")
                               .arg(QString::fromUtf8(sid)).arg(clock.elapsed()));
    });
}

void ChatController::newRoamSession(const QString &label)
{
    m_activePeerLabel = label;
    emit m_manager.queuedChanged();
    m_landing = false;
    emit m_manager.landingChanged();
    resetContextUsage();
    m_currentSessionId.clear();
    m_currentSessionTitle.clear();
    m_tools.clear();
    m_manager.m_extDefs.clear();
    m_manager.m_catalogDefs.clear();
    m_manager.m_sessionExts.clear();
    m_manager.m_sessionRestricted.clear();
    m_manager.m_peekQueue.clear();
    m_manager.m_peekFailed.clear();
    publishToolGroups();
    m_messageModel->clear();
    m_manager.m_currentIndex = -1;
    emit m_manager.currentSessionChanged();
    emit m_manager.pinnedAppsChanged();
    emit m_manager.toolsChanged();
    emit m_manager.messagesChanged();
    setStatus(QStringLiteral("connecting…"));
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    // `session/new` on the peer is a handshake: same worker, same reason.
    const CoreBridge::AimsApi api = m_bridge->api();
    void *h = m_bridge->handle();
    const QByteArray l = label.toUtf8();
    runOnCore([api, h, l] { api.grouse_roam_new_session(h, l.constData()); });
}

void ChatController::newRoamSessionWithMode(const QString &label, const QString &cwd,
                                     const QString &mode)
{
    m_activePeerLabel = label;
    emit m_manager.queuedChanged();
    m_landing = false;
    emit m_manager.landingChanged();
    resetContextUsage();
    m_currentSessionId.clear();
    m_currentSessionTitle.clear();
    m_tools.clear();
    m_manager.m_extDefs.clear();
    m_manager.m_catalogDefs.clear();
    m_manager.m_sessionExts.clear();
    m_manager.m_sessionRestricted.clear();
    m_manager.m_peekQueue.clear();
    m_manager.m_peekFailed.clear();
    publishToolGroups();
    m_messageModel->clear();
    m_manager.m_currentIndex = -1;
    emit m_manager.currentSessionChanged();
    emit m_manager.pinnedAppsChanged();
    emit m_manager.toolsChanged();
    emit m_manager.messagesChanged();
    setStatus(QStringLiteral("connecting…"));
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const CoreBridge::AimsApi api = m_bridge->api();
    void *h = m_bridge->handle();
    const QByteArray l = label.toUtf8();
    const QString t = cwd.trimmed();
    const bool hasCwd = !t.isEmpty();
    const QByteArray c = t.toUtf8();
    const QByteArray md = mode.toUtf8();
    const bool plain = mode.isEmpty() || !api.grouse_roam_new_session_with_mode;
    runOnCore([api, h, l, c, md, hasCwd, plain] {
        if (plain) {
            api.grouse_roam_new_session_cwd(h, l.constData(), hasCwd ? c.constData() : nullptr);
        } else {
            api.grouse_roam_new_session_with_mode(h, l.constData(),
                                                  hasCwd ? c.constData() : nullptr,
                                                  md.constData());
        }
    });
}

void ChatController::newRoamSessionIn(const QString &label, const QString &cwd)
{
    newRoamSessionWithMode(label, cwd, QString());
}

/// A roam peer's new chat in a mode. The tag is written by THAT peer's goose, so a
/// roam chat carries a mode exactly like a local one.

void ChatController::setActiveTab(const QString &tab)
{
    if (tab != QLatin1String("main") || m_activePeerLabel.isEmpty())
        return;
    // A roam chat owns the ACTIVE SESSION, not the sidebar's grouping. Clearing the
    // label is what stops later local lists from inheriting it (coreOnSessions no
    // longer stamps them at all), and the re-list is what repairs rows already on
    // screen: a list fetched while the peer was set stays grouped under it until the
    // next refresh, which is exactly the "Projects vanished after visiting Roam"
    // report. One round trip on the switch back, not on every tab click.
    m_activePeerLabel.clear();
    refreshSessions();
}

QString ChatController::roamIdentity()
{
    QString secret = storedRoamIdentity();
    if (secret.isEmpty() && m_bridge && m_bridge->isAvailable()) {
        secret = m_bridge->takeString(m_bridge->api().grouse_identity_generate());
        if (!secret.isEmpty())
            setRoamIdentity(secret);
    }
    return secret;
}

QString ChatController::roamPublicKey() const
{
    const QString secret = storedRoamIdentity();
    if (secret.isEmpty() || !m_bridge || !m_bridge->isAvailable())
        return QString();
    char *err = nullptr;
    const QByteArray s = secret.toUtf8();
    QString key = m_bridge->takeString(
        m_bridge->api().grouse_identity_public_key(s.constData(), &err));
    if (err) {
        m_bridge->api().grouse_string_free(err);
        return QString();
    }
    return key;
}

QString ChatController::roamCard() const
{
    // Matches the Android client's card exactly: base64url (no padding) of
    // {"version":1,"endpoint_id":"<hex key>","relay_urls":[]}. The card decoder
    // accepts empty relay URLs (LAN-direct); the host reaches this device by its
    // public endpoint_id, so only the key goes in.
    const QString key = roamPublicKey();
    if (key.isEmpty())
        return QString();
    const QByteArray json =
        QStringLiteral("{\"version\":1,\"endpoint_id\":\"%1\",\"relay_urls\":[]}")
            .arg(key)
            .toUtf8();
    const QByteArray b64 = json.toBase64(
        QByteArray::Base64UrlEncoding | QByteArray::OmitTrailingEquals);
    return QStringLiteral("goose+roam://") + QString::fromLatin1(b64);
}

void ChatController::openSession(const QString &sessionId)
{
    const QString previous = m_currentSessionId;
    setMyDayVisible(false);   // opening a conversation takes the chat area back from My Day
    resetReloadProgress();    // a reload bar belongs to the chat that started it
    // Ask the CORE which wire owns this display id (grouse_session_owner), not the
    // `roam:` text: the core is the one routing the open. Keep the label in sync
    // with that answer — otherwise the core routes to the peer while this client
    // still thinks the main wire owns the turn, and a main-wire Ready clears a
    // live roam turn. The namespace is only the fallback when the core cannot answer.
    m_activePeerLabel = sessionPeerLabel(sessionId);
    // Parking is per chat: a message queued in the chat we are leaving keeps waiting for
    // ITS turn to end (flushQueue only drains the on-screen chat's), so a switch cannot
    // silently lose input. Stop is what discards it.
    m_landing = false;
    emit m_manager.landingChanged();
    resetContextUsage();
    m_currentSessionId = sessionId;
    restoreTurnState(sessionId);
    emit m_manager.queuedChanged();   // the queue view is per-chat: republish for the chat we opened
    m_currentSessionTitle.clear();
    if (!m_activePeerLabel.isEmpty()) {
        // A peer's title lives only in its own session/list, not in the main list.
        m_currentSessionTitle = m_roamTitles.value(sessionId);
    }
    if (m_currentSessionTitle.isEmpty()) {
        for (const auto &v : m_sessions) {
            const QVariantMap s = v.toMap();
            if (s.value("sessionId").toString() == sessionId) {
                m_currentSessionTitle = s.value("title").toString();
                break;
            }
        }
    }
    m_tools.clear();
    m_manager.m_extDefs.clear();
    m_manager.m_catalogDefs.clear();
    m_manager.m_sessionExts.clear();
    m_manager.m_sessionRestricted.clear();
    m_manager.m_peekQueue.clear();
    m_manager.m_peekFailed.clear();
    loadToolCache(sessionId);
    publishToolGroups();
    m_messageModel->clear();
    m_manager.m_currentIndex = -1;
    emit m_manager.currentSessionChanged();
    emit m_manager.pinnedAppsChanged();
    emit m_manager.toolsChanged();
    emit m_manager.messagesChanged();
    setStatus(QStringLiteral("loading…"));
    m_lastCwd = workingDir();
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    m_openClock.start();
    m_openFor = sessionId;
    timingLog(QStringLiteral("open start %1 (was %2)").arg(sessionId, previous));
    // OFF THE UI THREAD. Everything here can block: reading and parsing the cached
    // transcript, and — for a session `session/list` has never returned, i.e. a
    // thread chat — a `session/info` round trip just to learn its cwd. Both ran on
    // the main thread, which is why a chat switch froze the window before the new
    // chat appeared. The core is built for this (Android has always called it from
    // a background executor) and CoreBridge marshals every event it emits back onto
    // the main thread, so nothing below touches UI state off-main.
    const CoreBridge::AimsApi api = m_bridge->api();
    void *h = m_bridge->handle();
    const QByteArray sid = sessionId.toUtf8();
    runOnCore([api, h, sid] {
        QElapsedTimer clock;
        clock.start();
        api.grouse_open_session(h, sid.constData());
        // NO grouse_load_cached_transcript here. `open_session` has already painted
        // the core's live store — including a BIND-ONLY load for a chat with a turn
        // in flight — with the correct freshness/stale-tail rules. A second cache
        // load replaces that with stale disk rows: missing the in-flight reply and
        // discarding a tail the wire just merged (2026-11-22).
        // Reported HERE, not handed to the Window: the core emits its paint DURING
        // the call, so a value posted after it reaches the main thread too late and
        // the paint line read zeros (measured: "worker 0ms: open 0, cache 0").
        CoreApi::timingLog(QStringLiteral("open %1 worker %2ms")
                               .arg(QString::fromUtf8(sid)).arg(clock.elapsed()));
    });
}

QString ChatController::sessionStamp(const QString &sessionId) const
{
    for (const auto &v : m_sessions) {
        const QVariantMap row = v.toMap();
        if (row.value("sessionId").toString() != sessionId)
            continue;
        const QString stamp = row.value("lastMessageAt").toString();
        return stamp.isEmpty() ? row.value("updatedAt").toString() : stamp;
    }
    return {};
}

void ChatController::refreshSession()
{
    // QUICK (grouse#18): freshness decides — a current cache suppresses the
    // replay, a stale one merges a bounded tail, an incomplete store still
    // replays in full. Either way it is ingested silently and painted once, so
    // the chat lands on its newest messages instead of re-rendering from the
    // top.
    refreshSessionWith(0);
}

void ChatController::reloadSessionFromStart()
{
    // FULL: throw the cached transcript away and rebuild the chat from the
    // server — the escape hatch for a view known to have drifted, and the only
    // path that is allowed to ignore what is on disk. It is the slower, heavier
    // of the two, so it stays behind an explicit gesture.
    //
    // ONE at a time: a second press while the replay is in flight would clear
    // the transcript again mid-rebuild and restart the bar. The core refuses the
    // repeat as well; refusing here keeps the button honest.
    if (m_manager.m_reloading)
        return;
    setStatus(QStringLiteral("reloading from the start…"));
    refreshSessionWith(1);
}

void ChatController::refreshSessionWith(int full)
{
    if (!m_bridge || !m_bridge->isAvailable() || m_currentSessionId.isEmpty())
        return;
    if (full == 0)
        setStatus(QStringLiteral("reloading…"));
    const QByteArray sid = m_currentSessionId.toUtf8();
    m_bridge->api().grouse_refresh_session(m_bridge->handle(), sid.constData(), full);
}

void ChatController::newChat()
{
    newChatInMode(QString());
}

/// A new chat in a mode (`grouse.mode`: `dev`). Empty = an ordinary chat.
///
/// The mode rides `session/new` as a tag: the server stores it, filters on it and
/// never returns it, so a mode can only be chosen when the chat is made. That is
/// why creation — not a control in the chat — is where the choice lives.

void ChatController::newChatInMode(const QString &mode)
{
    setMyDayVisible(false);   // a new conversation also takes the chat area back
    resetReloadProgress();
    m_activePeerLabel.clear();
    emit m_manager.queuedChanged();
    m_landing = false;
    emit m_manager.landingChanged();
    resetContextUsage();
    m_currentSessionId.clear();
    m_currentSessionTitle.clear();
    m_tools.clear();
    m_manager.m_extDefs.clear();
    m_manager.m_catalogDefs.clear();
    m_manager.m_sessionExts.clear();
    m_manager.m_sessionRestricted.clear();
    m_manager.m_peekQueue.clear();
    m_manager.m_peekFailed.clear();
    m_messageModel->clear();
    m_manager.m_currentIndex = -1;
    publishToolGroups();
    emit m_manager.currentSessionChanged();
    emit m_manager.pinnedAppsChanged();
    emit m_manager.toolsChanged();
    emit m_manager.messagesChanged();
    // The composer is a surviving QML object, so a draft typed for the previous
    // conversation would otherwise follow the user into the new one (and, in a
    // thread chat, into a prefill it does not belong to).
    emit m_manager.draftRequested(QString());
    setStatus(QStringLiteral("connecting…"));
    // The recipe id and project id ride session/new directly. `grouse_new_session`
    // already took a recipe id; passing it here (rather than letting
    // serverConfigJson consume it) is what actually starts a recipe run.
    const QString recipeId = m_pendingRecipeId;
    m_pendingRecipeId.clear();
    const QString projectId = m_pendingProjectFiling;
    m_pendingProjectFiling.clear();
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    // Also off the UI thread: `session/new` is a handshake, and it blocked the
    // window for its duration ("New chat" sat on the old screen until the server
    // answered). The window has already moved to the new chat here — the id arrives
    // through the usual events.
    const CoreBridge::AimsApi api = m_bridge->api();
    void *h = m_bridge->handle();
    const QByteArray rid = recipeId.toUtf8();
    const QByteArray pid = projectId.toUtf8();
    const QByteArray md = mode.toUtf8();
    const bool plain = mode.isEmpty() || !api.grouse_new_session_with_mode;
    const bool hasRecipe = !recipeId.isEmpty();
    const bool hasProject = !projectId.isEmpty();
    runOnCore([this, api, h, rid, pid, md, plain, hasRecipe, hasProject] {
        char *err = nullptr;
        if (plain) {
            api.grouse_new_session(h, hasRecipe ? rid.constData() : nullptr,
                                   hasProject ? pid.constData() : nullptr, &err);
        } else {
            api.grouse_new_session_with_mode(h, hasRecipe ? rid.constData() : nullptr,
                                             hasProject ? pid.constData() : nullptr,
                                             md.constData(), &err);
        }
        if (!err)
            return;
        const QString message = QString::fromUtf8(err);
        api.grouse_string_free(err);
        QMetaObject::invokeMethod(&m_manager, [this, message] {
            setStatus(QStringLiteral("new session failed: ") + message);
        }, Qt::QueuedConnection);
    });
}

void ChatController::beginChat()
{
    if (!m_landing)
        return;
    m_landing = false;
    emit m_manager.landingChanged();
}

void ChatController::sendPrompt(const QString &text, const QVariantList &images)
{
    if (!m_bridge || !m_bridge->isAvailable() || (text.trimmed().isEmpty() && images.isEmpty()))
        return;
    m_landing = false;
    emit m_manager.landingChanged();
    // A review is about the turn that is over: starting the next one retires it.
    dismissAdvisorReport();

    const QString expectJson = m_currentSessionId.isEmpty()
        ? QString() : QStringLiteral("{\"session_id\":\"%1\"}").arg(m_currentSessionId);

    // Images still ride inline (goose renders those); everything else is a
    // PATH the core uploads, because goose DROPS a `resource` block whose
    // contents are a base64 blob -- which is what an attached PDF used to be.
    dispatchSend(text.trimmed(), buildAttachmentBlocks(images),
                 attachmentPaths(images));
}

void ChatController::dispatchSend(const QString &text, const QVariantList &blocks,
                           const QStringList &files)
{
    const QString expectJson = m_currentSessionId.isEmpty()
        ? QString() : QStringLiteral("{\"session_id\":\"%1\"}").arg(m_currentSessionId);
    // Two questions, not one: is the chat's OWN transport up (main wire or the
    // peer), and — on the main wire — is a session bound to send into? After a
    // wire-only attach `connected` is true while `ready` is false; using ready as
    // the transport gate parked every offline send forever (and asking only
    // `connected` would send into a main chat the core has not bound yet).
    const bool wireUp = wireUpForCurrentChat();
    const bool sessionBound = !m_activePeerLabel.isEmpty()
        || (m_bridge && m_bridge->isAvailable() && m_bridge->api().grouse_ready
            && m_bridge->api().grouse_ready(m_bridge->handle()));
    const bool ready = wireUp && sessionBound;
    // The core echoes a PURE-TEXT prompt into the transcript itself: the live wire
    // carries NO user_message_chunk for our own send, so the core keeps its own copy
    // (otherwise the question is never recorded, and a reopen that trusts the cache
    // shows the replies with the questions missing — see grouse-core's
    // `spawn_prompt`). Attachments are the exception: the item model has no image
    // field, so the client keeps its own row for those — which is also what renders
    // the thumbnails (a replayed session reconstructs text only).
    QVariantList shown;
    for (const auto &v : blocks) {
        const QVariantMap b = v.toMap();
        if (b.value("type").toString() == QLatin1String("image"))
            shown << QVariantMap{{"image", true},
                                 {"url", QStringLiteral("data:%1;base64,%2")
                                            .arg(b.value("mimeType").toString(),
                                                 b.value("data").toString())}};
        else if (b.value("type").toString() == QLatin1String("resource"))
            shown << QVariantMap{{"image", false},
                                 {"url", b.value("resource").toMap().value("uri").toString()},
                                 {"name", QFileInfo(b.value("resource").toMap()
                                                    .value("uri").toString()).fileName()}};
    }
    // A file by PATH: this is only the fallback (an older core library that
    // reports a count but no records) — the row's cards normally come from the
    // core's own `files`, which is what a replayed transcript still has.
    for (const QString &path : files) {
        const QFileInfo info(path);
        shown << QVariantMap{{"image", false},
                             {"url", path},
                             {"name", info.fileName()},
                             {"mime", QMimeDatabase().mimeTypeForFile(path).name()},
                             {"bytes", info.size()}};
    }
    const bool sameChatBusy = m_prompting && m_promptingSessionId == m_currentSessionId;
    if (ready && !sameChatBusy) {
        // Thumbnails are matched back onto the row the core echoes, so they are queued
        // only at the moment the prompt actually goes out — queueing them earlier put a
        // second copy in the map every time a parked send was drained. `files` counts
        // too: a prompt with ONLY a non-image attachment produces no blocks, and the
        // blocks-only guard is why an attached PDF showed up in the transcript as
        // nothing at all on this client.
        if ((!blocks.isEmpty() || !files.isEmpty()) && !shown.isEmpty())
            m_pendingAttachments[scopedKey(m_currentSessionId, text)].append(shown);
        m_prompting = true;
        m_promptingSessionId = m_currentSessionId;
        if (!m_currentSessionId.isEmpty())
            m_inFlight.insert(m_currentSessionId);
        emit m_manager.promptingChanged();
        char *err = nullptr;
        const QByteArray prompt = promptJson(text, blocks).toUtf8();
        const QByteArray expect = expectJson.toUtf8();
        if (files.isEmpty()) {
            m_bridge->api().grouse_send_prompt(
                m_bridge->handle(), prompt.constData(),
                expectJson.isEmpty() ? nullptr : expect.constData(), &err);
        } else if (m_bridge->api().grouse_send_prompt_with_paths) {
            QJsonArray arr;
            for (const QString &path : files)
                arr.append(path);
            const QByteArray paths =
                QJsonDocument(arr).toJson(QJsonDocument::Compact);
            m_bridge->api().grouse_send_prompt_with_paths(
                m_bridge->handle(), prompt.constData(), paths.constData(),
                expectJson.isEmpty() ? nullptr : expect.constData(), &err);
        } else {
            // An older libgrouse_core: say so rather than silently dropping the
            // attachment, which is the bug this whole path exists to remove.
            m_bridge->api().grouse_send_prompt(
                m_bridge->handle(), prompt.constData(),
                expectJson.isEmpty() ? nullptr : expect.constData(), &err);
            if (!err)
                onError(QStringLiteral("this core cannot attach files "
                                       "(grouse_send_prompt_with_paths missing)"), false);
        }
        if (err) {
            onError(QString::fromUtf8(err), false);
            m_bridge->api().grouse_string_free(err);
        }
    } else {
        // A turn is running on THIS chat (or there is no session bound yet): the message
        // is PARKED, never steered. A steer REDIRECTS the reply being written, so it has
        // to be the user's explicit choice — the Steer button on the queued card
        // (steerQueued) — and not a side effect of pressing send while a reply streams
        // (Android parity). flushQueue sends it when the turn ends.
        enqueue({text, blocks, files, m_currentSessionId, 0});
        if (!ready && !secretKey().isEmpty())
            connectToServer();
    }
}

bool ChatController::wireUpForCurrentChat() const
{
    if (!m_activePeerLabel.isEmpty())
        return m_roamModel && m_roamModel->peerConnected(m_activePeerLabel);
    // `online` is a cached status event; the core's `connected` is the transport
    // truth and stays true on an attach that bound no session. The transport gate
    // must not be `grouse_ready` here — that would be false on exactly that wire.
    return mainWireConnected();
}

void ChatController::releaseTurnForLostWire(const QString &lostSessionId)
{
    // Only the wire that OWNS the turn may release it: a drop in another chat, or on
    // the main socket while a peer owns the turn, must leave it alone (Android parity).
    if (!turnOwnerMatches(m_promptingSessionId, m_currentSessionId, lostSessionId))
        return;
    m_inFlight.remove(lostSessionId);
    m_activeRunIds.remove(lostSessionId);
    // A dead host emits neither RunEnded nor a cleared run id, so without this
    // m_prompting stays true forever: "N queued — will send when this turn finishes"
    // is a promise that can never be kept and the queue never drains.
    m_promptingSessionId.clear();
    if (m_prompting) {
        m_prompting = false;
        emit m_manager.promptingChanged();
    }
    if (m_compacting) {
        m_compacting = false;
        emit m_manager.compactingChanged();
    }
    setActiveRunId(QString());
    // The queue itself survives (the core flushes on the next Ready); releasing the
    // latch is what lets it move at all.
    flushQueue();
}

void ChatController::setActiveRunId(const QString &runId)
{
    if (m_activeRunId == runId)
        return;
    m_activeRunId = runId;
    emit m_manager.activeRunIdChanged();
}

void ChatController::enqueue(const PendingSend &p)
{
    PendingSend q = p;
    q.id = ++m_sendSeq;
    q.sessionId = m_currentSessionId;   // the chat it was TYPED in
    m_pendingQueue << q;
    emit m_manager.queuedChanged();
}

int ChatController::queuedCount() const
{
    int n = 0;
    for (const PendingSend &p : m_pendingQueue) {
        if (p.sessionId.isEmpty() || p.sessionId == m_currentSessionId)
            ++n;
    }
    return n;
}

QVariant ChatController::queuedSends() const
{
    QVariantList out;
    for (const PendingSend &p : m_pendingQueue) {
        // An empty sessionId is a send typed before any chat was bound; it belongs to
        // whichever chat is on screen (flushQueue adopts it), so show it here.
        if (!p.sessionId.isEmpty() && p.sessionId != m_currentSessionId)
            continue;
        out << QVariantMap{{"id", p.id},
                           {"text", p.text},
                           {"attachments", p.images.size()}};
    }
    return out;
}

void ChatController::steerQueued(int id)
{
    if (!m_bridge || !m_bridge->isAvailable() || m_activeRunId.isEmpty())
        return;
    for (int i = 0; i < m_pendingQueue.size(); ++i) {
        const PendingSend &p = m_pendingQueue.at(i);
        if (p.id != id || (!p.sessionId.isEmpty() && p.sessionId != m_currentSessionId))
            continue;
        // A steer carries text only. Sending the text and dropping the images would lose
        // input silently, so an attachment-carrying message keeps waiting instead.
        if (p.text.trimmed().isEmpty() || !p.images.isEmpty())
            return;
        const QString text = p.text;
        m_pendingQueue.removeAt(i);
        emit m_manager.queuedChanged();
        m_prompting = true;
        m_promptingSessionId = m_currentSessionId;
        if (!m_currentSessionId.isEmpty())
            m_inFlight.insert(m_currentSessionId);
        emit m_manager.promptingChanged();
        const QByteArray t = text.toUtf8();
        const QByteArray r = m_activeRunId.toUtf8();
        m_bridge->api().grouse_unstable_steer(m_bridge->handle(), t.constData(), r.constData());
        return;
    }
}

void ChatController::cancelQueued(int id)
{
    for (int i = 0; i < m_pendingQueue.size(); ++i) {
        if (m_pendingQueue.at(i).id != id)
            continue;
        m_pendingQueue.removeAt(i);
        emit m_manager.queuedChanged();
        return;
    }
}

void ChatController::restoreTurnState(const QString &sessionId)
{
    const bool busy = m_inFlight.contains(sessionId);
    m_prompting = busy;
    m_promptingSessionId = busy ? sessionId : QString();
    m_compacting = false;
    setActiveRunId(busy ? m_activeRunIds.value(sessionId) : QString());
    emit m_manager.promptingChanged();
    emit m_manager.compactingChanged();
}

void ChatController::flushQueue()
{
    if (m_pendingQueue.isEmpty()
        || (m_prompting && m_promptingSessionId == m_currentSessionId))
        return;
    // Drain only a message meant for the chat ON SCREEN. Taking the head regardless sent a
    // prompt parked in chat A into chat B after a switch (Android fixed this the same way);
    // an entry with no session yet adopts this one, which is where it goes anyway.
    int idx = -1;
    for (int i = 0; i < m_pendingQueue.size(); ++i) {
        const QString &sid = m_pendingQueue.at(i).sessionId;
        if (sid.isEmpty() || sid == m_currentSessionId) {
            idx = i;
            break;
        }
    }
    if (idx < 0)
        return;
    const PendingSend p = m_pendingQueue.takeAt(idx);
    emit m_manager.queuedChanged();
    dispatchSend(p.text, p.images, p.files);
}

namespace {

/// A prompt's image reaches the row as a `data:` URI: the core hands the client the
/// PIXELS, not a path, so the row can show what was sent without the file still being
/// there. QML's Image does accept those, but a 2-4 MB base64 string is parsed on every
/// repaint and — measured on the first desktop build that carried user images — painted
/// nothing at all. Every other picture this client draws comes from a FILE (an agent's
/// shared image is downloaded first), so decode once here and hand QML a file URL: the
/// user's own image then takes exactly the path that is known to work.
QString materialiseChatImage(const QString &uri)
{
    if (!uri.startsWith(QLatin1String("data:")))
        return uri;
    const int comma = uri.indexOf(QLatin1Char(','));
    if (comma < 0)
        return uri;
    const QString header = uri.mid(5, comma - 5);   // "image/png;base64"
    if (!header.endsWith(QLatin1String(";base64")))
        return uri;
    const QString mime = header.left(header.size() - 7);
    const QByteArray bytes = QByteArray::fromBase64(uri.mid(comma + 1).toLatin1());
    if (bytes.isEmpty())
        return uri;
    // Content-addressed, so the same picture sent twice (or replayed) is written once.
    const QString digest = QString::fromLatin1(
        QCryptographicHash::hash(bytes, QCryptographicHash::Sha1).toHex().left(16));
    QString suffix = QMimeDatabase().mimeTypeForName(mime).preferredSuffix();
    if (suffix.isEmpty())
        suffix = QStringLiteral("img");
    QDir dir(QStandardPaths::writableLocation(QStandardPaths::CacheLocation)
             + QStringLiteral("/chat-images"));
    if (!dir.exists() && !dir.mkpath(QStringLiteral(".")))
        return uri;
    const QString path = dir.filePath(digest + QLatin1Char('.') + suffix);
    if (!QFileInfo::exists(path)) {
        QSaveFile out(path);
        if (!out.open(QIODevice::WriteOnly) || out.write(bytes) != bytes.size() || !out.commit())
            return uri;   // fall back to the URI: a broken cache must not hide the picture
    }
    return QUrl::fromLocalFile(path).toString();
}

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

QStringList ChatController::pickAttachmentFiles()
{
    const QString dir = lastAttachDir();
    const QStringList files = QFileDialog::getOpenFileNames(
        nullptr, QStringLiteral("Attach files"), dir);
    if (!files.isEmpty()) {
        const QFileInfo info(files.first());
        setLastAttachDir(info.absolutePath());
    }
    return files;
}

QStringList ChatController::clipboardAttachmentFiles()
{
    const QMimeData *mime = QGuiApplication::clipboard()->mimeData();
    if (!mime)
        return {};
    // A file copied from a file manager (Dolphin, Spectacle's "copy to clipboard"):
    // attach the files themselves. Their bytes must not go through an image
    // round-trip, and non-image files are attachable too.
    QStringList files;
    if (mime->hasUrls()) {
        for (const QUrl &u : mime->urls()) {
            if (!u.isLocalFile())
                continue;
            const QString path = u.toLocalFile();
            if (!path.isEmpty() && QFileInfo::exists(path))
                files << path;
        }
        if (!files.isEmpty())
            return files;
    }
    // Raw image data with no file behind it (a screenshot copied out of a browser).
    // An attachment is a file path end to end, so park it under a name that carries
    // a real image extension — the transcript picks its thumbnail renderer from it.
    if (!mime->hasImage())
        return {};
    const QImage image = qvariant_cast<QImage>(mime->imageData());
    if (image.isNull())
        return {};
    const QString dir = QStandardPaths::writableLocation(QStandardPaths::TempLocation);
    if (dir.isEmpty())
        return {};
    const QString path = QStringLiteral("%1/grouse-paste-%2.png")
                             .arg(dir, QString::number(QDateTime::currentMSecsSinceEpoch()));
    if (!image.save(path, "PNG"))
        return {};
    return {path};
}

QVariantList ChatController::buildAttachmentBlocks(const QVariantList &paths)
{
    // Same ACP content-block shapes as before; promptJson() converts them to the
    // core's serde Prompt JSON at the wire boundary.
    QVariantList out;
    QMimeDatabase mimeDb;
    for (const auto &v : paths) {
        const QString path = v.toString();
        if (path.isEmpty())
            continue;
        const QString mime = mimeDb.mimeTypeForFile(path).name();
        if (!mime.startsWith(QLatin1String("image/")))
            continue;   // uploaded by path -- see attachmentPaths()
        QFile f(path);
        if (!f.open(QIODevice::ReadOnly))
            continue;
        const QByteArray data = f.readAll();
        f.close();
        QString b64 = QString::fromLatin1(data.toBase64());
        b64.remove(QLatin1Char('\n')).remove(QLatin1Char('\r'));
        out << QVariantMap{{"type", "image"}, {"mimeType", mime}, {"data", b64}};
        // NOTHING else becomes a block. A `resource` block carrying a base64
        // `blob` is DROPPED by goose with no placeholder and no error, so the
        // file goes through attachmentPaths() and is uploaded instead; the core
        // then appends a `resource_link` that the server resolves itself.
    }
    return out;
}

QStringList ChatController::attachmentPaths(const QVariantList &paths)
{
    // Everything that is not an image: the core opens these itself (it shares
    // this machine's filesystem), uploads them chunked, and appends a
    // `resource_link` per file. The alternative -- inlining the bytes as a
    // `resource` block -- is silently dropped by goose.
    QStringList out;
    QMimeDatabase mimeDb;
    for (const auto &v : paths) {
        const QString path = v.toString();
        if (path.isEmpty() || !QFileInfo::exists(path))
            continue;
        if (mimeDb.mimeTypeForFile(path).name().startsWith(QLatin1String("image/")))
            continue;
        out << path;
    }
    return out;
}

void ChatController::cancelTurn()
{
    // Stop means stop HERE: discarding another chat's parked message because you stopped
    // this one throws away input the user never cancelled (Android parity).
    const int before = m_pendingQueue.size();
    for (int i = m_pendingQueue.size() - 1; i >= 0; --i) {
        if (m_pendingQueue.at(i).sessionId == m_currentSessionId)
            m_pendingQueue.removeAt(i);
    }
    if (m_pendingQueue.size() != before)
        emit m_manager.queuedChanged();
    if (m_bridge && m_bridge->isAvailable())
        m_bridge->api().grouse_cancel(m_bridge->handle());
}

void ChatController::compactConversation()
{
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_ready(m_bridge->handle())
        || (m_prompting && m_promptingSessionId == m_currentSessionId))
        return;
    m_compacting = true;
    emit m_manager.compactingChanged();
    const QByteArray prompt = promptJson(QStringLiteral("/compact"), {}).toUtf8();
    char *err = nullptr;
    m_bridge->api().grouse_send_prompt(m_bridge->handle(), prompt.constData(), nullptr, &err);
    if (err)
        m_bridge->api().grouse_string_free(err);
}

void ChatController::onStatus(const QString &json)
{
    const QString s = json;
    // The card-pin check is a dial-time refusal. It arrives as an Error/Alert
    // envelope (or a plain string); surface the actionable text even when the
    // generic status branch below only paints the line.
    const QJsonObject statusObj = parseObj(s);
    QString statusText = s;
    for (const char *key : {"Error", "Alert"}) {
        const QJsonValue v = statusObj.value(QLatin1String(key));
        if (v.isObject() && v.toObject().contains(QStringLiteral("message"))) {
            statusText = v.toObject().value(QStringLiteral("message")).toString();
            break;
        }
    }
    maybeShowCardPinRefusal(statusText, QString());
    // Ready is a CONNECTION phase, never an open result. An open emits its own
    // on_session_opened (a paint / a Window is not one either: background fills
    // send those). Leave a chat's loading line alone until that answer arrives.
    const bool openingChat = m_status == QStringLiteral("loading…")
                             || m_status == QStringLiteral("reloading…")
                             || m_status == QStringLiteral("reloading from the start…");
    if (s == QStringLiteral("\"Ready\"") || s == QStringLiteral("Ready")) {
        setOnline(true);
        if (!openingChat)
            setStatus(QStringLiteral("ready"));
        // A MAIN-wire Ready may only clear turns OWNED BY THE MAIN WIRE. A peer's
        // turn is carried by its own connection; clearing it here is the desktop
        // twin of the Android bug fixed today — a roam chat's composer unlocks
        // while its reply is still streaming and the next send lands in a live turn.
        // Ownership comes from the core, not from text parsing of the session id.
        const bool currentIsRoam = !sessionPeerLabel(m_currentSessionId).isEmpty();
        if (!currentIsRoam && mainWireOwnsTurnState(m_activePeerLabel)) {
            m_prompting = false;
            m_promptingSessionId.clear();
            // A reconnected client cannot receive the old turns' run ends.
            m_inFlight.clear();
            m_activeRunIds.clear();
            emit m_manager.promptingChanged();
        }
        if (m_wireOnlyConnect) {
            m_wireOnlyConnect = false;
            // A wire-only connect binds nothing. If the chat on screen is a MAIN
            // chat, bind it here (without the UI reset of openSession) so the
            // send queue can drain; a roam screen gets no main chat painted over it.
            if (m_activePeerLabel.isEmpty() && !m_landing && !currentIsRoam)
                bindSessionAfterAttach();
        }
        // Resolve a pending testConnection() probe. The failure paths
        // (Disconnected/Error) already reported; only Ready completed silently,
        // which made a SUCCESSFUL test indistinguishable from "nothing happened".
        if (m_testPending) {
            m_testPending = false;
            emit m_manager.connectionTested(true,
                QStringLiteral("Connection OK — %1:%2, handshake complete.")
                    .arg(useTls() ? QStringLiteral("wss://") : QStringLiteral("ws://"),
                         host().trimmed(), port().trimmed()));
        }
        // The core bridge resolves lazily; by Ready it is definitely live, so this
        // is the safe point to re-arm dials for persisted roam peers (once). Roam
        // is opt-in, so stay dormant (and leave m_roamRestored false) when off.
        if (roamEnabled() && !m_roamRestored) {
            m_roamRestored = true;
            syncRoamIdentityToCore();
            restoreRoamPeers();
        }
        syncRunSummariesToCore();
        syncAdvisorToCore();
        syncRescaleToCore();
        flushQueue();
        refreshSessions();
        refreshProjects();
        refreshRecipes();
        refreshProviders();
        // Tool state must land WITH the session, not on the next toggle: global
        // extension catalog + this session's active tools + attached extensions.
        refreshToolGroups();
    } else if (s == QStringLiteral("\"Connecting\"") || s == QStringLiteral("Connecting")) {
        setOnline(false);
        setStatus(QStringLiteral("connecting…"));
    } else if (s == QStringLiteral("\"Syncing\"") || s == QStringLiteral("Syncing")) {
        setStatus(QStringLiteral("syncing…"));
    } else if (s.contains(QLatin1String("Alert"))) {
        // Advisory failure on a control path (e.g. a set_config_option the
        // peer refused): text in the status bar only — the connection did not
        // go down, so the online state and turn wiring are untouched.
        QString msg = parseObj(s).value(QLatin1String("Alert")).toObject().value(QStringLiteral("message")).toString();
        if (!msg.isEmpty())
            setStatus(msg);
    } else if (s == QStringLiteral("\"Disconnected\"") || s == QStringLiteral("Disconnected")) {
        setOnline(false);
        m_wireOnlyConnect = false;
        setStatus(QStringLiteral("not connected"));
        if (m_activePeerLabel.isEmpty())
            releaseTurnForLostWire(m_currentSessionId);
        if (m_testPending) {
            m_testPending = false;
            emit m_manager.connectionTested(false, QStringLiteral("Connection failed — disconnected."));
        }
    } else {
        // Error { "message": ... }
        QJsonObject o = parseObj(s);
        QString msg = o.value(QStringLiteral("Error")).toObject().value(QStringLiteral("message")).toString();
        setOnline(false);
        m_wireOnlyConnect = false;
        setStatus(msg.isEmpty() ? QStringLiteral("connection error") : msg);
        if (m_activePeerLabel.isEmpty())
            releaseTurnForLostWire(m_currentSessionId);
        if (m_testPending) {
            m_testPending = false;
            emit m_manager.connectionTested(false, msg.isEmpty() ? QStringLiteral("Connection failed.") : msg);
        }
    }
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

void ChatController::onSessionOpened(const QString &sessionId)
{
    if (sessionId.isEmpty() || sessionId != m_currentSessionId)
        return;   // a background bind or another peer's open is not THIS chat's answer
    // The core emits this only after the store is painted and the session is bound
    // (a rejected load and a bind-only load stay silent). It is the one honest end
    // of the "loading…" line we set locally on open — Ready and Window are not.
    if (m_status == QStringLiteral("loading…")
        || m_status == QStringLiteral("reloading…")
        || m_status == QStringLiteral("reloading from the start…"))
        setStatus(QStringLiteral("ready"));
    // Same for the replay bar: a suppressed replay emits no progress at all, and
    // a paint whose ReplayProgress was lost used to leave the bar spinning after
    // the chat was already settled. A completed open owns both bars' finish.
    resetReloadProgress();
    if (m_openFor == sessionId) {
        timingLog(QStringLiteral("open %1 settled in %2ms (%3 rows)")
                      .arg(sessionId)
                      .arg(m_openClock.elapsed())
                      .arg(m_messageModel->rowCount()));
    }
}

void ChatController::onSessionOpenFailed(const QString &sessionId, const QString &message)
{
    // The core bound NOTHING for a failed open (grouse#19): the chat keeps
    // whatever the cache painted and the wire has no session. Surface the
    // failure where "reloading…" was, so an open that never loaded is visible
    // instead of looking like an empty conversation.
    setStatus(QStringLiteral("couldn't open that chat — %1").arg(message));
    // A failed open cannot finish a replay either; drop the bar with the status.
    resetReloadProgress();
    // A dead thread link is dropped by the CORE, which owns the map: a failed open is
    // the one signal a session invisible to every list can produce (grouse#19).
    emit m_manager.currentThreadLabelChanged();
}

void ChatController::onRunEnded(const QString &sessionId, const QString &stopReason)
{
    Q_UNUSED(stopReason);
    // Every completion clears ITS chat; only the chat on screen owns the UI state.
    m_inFlight.remove(sessionId);
    m_activeRunIds.remove(sessionId);
    if (!sessionId.isEmpty() && sessionId != m_currentSessionId)
        return;
    m_prompting = false;
    m_promptingSessionId.clear();
    m_compacting = false;
    m_compactionStatus.clear();
    setActiveRunId(QString());
    emit m_manager.promptingChanged();
    emit m_manager.compactingChanged();
    flushQueue();
    // Before the turn-over side effects: the last bubble is final now, so any
    // markdown the seal missed gets rendered while the row is still the last
    // thing the user is looking at.
    finishTail();
    notifyTurnFinished();
    // A delegate that ended is a lane whose status just changed, and the panel must not
    // keep saying "running" after the turn is over.
    refreshActivity();
}

void ChatController::onActiveRunChanged(const QString &sessionId, const QString &runId)
{
    // Run ids are per session: a background chat's run must not replace the
    // steer key of the chat on screen, and its end must not clear it.
    if (runId.isEmpty()) {
        m_activeRunIds.remove(sessionId);
        m_inFlight.remove(sessionId);
        if (sessionId == m_currentSessionId || sessionId.isEmpty())
            setActiveRunId(QString());
        return;
    }
    m_activeRunIds.insert(sessionId, runId);
    m_inFlight.insert(sessionId);
    if (sessionId == m_currentSessionId)
        setActiveRunId(runId);
}

void ChatController::onCompactionStatus(const QString &message)
{
    // Substring matching was far too greedy: ANY status line merely containing
    // "compact" (a "Compaction complete" notice, or text that names another
    // session's background run) armed the spinner, and the clear depended on
    // the exact "complete"/"error" strings ever arriving. Only arm on the
    // phrases goose actually uses for the RUNNING compaction — every one of
    // them describes the act of compacting in the progressive — and clear on
    // its terminators. Turn-end (coreOnRunEnded) clears it as the backstop.
    const QString m = message.toLower();
    const bool arming = m.contains(QLatin1String("compacting"))
        || (m.contains(QLatin1String("compaction"))
            && (m.endsWith(QLatin1String("...")) || m.endsWith(QLatin1String("…"))));
    const bool done = m.contains(QLatin1String("complete"))
        || m.contains(QLatin1String("error"))
        || m.contains(QLatin1String("failed"))
        || m.contains(QLatin1String("cleared"));
    if (arming && !done) {
        m_compacting = true;
        m_compactionStatus = message;
    } else if (done) {
        m_compacting = false;
        m_compactionStatus.clear();
    }
    emit m_manager.compactingChanged();
}
