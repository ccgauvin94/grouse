// SPDX-License-Identifier: AGPL-3.0-or-later

#include "manager.h"

#include "corebridge.h"
#include "markdown.h"
#include "messagelistmodel.h"
#include "roamlistmodel.h"
#include "sessionlistmodel.h"

#include <QDir>
#include <QFile>
#include <QFileDialog>
#include <QFileInfo>
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
    return m_currentSessionId.isEmpty() ? QStringLiteral("New chat") : m_currentSessionId;
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
    if (label.isEmpty()) {
        setStatus(QStringLiteral("roam: label required"));
        return;
    }
    if (m_bridge && m_bridge->isAvailable()) {
        const QByteArray c = card.toUtf8();
        const QByteArray l = label.toUtf8();
        m_bridge->api().grouse_roam_connect(m_bridge->handle(), c.constData(), l.constData());
        m_roamModel->addPeer(label);
    }
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
    m_toolCatalog.clear();
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
    m_toolCatalog.clear();
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
    m_toolCatalog.clear();
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
    if (ready && !m_prompting) {
        m_prompting = true;
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
    } else {
        // Not ready or a turn is already running: queue (the core flushes the
        // prompt queue itself; this app-level queue only waits for ready()).
        enqueue({text, blocks});
        if (!ready && !secretKey().isEmpty())
            connectToServer();
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

void Manager::deleteSkill(const QString &path)
{
    if (!m_bridge || !m_bridge->isAvailable() || path.isEmpty())
        return;
    const QByteArray type = QByteArrayLiteral("skill");
    const QByteArray p = path.toUtf8();
    m_bridge->api().grouse_unstable_sources_delete(m_bridge->handle(), type.constData(), p.constData());
}

// ---- server config (providers) ---------------------------------------------

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
    const QByteArray sid = m_currentSessionId.toUtf8();
    m_bridge->api().grouse_unstable_list_global_extensions(m_bridge->handle());
    m_bridge->api().grouse_unstable_list_tools(m_bridge->handle(), sid.constData());
}

void Manager::discoverToolGroup(const QString &extName)
{
    const ExtDef *d = extDef(extName);
    if (!d || m_toolCatalog.contains(extName) || !m_bridge || !m_bridge->isAvailable())
        return;
    QJsonObject unfiltered = d->raw;
    unfiltered.insert("available_tools", QJsonArray());
    m_discoveringExt = extName;
    const QByteArray sid = m_currentSessionId.toUtf8();
    const QByteArray ext = QJsonDocument(unfiltered).toJson(QJsonDocument::Compact);
    m_bridge->api().grouse_unstable_session_extensions_remove(m_bridge->handle(), sid.constData(),
                                                              d->name.toUtf8().constData());
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
        m_bridge->api().grouse_unstable_session_extensions_add(
            m_bridge->handle(), sid.constData(),
            QJsonDocument(d->raw).toJson(QJsonDocument::Compact).constData());
    } else {
        m_sessionExts.removeAll(extName);
        m_bridge->api().grouse_unstable_session_extensions_remove(
            m_bridge->handle(), sid.constData(), extName.toUtf8().constData());
    }
    publishToolGroups();
    saveToolCache(m_currentSessionId);
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
        group["name"] = d.name;
        group["type"] = d.type;
        group["attrib"] = d.attrib;
        group["enabled"] = d.enabled;
        QVariantList tools;
        const QString prefix = d.name + QStringLiteral("__");
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
    if (m_bridge && m_bridge->isAvailable())
        m_bridge->api().grouse_unstable_list_global_extensions(m_bridge->handle());
}

void Manager::setGlobalExtensionEnabled(const QString &extName, bool enabled)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    const ExtDef *d = extDef(extName);
    if (!d)
        return;
    m_bridge->api().grouse_unstable_set_extension_enabled(
        m_bridge->handle(), extName.toUtf8().constData(), enabled ? 1 : 0);
}

void Manager::setGlobalToolEnabled(const QString &extName, const QString &toolName, bool on)
{
    const ExtDef *d = extDef(extName);
    if (!d)
        return;
    const QString prefix = extName + QStringLiteral("__");
    QSet<QString> current(d->availableTools.constBegin(), d->availableTools.constEnd());
    if (on == current.contains(prefix + toolName))
        return;
    if (on) current << (prefix + toolName);
    else current.remove(prefix + toolName);
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

const Manager::ExtDef *Manager::extDef(const QString &name) const
{
    for (const auto &d : m_extDefs)
        if (d.name == name)
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
    for (const auto &t : list)
        arr.append(t);
    scoped.insert("available_tools", arr);
    m_discoveringExt.clear();
    const QByteArray sid = m_currentSessionId.toUtf8();
    m_bridge->api().grouse_unstable_session_extensions_remove(
        m_bridge->handle(), sid.constData(), extName.toUtf8().constData());
    m_bridge->api().grouse_unstable_session_extensions_add(
        m_bridge->handle(), sid.constData(),
        QJsonDocument(scoped).toJson(QJsonDocument::Compact).constData());
}

void Manager::publishToolGroups()
{
    emit toolGroupsChanged();
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
        if (m_testPending) {
            m_testPending = false;
            emit connectionTested(true, QStringLiteral("Connection OK — the server responded."));
        }
        flushQueue();
        refreshSessions();
        refreshProjects();
        refreshRecipes();
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
        if (m_testPending) {
            m_testPending = false;
            emit connectionTested(false, msg.isEmpty() ? QStringLiteral("Connection failed.") : msg);
        }
    }
}

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
        m["projectId"] = o.value("project_id").toString();
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
        if (idx < 0) {
            m_messageModel->append(row);
        } else {
            m_messageModel->update(idx, row);
        }
    } else { // Update
        if (idx >= 0)
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

void Manager::coreOnSessionTouched(const QString &sid, const QString &title, const QString &u)
{
    Q_UNUSED(sid); Q_UNUSED(title); Q_UNUSED(u);
    // The core performs its own debounced resync of the active session. The UI
    // only needs to refresh the sidebar so order/title/status reflect the touch.
    refreshSessions();
}

void Manager::coreOnProjects(const QString &json)
{
    QVariantList projects;
    const QJsonArray arr = parseArr(json);
    for (const auto &el : arr) {
        const QJsonObject o = el.toObject();
        projects << QVariantMap{{"id", o.value("path").toString()},
                                {"name", o.value("name").toString()},
                                {"path", o.value("path").toString()},
                                {"description", o.value("description").toString()}};
    }
    onProjects(projects);
}

void Manager::coreOnRoamPeerStatus(const QString &label, const QString &status)
{
    const bool down = status == QStringLiteral("disconnected") || status.startsWith(QStringLiteral("error:"));
    m_roamModel->setPeerStatus(label, status, !down);
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
    Q_UNUSED(sid);
    QStringList names;
    for (const auto &el : parseArr(json)) {
        if (el.isObject())
            names << el.toObject().value("name").toString();
        else
            names << el.toString();
    }
    onSessionExtensions(names);
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

void Manager::coreOnProviders(const QString &)
{
    // Provider inventory is server-authoritative; currently not surfaced in the
    // desktop UI beyond the model list handled above.
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
        m_compacting = false;
        m_activeRunId.clear();
        emit promptingChanged();
        emit compactingChanged();
        flushQueue();
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
    m_messageModel->append(QVariantMap{{"id", m_seq++}, {"role", "mcpapp"}, {"text", ""},
                                       {"title", title}, {"detail", appInput}, {"appKey", appKey},
                                       {"appHtml", QString()}, {"toolCallId", toolCallId},
                                       {"status", "in_progress"}});
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
    saveToolCache(m_currentSessionId);
    emit globalExtensionsChanged();
    emit toolsChanged();
}

void Manager::onSessionExtensions(const QStringList &names)
{
    m_sessionExts = names;
    publishToolGroups();
    saveToolCache(m_currentSessionId);
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
    QStringList allNames;
    for (const auto &d : m_extDefs)
        allNames << d.name;
    for (const auto &n : m_sessionExts)
        if (!allNames.contains(n))
            allNames << n;

    // Some goose versions expose tools before extension profiles: group the
    // active names directly so the panel stays useful.
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
