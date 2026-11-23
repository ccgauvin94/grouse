#include "dbusadapter.h"

#include "manager.h"

#include <QDBusConnection>
#include <QDBusConnectionInterface>
#include <QDBusError>
#include <QFile>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>

#include <unistd.h>

namespace {

// /proc is readable inside the Flatpak for another process of the same user.
// A peer with no readable XDG_SESSION_ID is left to the UID check; one that
// names a different login session is not this desktop's KRunner.
QByteArray sessionIdOf(qint64 pid)
{
    if (pid <= 0)
        return {};
    QFile environ(QStringLiteral("/proc/%1/environ").arg(pid));
    if (!environ.open(QIODevice::ReadOnly))
        return {};
    const QByteArray vars = environ.readAll();
    const QByteArray key = QByteArrayLiteral("XDG_SESSION_ID=");
    for (const QByteArray &entry : vars.split('\0')) {
        if (entry.startsWith(key))
            return entry.mid(key.size());
    }
    return {};
}

} // namespace

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

bool DbusAdapter::callerAuthorized() const
{
    // No DBus caller (a direct C++ call from tests) has no credentials to check.
    if (!calledFromDBus())
        return true;

    const QDBusConnection bus = connection();
    const QString sender = message().service();
    if (sender.isEmpty() || !bus.isConnected())
        return false;
    // Our own process, e.g. the app's own bus-activated path.
    if (sender == bus.baseService())
        return true;

    QDBusConnectionInterface *iface = bus.interface();
    if (!iface)
        return false;
    // Credentials come from the bus driver, not from the caller's message.
    const QDBusCredentials creds = iface->serviceCredentials(sender);
    if (creds.isNull() || creds.pid() <= 0 || creds.uid() != ::getuid())
        return false;

    const QByteArray ownSession = qgetenv("XDG_SESSION_ID");
    if (!ownSession.isEmpty()) {
        const QByteArray peerSession = sessionIdOf(creds.pid());
        if (!peerSession.isEmpty() && peerSession != ownSession)
            return false;
    }
    return true;
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
    if (!callerAuthorized()) {
        sendErrorReply(QDBusError::AccessDenied,
                       QStringLiteral("Only this desktop and same-user session peers may open chats"));
        return;
    }
    if (!sessionId.isEmpty())
        m_mgr->openSession(sessionId);
}

void DbusAdapter::NewChat()
{
    if (!callerAuthorized()) {
        sendErrorReply(QDBusError::AccessDenied,
                       QStringLiteral("Only this desktop and same-user session peers may open chats"));
        return;
    }
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
