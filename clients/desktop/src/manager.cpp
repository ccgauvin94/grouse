// SPDX-License-Identifier: AGPL-3.0-or-later

#include "manager.h"

#include "corebridge.h"
#include "markdown.h"
#include "messagelistmodel.h"
#include "notifier.h"
#include "roamlistmodel.h"
#include "sessionlistmodel.h"

#include <QDateTime>
#include <QDir>
#include <QDesktopServices>
#include <QFile>
#include <QFileDialog>
#include <QFileInfo>
#include <QGuiApplication>
#include <QUrl>
#include <QClipboard>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QMimeDatabase>
#include <QSet>
#include <QStandardPaths>
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
    , m_store(QSettings::UserScope, QStringLiteral("grouse"), QStringLiteral("grouse-desktop"))
{
    m_sessionsModel = new SessionListModel(this);
    m_messageModel = new MessageListModel(this);
    m_roamModel = new RoamListModel(this);

    // Coalesce transcript updates while a turn streams (same rate-limit as the
    // old per-chunk path): the signal fires at most every 50ms.
    m_peekTimer = new QTimer(this);
    m_peekTimer->setSingleShot(true);
    connect(m_peekTimer, &QTimer::timeout, this, &Manager::doPeekStep);

    m_updateTimer = new QTimer(this);
    m_updateTimer->setSingleShot(true);
    m_updateTimer->setInterval(50);
    connect(m_updateTimer, &QTimer::timeout, this, [this] { emit messagesChanged(); });

    // The core is the sole wire path. dlopen + resolve on first use.
    CoreBridge *bridge = CoreBridge::instance();
    m_bridge = bridge;
    bridge->setTarget(this);
    bridge->installListener();
}

void Manager::requestMessagesUpdate()
{
    if (!m_updateTimer->isActive())
        m_updateTimer->start();
}

QString Manager::host() const { return m_store.value("host", "192.168.1.5").toString(); }
QString Manager::port() const { return m_store.value("port", "3284").toString(); }
QString Manager::secretKey() const { return m_store.value("secret", "").toString().trimmed(); }
bool Manager::useTls() const { return m_store.value("wss", true).toBool(); }
bool Manager::autoConnectEnabled() const { return m_store.value("auto_connect", true).toBool(); }
bool Manager::notificationsEnabled() const { return m_store.value("notify_events", true).toBool(); }
bool Manager::configuredProvidersOnly() const { return m_store.value("configured_providers_only", true).toBool(); }
QString Manager::workingDir() const { return m_store.value("cwd", "").toString(); }
bool Manager::roamEnabled() const { return m_store.value("roam_enabled", false).toBool(); }

void Manager::setRoamEnabled(bool v)
{
    if (roamEnabled() == v)
        return;
    m_store.setValue("roam_enabled", v);
    emit settingsChanged();
    // Enabling mid-session must bring stored peers up without a restart: the
    // Ready hook below only re-arms them once per launch.
    if (v && m_online && !m_roamRestored) {
        m_roamRestored = true;
        syncRoamIdentityToCore();
        restoreRoamPeers();
    }
}

void Manager::setHost(const QString &v) { m_store.setValue("host", v); emit settingsChanged(); }
void Manager::setPort(const QString &v) { m_store.setValue("port", v); emit settingsChanged(); }
void Manager::setSecretKey(const QString &v) { m_store.setValue("secret", v.trimmed()); emit settingsChanged(); }
void Manager::setUseTls(bool v) { m_store.setValue("wss", v); emit settingsChanged(); }
void Manager::setAutoConnectEnabled(bool v) { m_store.setValue("auto_connect", v); emit settingsChanged(); }
void Manager::setNotificationsEnabled(bool v) { m_store.setValue("notify_events", v); emit settingsChanged(); }
void Manager::setConfiguredProvidersOnly(bool v)
{
    m_store.setValue("configured_providers_only", v);
    emit settingsChanged();
    emit providersChanged();   // the pickers filter on this
}
void Manager::setWorkingDir(const QString &v)
{
    m_store.setValue("cwd", v.trimmed().remove(QRegularExpression(QStringLiteral("/+$"))));
    emit settingsChanged();
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

QString Manager::currentSessionTitle() const
{
    if (!m_currentSessionTitle.isEmpty())
        return m_currentSessionTitle;
    return m_currentSessionId.isEmpty() ? QStringLiteral("New Chat") : m_currentSessionId;
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

// ---------------------------------------------------------------------------
// ServerConfig JSON (input to grouse_connect)
// ---------------------------------------------------------------------------

QString Manager::serverConfigJson()
{
    QJsonObject o;
    o["host"] = host().trimmed();
    o["port"] = port().trimmed().toInt();
    o["secret_key"] = secretKey();
    o["use_tls"] = useTls();
    o["accept_invalid_certs"] = true; // historical self-signed-tailnet trust-all
    o["ca_cert_pem"] = QJsonValue::Null;
    o["cwd"] = workingDir();
    o["auto_connect"] = true;
    o["client_id"] = QStringLiteral("grouse-desktop");
    o["initial_recipe_id"] = m_pendingRecipeId.isEmpty()
        ? QJsonValue::Null : QJsonValue(m_pendingRecipeId);
    m_pendingRecipeId.clear();
    return QString::fromUtf8(QJsonDocument(o).toJson(QJsonDocument::Compact));
}

// ---------------------------------------------------------------------------
// Prompt JSON (input to grouse_send_prompt): serde Prompt { blocks: [...] }
// ---------------------------------------------------------------------------

QString Manager::promptJson(const QString &text, const QVariantList &blocks) const
{
    QJsonArray arr;
    if (!text.trimmed().isEmpty()) {
        arr.append(QJsonObject{{"Text", QJsonObject{{"text", text}}}});
    }
    for (const auto &b : blocks) {
        const QVariantMap m = b.toMap();
        const QString type = m.value("type").toString();
        if (type == QLatin1String("image")) {
            arr.append(QJsonObject{{"Image", QJsonObject{
                {"mime_type", m.value("mimeType").toString()},
                {"data", m.value("data").toString()}}}});
        } else if (type == QLatin1String("resource")) {
            const QVariantMap r = m.value("resource").toMap();
            QJsonObject res{{"uri", r.value("uri").toString()},
                            {"mime_type", r.value("mimeType").toString()}};
            if (r.contains("text"))
                res["text"] = r.value("text").toString();
            else
                res["text"] = QJsonValue::Null;
            res["blob"] = r.contains("blob") ? QJsonValue(r.value("blob").toString())
                                             : QJsonValue::Null;
            arr.append(QJsonObject{{"Resource", res}});
        }
    }
    return QString::fromUtf8(QJsonDocument(QJsonObject{{"blocks", arr}})
                                 .toJson(QJsonDocument::Compact));
}

// ---------------------------------------------------------------------------
// Intents (UI -> core)
// ---------------------------------------------------------------------------

void Manager::connectToServer()
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    if (secretKey().isEmpty()) {
        setStatus(QStringLiteral("no secret key"));
        return;
    }
    m_landing = false;
    emit landingChanged();
    m_lastCwd = workingDir();
    char *err = nullptr;
    const QByteArray cfg = serverConfigJson().toUtf8();
    m_bridge->api().grouse_connect(m_bridge->handle(), cfg.constData(), &err);
    if (err) {
        setStatus(QStringLiteral("connect failed: ") + QString::fromUtf8(err));
        m_bridge->api().grouse_string_free(err);
    } else {
        setStatus(QStringLiteral("connecting…"));
    }
}

void Manager::autoConnect()
{
    if (!autoConnectEnabled())
        return;
    if (host().trimmed().isEmpty() || secretKey().isEmpty()) {
        setStatus(QStringLiteral("not configured — press Connect"));
        return;
    }
    m_landing = true;
    connectToServer();
}

void Manager::disconnect()
{
    if (m_bridge && m_bridge->isAvailable())
        m_bridge->api().grouse_disconnect(m_bridge->handle());
    setOnline(false);
    m_prompting = false;
    emit promptingChanged();
    m_compacting = false;
    emit compactingChanged();
    m_landing = true;
    emit landingChanged();
    setStatus(QStringLiteral("disconnected"));
}

void Manager::connectRoam(const QString &card, const QString &label)
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
void Manager::syncRoamIdentityToCore()
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QString secret = m_store.value("roam_identity").toString().trimmed();
    if (secret.isEmpty())
        return;
    const QByteArray s = secret.toUtf8();
    m_bridge->api().grouse_set_roam_identity(m_bridge->handle(), s.constData());
}

void Manager::disconnectRoam(const QString &label)
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

void Manager::persistRoamCard(const QString &label, const QString &card)
{
    QVariantMap cards = m_store.value(QStringLiteral("roam_cards")).toMap();
    cards.insert(label, card);
    m_store.setValue(QStringLiteral("roam_cards"), cards);
}

void Manager::forgetRoamCard(const QString &label)
{
    QVariantMap cards = m_store.value(QStringLiteral("roam_cards")).toMap();
    if (cards.remove(label))
        m_store.setValue(QStringLiteral("roam_cards"), cards);
}

/// Re-dial + re-list every persisted roam peer so stored connections survive a
/// restart (the wires are the core's; this only re-arms them).
void Manager::restoreRoamPeers()
{
    const QVariantMap cards = m_store.value(QStringLiteral("roam_cards")).toMap();
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

void Manager::openRoamSession(const QString &label, const QString &sessionId, const QString &cwd)
{
    m_activePeerLabel = label;
    m_pendingQueue.clear();
    emit queuedChanged();
    m_landing = false;
    emit landingChanged();
    m_currentSessionId = sessionId;
    m_currentSessionTitle.clear();
    m_tools.clear();
    m_extDefs.clear();
    m_sessionExts.clear();
    m_sessionRestricted.clear();
    m_peekQueue.clear();
    m_peekFailed.clear();
    loadToolCache(sessionId);   // best-effort memory of last run's catalogues
    publishToolGroups();
    emit currentSessionChanged();
    emit toolsChanged();
    setStatus(QStringLiteral("loading…"));
    m_lastCwd = cwd.isEmpty() ? workingDir() : cwd;
    if (m_bridge && m_bridge->isAvailable()) {
        const QByteArray l = label.toUtf8();
        const QByteArray sid = sessionId.toUtf8();
        m_bridge->api().grouse_roam_open_session(m_bridge->handle(), l.constData(), sid.constData());
    }
}

void Manager::newRoamSession(const QString &label)
{
    m_activePeerLabel = label;
    m_pendingQueue.clear();
    emit queuedChanged();
    m_landing = false;
    emit landingChanged();
    m_currentSessionId.clear();
    m_currentSessionTitle.clear();
    m_tools.clear();
    m_extDefs.clear();
    m_sessionExts.clear();
    m_sessionRestricted.clear();
    m_peekQueue.clear();
    m_peekFailed.clear();
    publishToolGroups();
    m_messageModel->clear();
    m_currentIndex = -1;
    emit currentSessionChanged();
    emit toolsChanged();
    emit messagesChanged();
    setStatus(QStringLiteral("connecting…"));
    if (m_bridge && m_bridge->isAvailable()) {
        const QByteArray l = label.toUtf8();
        m_bridge->api().grouse_roam_new_session(m_bridge->handle(), l.constData());
    }
}

void Manager::newRoamSessionIn(const QString &label, const QString &cwd)
{
    m_activePeerLabel = label;
    m_pendingQueue.clear();
    emit queuedChanged();
    m_landing = false;
    emit landingChanged();
    m_currentSessionId.clear();
    m_currentSessionTitle.clear();
    m_tools.clear();
    m_extDefs.clear();
    m_sessionExts.clear();
    m_sessionRestricted.clear();
    m_peekQueue.clear();
    m_peekFailed.clear();
    publishToolGroups();
    m_messageModel->clear();
    m_currentIndex = -1;
    emit currentSessionChanged();
    emit toolsChanged();
    emit messagesChanged();
    setStatus(QStringLiteral("connecting…"));
    if (m_bridge && m_bridge->isAvailable()) {
        const QByteArray l = label.toUtf8();
        const QString t = cwd.trimmed();
        const QByteArray c = t.toUtf8();
        m_bridge->api().grouse_roam_new_session_cwd(
            m_bridge->handle(), l.constData(), t.isEmpty() ? nullptr : c.constData());
    }
}

void Manager::toggleRoamPeer(const QString &label)
{
    m_roamModel->togglePeer(label);
}

void Manager::setActiveTab(const QString &tab)
{
    if (tab == QLatin1String("main") && !m_activePeerLabel.isEmpty())
        m_activePeerLabel.clear();
}

QString Manager::roamIdentity()
{
    QString secret = m_store.value("roam_identity").toString();
    if (secret.isEmpty() && m_bridge && m_bridge->isAvailable()) {
        secret = m_bridge->takeString(m_bridge->api().grouse_identity_generate());
        if (!secret.isEmpty())
            m_store.setValue("roam_identity", secret);
    }
    return secret;
}

QString Manager::roamPublicKey() const
{
    const QString secret = m_store.value("roam_identity").toString();
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

QString Manager::roamCard() const
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

void Manager::copyToClipboard(const QString &s)
{
    QGuiApplication::clipboard()->setText(s);
}

QObject *Manager::roamModel() const { return m_roamModel; }

void Manager::testConnection()
{
    if (!m_bridge || !m_bridge->isAvailable()) {
        emit connectionTested(false, QStringLiteral("grouse-core not loaded."));
        return;
    }
    if (secretKey().isEmpty()) {
        emit connectionTested(false, QStringLiteral("No secret key set — fill it in above."));
        return;
    }
    // Drive a real connect; the resulting on_status (Ready/Error) resolves the
    // probe via coreOnStatus. The core owns the connection.
    m_testPending = true;
    connectToServer();
}

void Manager::openSession(const QString &sessionId)
{
    m_activePeerLabel.clear();
    m_pendingQueue.clear();
    emit queuedChanged();
    m_landing = false;
    emit landingChanged();
    m_currentSessionId = sessionId;
    m_currentSessionTitle.clear();
    for (const auto &v : m_sessions) {
        const QVariantMap s = v.toMap();
        if (s.value("sessionId").toString() == sessionId) {
            m_currentSessionTitle = s.value("title").toString();
            break;
        }
    }
    m_tools.clear();
    m_extDefs.clear();
    m_sessionExts.clear();
    m_sessionRestricted.clear();
    m_peekQueue.clear();
    m_peekFailed.clear();
    loadToolCache(sessionId);
    publishToolGroups();
    m_messageModel->clear();
    m_currentIndex = -1;
    emit currentSessionChanged();
    emit toolsChanged();
    emit messagesChanged();
    setStatus(QStringLiteral("loading…"));
    m_lastCwd = workingDir();
    if (m_bridge && m_bridge->isAvailable()) {
        const QByteArray sid = sessionId.toUtf8();
        m_bridge->api().grouse_open_session(m_bridge->handle(), sid.constData());
        m_bridge->api().grouse_load_cached_transcript(m_bridge->handle(), sid.constData());
    }
}

void Manager::newChat()
{
    m_activePeerLabel.clear();
    m_pendingQueue.clear();
    emit queuedChanged();
    m_landing = false;
    emit landingChanged();
    m_currentSessionId.clear();
    m_currentSessionTitle.clear();
    m_tools.clear();
    m_extDefs.clear();
    m_sessionExts.clear();
    m_sessionRestricted.clear();
    m_peekQueue.clear();
    m_peekFailed.clear();
    m_messageModel->clear();
    m_currentIndex = -1;
    publishToolGroups();
    emit currentSessionChanged();
    emit toolsChanged();
    emit messagesChanged();
    setStatus(QStringLiteral("connecting…"));
    char *err = nullptr;
    const QByteArray cfg = serverConfigJson().toUtf8();
    if (m_bridge && m_bridge->isAvailable()) {
        m_bridge->api().grouse_new_session(m_bridge->handle(), nullptr, &err);
        if (err) {
            setStatus(QStringLiteral("new session failed: ") + QString::fromUtf8(err));
            m_bridge->api().grouse_string_free(err);
        }
    }
}

void Manager::beginChat()
{
    if (!m_landing)
        return;
    m_landing = false;
    emit landingChanged();
}

void Manager::sendPrompt(const QString &text, const QVariantList &images)
{
    if (!m_bridge || !m_bridge->isAvailable() || (text.trimmed().isEmpty() && images.isEmpty()))
        return;
    m_landing = false;
    emit landingChanged();

    const QString expectJson = m_currentSessionId.isEmpty()
        ? QString() : QStringLiteral("{\"session_id\":\"%1\"}").arg(m_currentSessionId);

    dispatchSend(text.trimmed(), buildAttachmentBlocks(images));
}

void Manager::dispatchSend(const QString &text, const QVariantList &blocks)
{
    const QString expectJson = m_currentSessionId.isEmpty()
        ? QString() : QStringLiteral("{\"session_id\":\"%1\"}").arg(m_currentSessionId);
    const bool ready = m_bridge && m_bridge->isAvailable() && m_bridge->api().grouse_ready(m_bridge->handle());
    // Local echo of the question (Android's send() does the same): the live wire
    // carries NO user_message_chunk — the prompt only appears in the next
    // session/load replay — so without this the bubble surfaces after the reply.
    // A replay's Clear rebuilds the model from the store anyway, so no duplicate.
    QVariantMap userRow;
    userRow["id"] = QString();
    userRow["role"] = QStringLiteral("user");
    userRow["text"] = text;
    userRow["html"] = markdownToHtml(text);
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
    if (!shown.isEmpty())
        userRow["images"] = shown;
    m_messageModel->append(userRow);
    m_currentIndex = m_messageModel->count() - 1;
    requestMessagesUpdate();
    if (ready && !m_prompting) {
        m_prompting = true;
        m_promptingSessionId = m_currentSessionId;
        emit promptingChanged();
        char *err = nullptr;
        const QByteArray prompt = promptJson(text, blocks).toUtf8();
        const QByteArray expect = expectJson.toUtf8();
        m_bridge->api().grouse_send_prompt(m_bridge->handle(), prompt.constData(),
                                           expectJson.isEmpty() ? nullptr : expect.constData(), &err);
        if (err) {
            onError(QString::fromUtf8(err), false);
            m_bridge->api().grouse_string_free(err);
        }
    } else if (ready && m_prompting && !m_activeRunId.isEmpty() && shown.isEmpty()) {
        // STEER (Android parity): the turn already running on THIS session is
        // redirected by this message instead of queueing behind it. m_activeRunId
        // is only ever set for the current session, so a run on another session
        // can't be steered from here. Text only — steering carries no blocks, so
        // a message with attachments queues rather than silently dropping them.
        // The server validates expected_run_id: a run that ended between typing
        // and sending fails loudly instead of starting a stray second turn.
        const QByteArray t = text.toUtf8();
        const QByteArray r = m_activeRunId.toUtf8();
        m_bridge->api().grouse_unstable_steer(m_bridge->handle(), t.constData(), r.constData());
    } else {
        // Not ready, or a turn is running without a steer key: queue (the core
        // flushes the prompt queue itself; this app-level queue only waits for
        // ready()).
        enqueue({text, blocks});
        if (!ready && !secretKey().isEmpty())
            connectToServer();
    }
}

bool Manager::wireUpForCurrentChat() const
{
    if (!m_activePeerLabel.isEmpty())
        return m_roamModel && m_roamModel->peerConnected(m_activePeerLabel);
    return m_online;
}

void Manager::releaseTurnForLostWire(const QString &lostSessionId)
{
    // Only the wire that OWNS the turn may release it: a drop in another chat, or on
    // the main socket while a peer owns the turn, must leave it alone (Android parity).
    if (!turnOwnerMatches(m_promptingSessionId, m_currentSessionId, lostSessionId))
        return;
    // A dead host emits neither RunEnded nor a cleared run id, so without this
    // m_prompting stays true forever: "N queued — will send when this turn finishes"
    // is a promise that can never be kept and the queue never drains.
    m_promptingSessionId.clear();
    if (m_prompting) {
        m_prompting = false;
        emit promptingChanged();
    }
    if (m_compacting) {
        m_compacting = false;
        emit compactingChanged();
    }
    setActiveRunId(QString());
    // The queue itself survives (the core flushes on the next Ready); releasing the
    // latch is what lets it move at all.
    flushQueue();
}

void Manager::setActiveRunId(const QString &runId)
{
    if (m_activeRunId == runId)
        return;
    m_activeRunId = runId;
    emit activeRunIdChanged();
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
    // Same ACP content-block shapes as before; promptJson() converts them to the
    // core's serde Prompt JSON at the wire boundary.
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
    m_pendingQueue.clear();
    emit queuedChanged();
    if (m_bridge && m_bridge->isAvailable())
        m_bridge->api().grouse_cancel(m_bridge->handle());
}

void Manager::compactConversation()
{
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_ready(m_bridge->handle()) || m_prompting)
        return;
    m_compacting = true;
    emit compactingChanged();
    const QByteArray prompt = promptJson(QStringLiteral("/compact"), {}).toUtf8();
    char *err = nullptr;
    m_bridge->api().grouse_send_prompt(m_bridge->handle(), prompt.constData(), nullptr, &err);
    if (err)
        m_bridge->api().grouse_string_free(err);
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

void Manager::respondPermission(const QString &toolCallId, const QString &optionId)
{
    if (m_bridge && m_bridge->isAvailable()) {
        const QByteArray id = toolCallId.toUtf8();
        const QByteArray outcome = optionId.isEmpty()
            ? QByteArrayLiteral("\"Cancelled\"")
            : QJsonDocument(QJsonObject{{"Selected", QJsonObject{{"option_id", optionId}}}})
                  .toJson(QJsonDocument::Compact);
        m_bridge->api().grouse_respond_permission(m_bridge->handle(), id.constData(), outcome.constData());
    }
    m_permToolCallId.clear();
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
    if (m_bridge && m_bridge->isAvailable())
        m_bridge->api().grouse_list_sessions(m_bridge->handle());
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

void Manager::refreshRecipes()
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    m_bridge->api().grouse_unstable_recipes_list(m_bridge->handle());
    m_bridge->api().grouse_unstable_schedules_list(m_bridge->handle());
}

void Manager::runRecipe(const QString &id)
{
    m_pendingRecipeId = id;
    m_pendingProjectFiling.clear();
    newChat();
}

void Manager::scheduleRecipe(const QString &id, const QString &cron)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QByteArray rid = id.toUtf8();
    const QByteArray c = cron.isEmpty() ? QByteArray() : cron.toUtf8();
    m_bridge->api().grouse_unstable_recipes_schedule(m_bridge->handle(), rid.constData(),
                                                     cron.isEmpty() ? nullptr : c.constData());
}

void Manager::deleteRecipe(const QString &id)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QByteArray rid = id.toUtf8();
    m_bridge->api().grouse_unstable_recipes_delete(m_bridge->handle(), rid.constData());
}

// recipes/save replaces the WHOLE recipe: the DTO must be the complete listed
// object with only the edited keys changed (mirrors Android's recipeWith).
void Manager::saveRecipe(const QString &id, const QString &recipeJson)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QByteArray rid = id.toUtf8();
    const QByteArray rj = recipeJson.toUtf8();
    m_bridge->api().grouse_unstable_recipes_save(m_bridge->handle(), rid.constData(),
                                                 rj.constData());
}

void Manager::setSchedulePaused(const QString &scheduleId, bool paused)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QByteArray s = scheduleId.toUtf8();
    auto f = paused ? m_bridge->api().grouse_unstable_schedules_pause
                    : m_bridge->api().grouse_unstable_schedules_unpause;
    f(m_bridge->handle(), s.constData());
}

void Manager::runScheduleNow(const QString &scheduleId)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QByteArray s = scheduleId.toUtf8();
    m_bridge->api().grouse_unstable_schedules_run_now(m_bridge->handle(), s.constData());
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
    if (m_bridge && m_bridge->isAvailable())
        m_bridge->api().grouse_unstable_sources_list(m_bridge->handle(), "skill");
}

void Manager::saveSkill(const QString &path, const QString &name,
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
    if (!m_bridge || !m_bridge->isAvailable() || path.isEmpty())
        return;
    const QByteArray type = QByteArrayLiteral("skill");
    const QByteArray p = path.toUtf8();
    m_bridge->api().grouse_unstable_sources_delete(m_bridge->handle(), type.constData(), p.constData());
}

// ---- server config (providers) ---------------------------------------------

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

void Manager::setServerConfig(const QString &key, const QString &value)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QByteArray k = key.toUtf8();
    const QByteArray v = value.toUtf8();
    m_bridge->api().grouse_unstable_config_upsert(m_bridge->handle(), k.constData(), v.constData());
    m_bridge->api().grouse_unstable_config_read(m_bridge->handle(), k.constData());
}

void Manager::readServerConfig(const QString &key)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const QByteArray k = key.toUtf8();
    m_bridge->api().grouse_unstable_config_read(m_bridge->handle(), k.constData());
}

void Manager::refreshSupportedModels(const QString &providerId)
{
    if (m_bridge && m_bridge->isAvailable() && !providerId.isEmpty()) {
        const QByteArray p = providerId.toUtf8();
        m_bridge->api().grouse_unstable_supported_models(m_bridge->handle(), p.constData());
    }
}

// ---- per-session tool management ------------------------------------------

void Manager::refreshToolGroups()
{
    if (!m_bridge || !m_bridge->isAvailable() || !m_bridge->api().grouse_ready(m_bridge->handle()))
        return;
    // The row catalog is the GLOBAL extension list; the switches are the CURRENT
    // session's attached set; the tool checkboxes are the session's active tools.
    // All three, every time — the switches used to refresh only as a side effect
    // of an add's re-list, so opening a chat showed everything OFF until the user
    // toggled one thing (which "fixed" the rest).
    m_bridge->api().grouse_unstable_list_global_extensions(m_bridge->handle());
    const QByteArray sid = m_currentSessionId.toUtf8();
    if (sid.isEmpty())
        return;   // no session open: nothing session-scoped to pull
    m_bridge->api().grouse_unstable_list_tools(m_bridge->handle(), sid.constData());
    m_bridge->api().grouse_unstable_session_extensions_list(m_bridge->handle(), sid.constData());
}

void Manager::discoverToolGroup(const QString &extName)
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

void Manager::setSessionExtensionEnabled(const QString &extName, bool enabled)
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

void Manager::setSessionToolEnabled(const QString &extName, const QString &toolName, bool on)
{
    const ExtDef *d = extDef(extName);
    if (!d)
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

QVariant Manager::globalExtensions() const
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

void Manager::refreshGlobalExtensions()
{
    if (m_bridge && m_bridge->isAvailable())
        m_bridge->api().grouse_unstable_list_global_extensions(m_bridge->handle());
}

void Manager::setGlobalExtensionEnabled(const QString &extKey, bool enabled)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const ExtDef *d = extDef(extKey);
    if (!d)
        return;
    m_bridge->api().grouse_unstable_set_extension_enabled(
        m_bridge->handle(), d->key.toUtf8().constData(), enabled ? 1 : 0);
}

void Manager::setGlobalToolEnabled(const QString &extName, const QString &toolName, bool on)
{
    const ExtDef *d = extDef(extName);
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

const Manager::ExtDef *Manager::extDef(const QString &key) const
{
    for (const auto &d : m_extDefs)
        if (d.key == key)
            return &d;
    return nullptr;
}

void Manager::setSessionTools(const QString &extName, const QStringList &allowed)
{
    const ExtDef *d = extDef(extName);
    if (!d)
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

void Manager::publishToolGroups()
{
    emit toolGroupsChanged();
}

void Manager::loadToolCache(const QString &sessionId)
{
    // The catalogue is extension knowledge, not session state: reading it back
    // is what lets "does this row have >=2 sub-tools?" survive a restart
    // instead of re-arrowing every row on first open. Only keys not already
    // learned this run are merged (live truth wins over memory).
    // Global memory first: one server-wide sweep result shared by every chat.
    const QByteArray persisted =
        m_store.value(QStringLiteral("tool_catalogs")).toString().toUtf8();
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
    const QString s = json;
    if (s == QStringLiteral("\"Ready\"") || s == QStringLiteral("Ready")) {
        setOnline(true);
        setStatus(QStringLiteral("ready"));
        m_prompting = false;
        emit promptingChanged();
        // Resolve a pending testConnection() probe. The failure paths
        // (Disconnected/Error) already reported; only Ready completed silently,
        // which made a SUCCESSFUL test indistinguishable from "nothing happened".
        if (m_testPending) {
            m_testPending = false;
            emit connectionTested(true,
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
        flushQueue();
        refreshSessions();
        refreshProjects();
        refreshRecipes();
        refreshProviders();
        // Tool state must land WITH the session, not on the next toggle: global
        // extension catalog + this session's active tools + attached extensions.
        refreshToolGroups();
        if (!m_pendingProjectFiling.isEmpty()) {
            const QString proj = m_pendingProjectFiling;
            m_pendingProjectFiling.clear();
            if (!m_currentSessionId.isEmpty())
                moveSessionToProject(m_currentSessionId, proj);
        }
    } else if (s == QStringLiteral("\"Connecting\"") || s == QStringLiteral("Connecting")) {
        setOnline(false);
        setStatus(QStringLiteral("connecting…"));
    } else if (s == QStringLiteral("\"Syncing\"") || s == QStringLiteral("Syncing")) {
        setStatus(QStringLiteral("syncing…"));
    } else if (s == QStringLiteral("\"Disconnected\"") || s == QStringLiteral("Disconnected")) {
        setOnline(false);
        setStatus(QStringLiteral("not connected"));
        if (m_activePeerLabel.isEmpty())
            releaseTurnForLostWire(m_currentSessionId);
        if (m_testPending) {
            m_testPending = false;
            emit connectionTested(false, QStringLiteral("Connection failed — disconnected."));
        }
    } else {
        // Error { "message": ... }
        QJsonObject o = parseObj(s);
        QString msg = o.value(QStringLiteral("Error")).toObject().value(QStringLiteral("message")).toString();
        setOnline(false);
        setStatus(msg.isEmpty() ? QStringLiteral("connection error") : msg);
        if (m_activePeerLabel.isEmpty())
            releaseTurnForLostWire(m_currentSessionId);
        if (m_testPending) {
            m_testPending = false;
            emit connectionTested(false, msg.isEmpty() ? QStringLiteral("Connection failed.") : msg);
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

void Manager::coreOnSessions(const QString &json)
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
        m["snippet"] = o.value("last_message_snippet").toString();
        m["projectId"] = projectKey(o.value("project_id").toString());
        m["messageCount"] = o.value("message_count").toVariant();
        m["hasRecipe"] = o.value("has_recipe").toBool();
        m["archived"] = o.value("archived").toBool();
        m["peer"] = m_activePeerLabel;
        sessions << m;
    }
    onSessions(sessions);
}

void Manager::coreOnTranscript(const QString &json)
{
    QJsonObject root = parseObj(json);
    if (root.contains(QStringLiteral("Clear"))) {
        m_messageModel->clear();
        m_currentIndex = -1;
        requestMessagesUpdate();
        return;
    }
    QString tag = root.contains(QStringLiteral("Append")) ? QStringLiteral("Append")
                : root.contains(QStringLiteral("Update")) ? QStringLiteral("Update") : QString();
    if (tag.isEmpty())
        return;
    const QJsonObject message = root.value(tag).toObject().value(QStringLiteral("message")).toObject();
    const QString role = message.value(QStringLiteral("role")).toString();
    // Tool rows are rendered from on_stream (rich title/output/status); the
    // transcript's tool projection (title-only) would duplicate them.
    if (role == QStringLiteral("tool"))
        return;
    const QString content = message.value(QStringLiteral("content")).toString();
    const QString output = message.value(QStringLiteral("output")).toString();
    const QString messageId = message.value(QStringLiteral("id")).toString();

    QVariantMap row;
    row["id"] = messageId;
    row["role"] = role;
    row["text"] = content;
    row["output"] = output;
    if (role == QStringLiteral("thought")) {
        row["thought"] = true;
    } else if (role == QStringLiteral("error")) {
        row["html"] = QStringLiteral("<div>") + content + QStringLiteral("</div>");
    } else {
        row["html"] = markdownToHtml(content);
    }

    int idx = -1;
    for (int i = m_messageModel->count() - 1; i >= 0; --i) {
        if (m_messageModel->row(i).value("id").toString() == messageId
            && m_messageModel->row(i).value("role").toString() == role) {
            idx = i;
            break;
        }
    }
    if (tag == QStringLiteral("Append")) {
        // The store's arrival order is the truth: append unconditionally (Android's
        // appendFromMessage). The (id, role) lookup would collapse a fresh bubble
        // into an older one — live thought bubbles all carry an empty id, so every
        // second-turn thought used to overwrite the first row in place.
        m_messageModel->append(row);
    } else { // Update
        // Only Updates map to an existing row: a non-empty message id matches by
        // id+role; an empty-id live bubble matches the LAST row of the same role
        // (Android's updateFromMessage rule — roaming interleaves agent/thought
        // on the same empty stream).
        if (idx < 0)
            m_messageModel->append(row);
        else
            m_messageModel->update(idx, row);
    }
    m_currentIndex = m_messageModel->count() - 1;
    requestMessagesUpdate();
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

void Manager::coreOnPermission(const QString &json)
{
    const QJsonObject o = parseObj(json);
    const QJsonObject req = o.value(QStringLiteral("PermissionRequest")).isObject()
        ? o.value(QStringLiteral("PermissionRequest")).toObject() : o;
    QString toolCallId = req.value("tool_call_id").toString();
    // serde external tag puts the variant under the root; unwrap if needed.
    if (toolCallId.isEmpty()) {
        // Root may be the untagged object already.
    }
    QVariantList options;
    for (const auto &opt : req.value("options").toArray()) {
        const QJsonObject oo = opt.toObject();
        options << QVariantMap{{"option_id", oo.value("option_id").toString()},
                               {"name", oo.value("name").toString()},
                               {"kind", oo.value("kind").toString()}};
    }
    onPermission(toolCallId, req.value("title").toString(),
                 req.value("detail").toString(), options);
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
    const QJsonObject ctx{{QStringLiteral("app_visible"), Notifier::appVisible()},
                          {QStringLiteral("armed_session"), QJsonValue::Null},
                          {QStringLiteral("session_title"), m_currentSessionTitle},
                          {QStringLiteral("announce_any_turn"), true}};
    const QJsonObject d = parseObj(pushDecide(
        QString::fromUtf8(QJsonDocument(envelope).toJson(QJsonDocument::Compact)),
        QString::fromUtf8(QJsonDocument(ctx).toJson(QJsonDocument::Compact))));
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
        sessions << QVariantMap{{"sessionId", o.value("id").toString()},
                                {"title", o.value("title").toString()},
                                {"updatedAt", o.value("updated_at").toString()},
                                {"peer", label}};
    }
    m_roamModel->setPeerSessions(label, sessions);
}

void Manager::coreOnPeerNewSession(const QString &label, const QString &sid)
{
    refreshSessions();
    if (label == m_activePeerLabel) {
        m_currentSessionId = sid;
        m_currentSessionTitle.clear();
        emit currentSessionChanged();
    }
}

void Manager::coreOnActiveRun(const QString &sid, const QString &runId)
{
    onActiveRunChanged(sid, runId);
}

void Manager::coreOnCommands(const QString &json)
{
    QStringList commands;
    for (const auto &c : parseArr(json))
        commands << c.toString();
    onCommands(commands);
}

void Manager::coreOnExport(const QString &data)
{
    onExportResult(data);
}

void Manager::coreOnRecipeParams(const QString &) {}
void Manager::coreOnElicitation(const QString &) {}

void Manager::coreOnCompactionStatus(const QString &message)
{
    onCompactionStatus(message);
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

void Manager::coreOnProviders(const QString &json)
{
    // goose's inventory of providers, each with a `configured` flag: the authority on
    // which are usable (the app used to carry a hardcoded list that drifted).
    QStringList configured;
    for (const auto &el : parseArr(json)) {
        const QJsonObject o = el.toObject();
        const QString id = o.value("providerId").toString();
        if (!id.isEmpty() && o.value("configured").toBool())
            configured << id;
    }
    // An empty parse must not empty the pickers: a server that doesn't answer this
    // leaves the previous list (and configured-providers-only degrades to "show all").
    if (configured.isEmpty() && !m_configuredProviders.isEmpty())
        return;
    m_configuredProviders = configured;
    emit providersChanged();
}

void Manager::refreshProviders()
{
    if (m_bridge && m_bridge->isAvailable())
        m_bridge->api().grouse_unstable_providers_list(m_bridge->handle());
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
    Q_UNUSED(method);
    onError(message, false);
}

// ---------------------------------------------------------------------------
// Streaming (on_stream): tool/chart/MCP-App bubbles + usage + run-ended.
// Text bubbles are rendered via on_transcript (see coreOnTranscript).
// ---------------------------------------------------------------------------

void Manager::coreOnStream(const QString &json)
{
    const QJsonObject root = parseObj(json);
    if (root.contains(QStringLiteral("AgentChunk"))
        || root.contains(QStringLiteral("UserChunk"))
        || root.contains(QStringLiteral("ThoughtChunk")))
        return; // text handled by on_transcript
    if (root.contains(QStringLiteral("ToolCall"))) {
        const QJsonObject o = root.value("ToolCall").toObject();
        const QJsonObject kind = o.value("kind").toObject();
        const QString title = o.value("title").toString();
        const QString id = o.value("tool_call_id").toString();
        if (kind.contains(QStringLiteral("Chart"))) {
            onChartToolCall(title, id, kind.value("Chart").toObject().value("spec").toString());
        } else if (kind.contains(QStringLiteral("McpApp"))) {
            const QJsonObject m = kind.value("McpApp").toObject();
            onMcpAppToolCall(title, id,
                             QStringLiteral("%1|%2").arg(m.value("app_key").toString(), m.value("uri").toString()),
                             m.value("uri").toString(), m.value("extension").toString(),
                             m.value("input").toString());
        } else {
            onToolCall(title, o.value("detail").toString(), id);
        }
    } else if (root.contains(QStringLiteral("ToolCallUpdate"))) {
        const QJsonObject o = root.value("ToolCallUpdate").toObject();
        onToolCallUpdate(o.value("id").toString(), o.value("status").toString(),
                         o.value("output").toString(), o.value("live").toBool());
    } else if (root.contains(QStringLiteral("Usage"))) {
        const QJsonObject o = root.value("Usage").toObject();
        onUsage(int(o.value("used").toDouble()), int(o.value("size").toDouble()),
                o.value("cost").toDouble(), o.value("currency").toString());
    } else if (root.contains(QStringLiteral("RunEnded"))) {
        m_prompting = false;
        m_promptingSessionId.clear();
        m_compacting = false;
        setActiveRunId(QString());
        emit promptingChanged();
        emit compactingChanged();
        flushQueue();
        notifyTurnFinished();
    }
}

// ---------------------------------------------------------------------------
// Model-update handlers (called by the coreOn* entry points above)
// ---------------------------------------------------------------------------

void Manager::appendChunk(const QString &role, const QString &text, const QString &messageId, bool thought)
{
    Q_UNUSED(role); Q_UNUSED(text); Q_UNUSED(messageId); Q_UNUSED(thought);
    // Text is rendered via on_transcript; retained for API compatibility.
}

void Manager::finalizeCurrentMessage()
{
    // Render markdown for completed agent/user bubbles (on_transcript already
    // emits html; nothing further needed).
}

void Manager::onAgentChunk(const QString &text, const QString &messageId)
{
    Q_UNUSED(text); Q_UNUSED(messageId);
}

void Manager::onUserChunk(const QString &text, const QString &messageId)
{
    Q_UNUSED(text); Q_UNUSED(messageId);
}

void Manager::onThoughtChunk(const QString &text)
{
    Q_UNUSED(text);
}

void Manager::onToolCall(const QString &title, const QString &detail, const QString &toolCallId)
{
    m_messageModel->append(QVariantMap{{"id", m_seq++}, {"role", "tool"}, {"text", ""}, {"html", ""},
                                       {"title", title}, {"detail", detail}, {"output", ""},
                                       {"status", "in_progress"}, {"toolCallId", toolCallId}});
    m_currentIndex = -1;
    requestMessagesUpdate();
}

void Manager::onToolCallUpdate(const QString &toolCallId, const QString &status,
                               const QString &output, bool live)
{
    for (int i = m_messageModel->count() - 1; i >= 0; --i) {
        QVariantMap m = m_messageModel->row(i);
        const QString role = m.value("role").toString();
        if ((role == "tool" || role == "chart" || role == "mcpapp")
            && m.value("toolCallId").toString() == toolCallId) {
            m["status"] = status;
            if ((role == "tool" || role == "mcpapp") && !output.isEmpty())
                m["output"] = (live ? m.value("output").toString() : QString()) + output;
            m_messageModel->update(i, m);
            break;
        }
    }
    requestMessagesUpdate();
}

void Manager::onChartToolCall(const QString &title, const QString &toolCallId, const QString &chartSpec)
{
    m_messageModel->append(QVariantMap{{"id", m_seq++}, {"role", "chart"}, {"text", ""},
                                       {"title", title}, {"chartData", chartSpec},
                                       {"toolCallId", toolCallId}, {"status", "in_progress"}});
    m_currentIndex = -1;
    requestMessagesUpdate();
}

void Manager::onMcpAppToolCall(const QString &title, const QString &toolCallId, const QString &appKey,
                               const QString &appUri, const QString &appExt, const QString &appInput)
{
    // Late hydration: the creation frame announced a Plain call (a chip row was
    // appended then), and the core re-issues this ToolCall with the app kind when
    // the completing update carries `goose.mcpApp`. Convert the existing chip in
    // place so the transcript shows ONE row, not chip + app.
    bool converted = false;
    for (int i = m_messageModel->count() - 1; i >= 0 && !converted; --i) {
        QVariantMap m = m_messageModel->row(i);
        if (m.value("role").toString() == QLatin1String("tool")
            && m.value("toolCallId").toString() == toolCallId) {
            m["role"] = QStringLiteral("mcpapp");
            m["appKey"] = appKey;
            m["appHtml"] = QString();
            if (!appInput.isEmpty())
                m["detail"] = appInput;
            m_messageModel->update(i, m);
            converted = true;
        }
    }
    if (!converted) {
        m_messageModel->append(QVariantMap{{"id", m_seq++}, {"role", "mcpapp"}, {"text", ""},
                                           {"title", title}, {"detail", appInput}, {"appKey", appKey},
                                           {"appHtml", QString()}, {"toolCallId", toolCallId},
                                           {"status", "in_progress"}});
    }
    m_currentIndex = -1;
    if (m_bridge && m_bridge->isAvailable()) {
        const QByteArray sid = m_currentSessionId.toUtf8();
        const QByteArray uri = appUri.toUtf8();
        const QByteArray ext = appExt.toUtf8();
        m_bridge->api().grouse_unstable_resources_read(m_bridge->handle(), sid.constData(),
                                                       uri.constData(), ext.constData());
    }
    requestMessagesUpdate();
}

void Manager::onAppResource(const QString &appKey, const QString &html)
{
    if (!html.isEmpty())
        m_appHtml.insert(appKey, html);
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

void Manager::openAppInHtml(const QString &appKey)
{
    const QString html = m_appHtml.value(appKey);
    if (html.isEmpty())
        return;
    const QString path = QDir(QStandardPaths::writableLocation(QStandardPaths::TempLocation))
                             .filePath(QStringLiteral("grouse-app-%1.html")
                                           .arg(qHash(appKey)));
    QFile f(path);
    if (!f.open(QIODevice::WriteOnly | QIODevice::Truncate))
        return;
    f.write(html.toUtf8());
    f.close();
    // Same trust boundary as the in-app renderer: the document is server-supplied;
    // the browser sandbox is the isolation. One-shot view of a snapshot.
    QDesktopServices::openUrl(QUrl::fromLocalFile(path));
}
void Manager::onCompactionStatus(const QString &message)
{
    const QString m = message.toLower();
    if (m.contains(QLatin1String("compact")))
        m_compacting = true;
    if (m.contains(QLatin1String("complete")) || m.contains(QLatin1String("error")))
        m_compacting = false;
    emit compactingChanged();
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
    if (sessionId == m_currentSessionId || sessionId.isEmpty())
        setActiveRunId(runId);
    else
        setActiveRunId(QString());
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
    emit toolsChanged();
}

void Manager::onExtensions(const QVariantList &extensions)
{
    m_extDefs.clear();
    for (const auto &v : extensions) {
        const QVariantMap m = v.toMap();
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
        m_extDefs << d;
    }
    publishToolGroups();
    saveToolCache(m_currentSessionId);
    emit globalExtensionsChanged();
    emit toolsChanged();
}

void Manager::onSessionExtensions(const QStringList &names, const QSet<QString> &restricted)
{
    m_sessionExts = names;
    m_sessionRestricted = restricted;
    // Deferred peek commit (see onTools): the re-list that always follows a
    // session/extensions/add decides whether the peeked row really attached.
    if (!m_discoveringExt.isEmpty()) {
        const QString target = m_discoveringExt;
        const bool attachedNow = names.contains(target);
        const bool wasAttached = m_discoveringAttached;
        m_discoveringExt.clear();
        m_peekQueue.removeAll(target);       // this row is handled (or was repaired)
        if (!m_peekQueue.isEmpty())
            m_peekTimer->start(400);
        if (attachedNow) {
            m_toolCatalog[target] = m_discoveringFull;
            if (wasAttached) {
                // Put the session's real restriction back (empty allowlist = all).
                const ExtDef *d = extDef(target);
                const QStringList allowed = d ? d->availableTools : QStringList();
                setSessionTools(target, allowed.isEmpty() ? m_discoveringFull : allowed);
            } else {
                // It was detached before the peek — detach it again; its tool
                // list is now known without having paid the context cost.
                m_sessionExts.removeAll(target);
                m_sessionRestricted.remove(target);
                if (m_bridge && m_bridge->isAvailable()) {
                    const QByteArray sid = m_currentSessionId.toUtf8();
                    m_bridge->api().grouse_unstable_session_extensions_remove(
                        m_bridge->handle(), sid.constData(), target.toUtf8().constData());
                    // The peek's tools reply left the unfiltered set in m_tools
                    // and remove re-lists nothing — re-pull the truth.
                    m_bridge->api().grouse_unstable_list_tools(m_bridge->handle(), sid.constData());
                }
            }
        } else if (wasAttached) {
            // The peek's add failed (extension won't start here) but the peek
            // had to remove it first — repair by re-adding the real profile.
            const ExtDef *d = extDef(target);
            if (d && m_bridge && m_bridge->isAvailable()) {
                const QByteArray sid = m_currentSessionId.toUtf8();
                m_bridge->api().grouse_unstable_session_extensions_add(
                    m_bridge->handle(), sid.constData(),
                    QJsonDocument(d->raw).toJson(QJsonDocument::Compact).constData());
            }
        }
        if (!attachedNow) {
            // A peek that never attached means the extension would not start —
            // stop THIS session's sweep from re-dialing a dead endpoint every
            // pass (one failed client-init per chat was the error-bubble bug).
            // Manual clicks bypass the memo; a fresh chat gets one retry.
            m_peekFailed.insert(target);
        }
        m_discoveringFull.clear();
        m_discoveringAttached = false;
        persistCatalogs();
    }
    buildPeekQueue();
    publishToolGroups();
    saveToolCache(m_currentSessionId);
}

// The background sweep: learn every extension's tool list once, opportunistically,
// while the chat is doing nothing. Each step is the same transient attach→list→
// detach the manual peek arrow performs (a detached row never stays attached), so
// the drawer can print "# tools" on EVERY row — including rows nobody expanded —
// without permanently inflating the session's context. Persisted to QSettings, so
// a server with N extensions costs N one-off peeks per install, not per session.
void Manager::buildPeekQueue()
{
    if (!m_peekQueue.isEmpty() || !m_discoveringExt.isEmpty())
        return;                              // a sweep or a manual peek is running
    if (m_currentSessionId.isEmpty() || !m_activePeerLabel.isEmpty())
        return;                              // only the local chat can host a peek
    for (const auto &d : std::as_const(m_extDefs))
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

void Manager::doPeekStep()
{
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

void Manager::persistCatalogs()
{
    QJsonObject o;
    for (auto it = m_toolCatalog.constBegin(); it != m_toolCatalog.constEnd(); ++it) {
        QJsonArray a;
        for (const auto &t : it.value())
            a.append(t);
        o.insert(it.key(), a);
    }
    m_store.setValue(QStringLiteral("tool_catalogs"),
                     QString::fromUtf8(QJsonDocument(o).toJson(QJsonDocument::Compact)));
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
    m_skills = skills;
    emit skillsChanged();
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
    QVariantList out;
    const QSet<QString> active(m_tools.constBegin(), m_tools.constEnd());
    const QSet<QString> enabled(m_sessionExts.constBegin(), m_sessionExts.constEnd());
    // Every identity here is the extension KEY; `name` is carried per group for display.
    QStringList allKeys;
    for (const auto &d : m_extDefs)
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
