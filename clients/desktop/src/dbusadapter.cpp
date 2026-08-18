#include "dbusadapter.h"

#include "manager.h"

#include <QDBusConnection>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>

DbusAdapter::DbusAdapter(Manager *mgr, QObject *parent)
    : QObject(parent)
    , m_mgr(mgr)
{
    QDBusConnection bus = QDBusConnection::sessionBus();
    if (!bus.isConnected())
        return;
    if (!bus.registerService(QStringLiteral("id.gauvin.Grouse")))
        return;   // another instance already owns the name
    m_registered = bus.registerObject(QStringLiteral("/id/gauvin/Grouse"), this,
                                      QDBusConnection::ExportAllSlots);
}

QString DbusAdapter::ListSessions() const
{
    QJsonArray out;
    const QVariantList sessions = m_mgr->sessions().toList();
    for (const auto &v : sessions) {
        const QVariantMap session = v.toMap();
        if (!session.value(QStringLiteral("sessionId")).toString().isEmpty())
            out.append(QJsonObject::fromVariantMap(session));
    }
    return QString::fromUtf8(QJsonDocument(out).toJson(QJsonDocument::Compact));
}

void DbusAdapter::OpenSession(const QString &sessionId)
{
    if (!sessionId.isEmpty())
        m_mgr->openSession(sessionId);
}

void DbusAdapter::NewChat()
{
    m_mgr->newChat();
}

bool DbusAdapter::Online() const
{
    return m_mgr->online();
}

QString DbusAdapter::Status() const
{
    return m_mgr->status();
}
