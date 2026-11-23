// SPDX-License-Identifier: AGPL-3.0-or-later

#include "coreapi.h"
#include "manager.h"

#include <QDateTime>
#include <QDebug>
#include <QDir>
#include <QFile>
#include <QJsonArray>
#include <QJsonDocument>
#include <QRegularExpression>
#include <QStandardPaths>

namespace {
QJsonObject parseObj(const QString &json)
{
    return QJsonDocument::fromJson(json.toUtf8()).object();
}
} // namespace

QString CoreApi::timingLogPath()
{
    const QString dir = QStandardPaths::writableLocation(QStandardPaths::AppDataLocation);
    if (dir.isEmpty())
        return {};
    QDir().mkpath(dir);
    return dir + QStringLiteral("/open-timings.log");
}

void CoreApi::timingLog(const QString &line)
{
    qInfo().noquote() << line;
    const QString path = timingLogPath();
    if (path.isEmpty())
        return;
    // Bounded, and rewritten rather than grown without limit: this file exists to be
    // read after a slow click, not to be an archive.
    QFile f(path);
    if (f.exists() && f.size() > 256 * 1024) {
        f.remove();
    }
    if (!f.open(QIODevice::Append | QIODevice::Text))
        return;
    f.write(QDateTime::currentDateTime().toString(QStringLiteral("HH:mm:ss.zzz")).toUtf8());
    f.write(" ");
    f.write(line.toUtf8());
    f.write("\n");
}

QVariantMap CoreApi::pinMapFromJson(const QString &raw)
{
    if (raw.trimmed().isEmpty())
        return {};
    const QJsonDocument doc = QJsonDocument::fromJson(raw.toUtf8());
    if (doc.isObject())
        return doc.object().toVariant().toMap();
    // lib.rs's roam_card_pins() already returns a JSON string; some C ABI
    // spellings hand that string back as a JSON string value rather than an
    // object. Unwrap once, never parse a peer-supplied value.
    if (doc.isString())
        return pinMapFromJson(doc.string());
    return {};
}

QString CoreApi::pinStringFromJson(const QString &raw)
{
    if (raw.trimmed().isEmpty() || raw.trimmed() == QLatin1String("null"))
        return QString();
    const QJsonDocument doc = QJsonDocument::fromJson(raw.toUtf8());
    return doc.isString() ? doc.string() : (doc.isObject() ? QString() : raw.trimmed());
}

QVariantMap CoreApi::parsePermissionRequest(const QString &json)
{
    const QJsonObject root = parseObj(json);
    const QJsonObject req = root.value(QStringLiteral("PermissionRequest")).isObject()
        ? root.value(QStringLiteral("PermissionRequest")).toObject() : root;
    const QString toolCallId = req.value("tool_call_id").toString(req.value("toolCallId").toString());
    const QString requestId = req.value("request_id").toString(
        req.value("requestId").toString(toolCallId));
    const QString sessionId = req.value("session_id").toString(req.value("sessionId").toString());
    QVariant owner = req.value("owner").toVariant();
    // Some C ABI spellings pass SessionOwner as a JSON *string*, not an object.
    if (owner.userType() == QMetaType::QString && owner.toString().trimmed().startsWith(QLatin1Char('{')))
        owner = QJsonDocument::fromJson(owner.toString().trimmed().toUtf8()).object().toVariant();

    QVariantList options;
    for (const auto &opt : req.value("options").toArray()) {
        const QJsonObject oo = opt.toObject();
        options << QVariantMap{{"option_id", oo.value("option_id").toString()},
                               {"name", oo.value("name").toString()},
                               {"kind", oo.value("kind").toString()}};
    }
    return {{"requestId", requestId},
            {"toolCallId", toolCallId},
            {"sessionId", sessionId},
            {"owner", owner},
            {"ownerLabel", requestOwnerLabel(owner, sessionId)},
            {"title", req.value("title").toString()},
            {"detail", req.value("detail").toString()},
            {"options", options}};
}

int CoreApi::requestQueueIndex(const QVariantList &queue, const QString &requestId)
{
    if (requestId.isEmpty())
        return -1;                 // never answer an ambiguous id
    for (int i = 0; i < queue.size(); ++i) {
        const QVariantMap entry = queue.at(i).toMap();
        QString id = entry.value("requestId").toString();
        if (id.isEmpty())
            id = entry.value("toolCallId").toString();
        if (id == requestId)
            return i;
    }
    return -1;
}

QString CoreApi::requestOwnerLabel(const QVariant &owner, const QString &sessionId)
{
    QString label;
    QString kind;
    if (owner.userType() == QMetaType::QVariantMap) {
        const QVariantMap o = owner.toMap();
        label = o.value("label").toString();
        kind = o.value("kind").toString();
    } else if (owner.isValid() && !owner.isNull()) {
        label = owner.toString();
    }
    if (!label.isEmpty()) {
        if (label.startsWith(QLatin1String("roam:")))
            return label;
        if (kind == QLatin1String("roam"))
            return QStringLiteral("roam:") + label;
        // The core sends a BARE roam label in PermissionRequest::owner. Pair it
        // with the display id so the dialog names both the wire and the chat.
        if (sessionId.startsWith(QLatin1String("roam:"))) {
            const QString peer = Manager::roamPeerForSession(sessionId);
            if (!peer.isEmpty() && label == peer)
                return QStringLiteral("roam:") + peer;
        }
        return sessionId.startsWith(label + QLatin1Char(':'))
            ? QStringLiteral("roam:") + label : label;
    }
    if (sessionId.isEmpty())
        return QString();
    if (sessionId.startsWith(QLatin1String("roam:"))) {
        const QString peer = Manager::roamPeerForSession(sessionId);
        return peer.isEmpty() ? sessionId : QStringLiteral("roam:") + peer;
    }
    return sessionId;
}

QVariantList CoreApi::formatCardPins(const QVariantMap &pins)
{
    QVariantList rows;
    for (auto it = pins.constBegin(); it != pins.constEnd(); ++it) {
        const QString peer = it.key().trimmed();
        const QString fingerprint = it.value().toString().trimmed();
        if (peer.isEmpty() || fingerprint.isEmpty())
            continue;
        const QString shortFp = fingerprint.length() > 16
            ? fingerprint.left(12) + QStringLiteral("…") + fingerprint.right(4)
            : fingerprint;
        rows << QVariantMap{{"peer", peer},
                            {"fingerprint", fingerprint},
                            {"short", shortFp}};
    }
    return rows;
}

QVariantMap CoreApi::parseCardPinRefusal(const QString &status, const QString &peerLabel)
{
    const int at = status.indexOf(QLatin1String("roam card pin refused"), Qt::CaseInsensitive);
    if (at < 0)
        return {};
    const QString message = status.mid(at).trimmed();
    // Core wording: "roam card for '<label>' changed: pinned <fp>, got <fp>.
    // Refusing to dial — clear the pin to re-enroll this peer".
    const QRegularExpression re(QStringLiteral(
        "roam card for '([^']+)' changed: pinned ([^,]+), got ([^.]+)"));
    const QRegularExpressionMatch m = re.match(message);
    QString peer = peerLabel.trimmed();
    if (peer.isEmpty())
        peer = m.captured(1).trimmed();
    QVariantMap out{{"message", message},
                    {"peer", peer},
                    {"pinned", m.captured(2).trimmed()},
                    {"got", m.captured(3).trimmed()}};
    return out;
}

QString CoreApi::promptJson(const QString &text, const QVariantList &blocks)
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

QString CoreApi::serverConfigJson(Manager &manager)
{
    QJsonObject o;
    o["host"] = manager.host().trimmed();
    o["port"] = manager.port().trimmed().toInt();
    o["secret_key"] = manager.secretKey();
    o["use_tls"] = manager.useTls();
    // User-controlled: self-signed tailnet hosts need an explicit opt-out from
    // WebPKI verification, but trust-all must not be a hidden constant.
    o["accept_invalid_certs"] = manager.acceptInvalidCerts();
    o["ca_cert_pem"] = QJsonValue::Null;
    o["cwd"] = manager.workingDir();
    o["auto_connect"] = true;
    o["client_id"] = QStringLiteral("grouse-desktop");
    o["initial_recipe_id"] = manager.m_pendingRecipeId.isEmpty()
        ? QJsonValue::Null : QJsonValue(manager.m_pendingRecipeId);
    manager.m_pendingRecipeId.clear();
    return QString::fromUtf8(QJsonDocument(o).toJson(QJsonDocument::Compact));
}

void CoreApi::reportError(const QString &method, const QString &message)
{
    // The METHOD is what identifies the failing call in a bug report; the UI
    // shows the message alone. Dropping it here is why a session-id failure took
    // a server-side probe to attribute (grouse#20).
    qWarning("grouse: core error from %s: %s",
             qUtf8Printable(method), qUtf8Printable(message));
}
