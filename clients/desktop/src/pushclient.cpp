#include "pushclient.h"

#include "manager.h"
#include "notifier.h"

#include <QCoreApplication>
#include <QDBusConnection>
#include <QDBusConnectionInterface>
#include <QDBusInterface>
#include <QDBusReply>
#include <QDBusServiceWatcher>
#include <QJsonDocument>
#include <QJsonObject>
#include <QSettings>
#include <QTimer>
#include <QUuid>

namespace {

const char *kBusName = "id.gauvin.Grouse";
const char *kConnectorPath = "/org/unifiedpush/Connector";
const char *kDistributorPath = "/org/unifiedpush/Distributor";
const char *kDistributorIface = "org.unifiedpush.Distributor2";

} // namespace

PushClient::PushClient(Manager *manager, QObject *parent)
    : QObject(parent), m_manager(manager)
{
    QSettings store(QStringLiteral("grouse"), QStringLiteral("grouse-desktop"));
    m_enabled = store.value(QStringLiteral("push_enabled"), true).toBool();
}

void PushClient::setEnabled(bool on)
{
    if (m_enabled == on)
        return;
    m_enabled = on;
    QSettings(QStringLiteral("grouse"), QStringLiteral("grouse-desktop"))
        .setValue(QStringLiteral("push_enabled"), on);
    emit enabledChanged();
    if (on)
        start();
    else
        unregister();
}

void PushClient::setStatus(const QString &s)
{
    if (m_status == s)
        return;
    m_status = s;
    qInfo("Grouse push: %s", qUtf8Printable(s));
    emit statusChanged();
}

void PushClient::setEndpoint(const QString &url)
{
    if (m_endpoint == url)
        return;
    m_endpoint = url;
    qInfo("Grouse push: endpoint %s", url.isEmpty() ? "(cleared)" : qUtf8Printable(url));
    emit endpointChanged();
}

QString PushClient::token()
{
    QSettings store(QStringLiteral("grouse"), QStringLiteral("grouse-desktop"));
    QString t = store.value(QStringLiteral("push_token")).toString();
    if (t.isEmpty()) {
        t = QUuid::createUuid().toString(QUuid::WithoutBraces);
        store.setValue(QStringLiteral("push_token"), t);
        // Flush NOW. QSettings buffers, and a killed/crashed app never runs the
        // destructor — the token would be lost, the next start would mint another,
        // and the distributor would keep BOTH registrations (a duplicate in its UI,
        // an orphaned endpoint in the push server, and no way to tell which one our
        // token now owns).
        store.sync();
    }
    return t;
}

bool PushClient::exportConnector()
{
    return QDBusConnection::sessionBus().registerObject(QLatin1String(kConnectorPath), this,
                                                        QDBusConnection::ExportAllSlots);
}

QString PushClient::pickDistributor() const
{
    // The spec's order: the environment's choice, then whatever is on the bus.
    const QString fromEnv = qEnvironmentVariable("UNIFIEDPUSH_DISTRIBUTOR");
    if (fromEnv.startsWith(QLatin1String("org.unifiedpush.Distributor.")))
        return fromEnv;
    // Inside the sandbox only names the manifest allows are visible, so the KDE
    // distributor (the one we ship --talk-name for) is tried directly first.
    const QString kde = QStringLiteral("org.unifiedpush.Distributor.kde");
    auto *bus = QDBusConnection::sessionBus().interface();
    if (bus && bus->isServiceRegistered(kde))
        return kde;
    if (bus) {
        const QStringList names = bus->registeredServiceNames().value();
        for (const QString &n : names)
            if (n.startsWith(QLatin1String("org.unifiedpush.Distributor.")))
                return n;
    }
    return {};
}

void PushClient::start()
{
    if (!m_enabled)
        return;
    if (!QDBusConnection::sessionBus().isConnected()) {
        setStatus(QStringLiteral("no session bus"));
        return;
    }
    // Own the name and export the connector BEFORE registering: the distributor may
    // call NewEndpoint the moment Register returns, and a callback with no object
    // would be dropped. A name clash (a second instance) is left to the first.
    QDBusConnection::sessionBus().registerService(QLatin1String(kBusName));
    if (!exportConnector()) {
        setStatus(QStringLiteral("could not export connector"));
        return;
    }
    registerWithDistributor();
}

void PushClient::registerWithDistributor()
{
    const QString distributor = pickDistributor();
    if (distributor.isEmpty()) {
        setStatus(QStringLiteral("no UnifiedPush distributor installed"));
        return;
    }
    QDBusInterface iface(distributor, QLatin1String(kDistributorPath),
                         QLatin1String(kDistributorIface), QDBusConnection::sessionBus());
    if (!iface.isValid()) {
        setStatus(QStringLiteral("distributor unreachable"));
        return;
    }
    QVariantMap args;
    args.insert(QStringLiteral("service"), QLatin1String(kBusName));
    args.insert(QStringLiteral("token"), token());
    args.insert(QStringLiteral("description"), QStringLiteral("Grouse notifications"));
    const QDBusReply<QVariantMap> reply = iface.call(QStringLiteral("Register"), args);
    if (!reply.isValid()) {
        setStatus(QStringLiteral("register failed: %1").arg(reply.error().message()));
        return;
    }
    const QVariantMap res = reply.value();
    if (res.value(QStringLiteral("success")).toString() != QLatin1String("REGISTRATION_SUCCEEDED")) {
        setStatus(QStringLiteral("register failed: %1")
                      .arg(res.value(QStringLiteral("reason")).toString()));
        return;
    }
    // The endpoint itself arrives in NewEndpoint, per the spec.
    setStatus(QStringLiteral("registered with %1").arg(distributor));
}

void PushClient::unregister()
{
    const QString distributor = pickDistributor();
    if (distributor.isEmpty())
        return;
    QDBusInterface iface(distributor, QLatin1String(kDistributorPath),
                         QLatin1String(kDistributorIface), QDBusConnection::sessionBus());
    if (iface.isValid()) {
        QVariantMap args;
        args.insert(QStringLiteral("service"), QLatin1String(kBusName));
        args.insert(QStringLiteral("token"), token());
        iface.call(QStringLiteral("Unregister"), args);
    }
    setEndpoint(QString());
    setStatus(QStringLiteral("push off"));
}

void PushClient::runBackground(int quitAfterMs)
{
    // A push woke us with no window: nothing to show, so there is nothing to keep
    // running for. Quit after the message (Message does it) or after the timeout.
    m_quitCountdown = new QTimer(this);
    m_quitCountdown->setSingleShot(true);
    connect(m_quitCountdown, &QTimer::timeout, qApp, &QCoreApplication::quit);
    m_quitCountdown->start(quitAfterMs);
}

QVariantMap PushClient::NewEndpoint(const QVariantMap &args)
{
    const QString url = args.value(QStringLiteral("endpoint")).toString();
    if (!url.isEmpty()) {
        setEndpoint(url);
        setStatus(QStringLiteral("registered"));
        emit endpointRegistered(url);
    }
    return {};
}

QVariantMap PushClient::Unregistered(const QVariantMap &args)
{
    Q_UNUSED(args);
    setEndpoint(QString());
    // The spec: an app that wants to stay registered must register again.
    if (m_enabled) {
        setStatus(QStringLiteral("unregistered — re-registering"));
        registerWithDistributor();
    } else {
        setStatus(QStringLiteral("unregistered"));
    }
    return {};
}

QVariantMap PushClient::Message(const QVariantMap &args)
{
    const QString raw = QString::fromUtf8(args.value(QStringLiteral("message")).toByteArray()).trimmed();
    if (!raw.isEmpty())
        handlePush(raw);
    if (m_quitCountdown)
        m_quitCountdown->start(150);   // let the notification flush, then quit
    return {};
}

void PushClient::handlePush(const QString &raw)
{
    if (!m_manager)
        return;
    // ONE policy, in the core: the same parser and the same show/don't-show rule the
    // phone calls over uniffi. This client contributes only what it knows.
    const QString envelope = m_manager->pushParse(raw);
    if (envelope.isEmpty())
        return;
    qInfo("Grouse push: received %s", qUtf8Printable(raw.left(200)));
    const QJsonObject ctx{{QStringLiteral("app_visible"), Notifier::appVisible()},
                          {QStringLiteral("armed_session"), QJsonValue::Null},
                          {QStringLiteral("session_title"), QJsonValue::Null},
                          {QStringLiteral("announce_any_turn"), true}};
    const QJsonObject d = QJsonDocument::fromJson(
        m_manager->pushDecide(envelope,
            QString::fromUtf8(QJsonDocument(ctx).toJson(QJsonDocument::Compact))).toUtf8()).object();
    if (d.value(QStringLiteral("show")).toBool())
        Notifier::send(d.value(QStringLiteral("summary")).toString(),
                       d.value(QStringLiteral("body")).toString());
}
