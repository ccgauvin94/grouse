#include "grouserunner.h"

#include <KLocalizedString>
#include <KPluginFactory>
#include <KService>
#include <KRunner/RunnerContext>
#include <KRunner/RunnerSyntax>
#include <QDBusConnection>
#include <QDBusConnectionInterface>
#include <QDBusMessage>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QProcess>

K_PLUGIN_FACTORY_WITH_JSON(GrouseRunnerFactory, "grouserunner.json",
                           registerPlugin<GrouseRunner>();)

GrouseRunner::GrouseRunner(QObject *parent, const KPluginMetaData &metaData)
    : KRunner::AbstractRunner(parent, metaData)
{
    addSyntax(KRunner::RunnerSyntax(QStringLiteral("grouse :q:"),
                                    i18n("Search Grouse chats by name")));
}

static bool serviceUp()
{
    return QDBusConnection::sessionBus().interface()
        ->isServiceRegistered(QStringLiteral("id.gauvin.Grouse"));
}

void GrouseRunner::match(KRunner::RunnerContext &context)
{
    // "grouse" alone (or "grouse new") always offers a fresh chat.
    KRunner::QueryMatch newChat(this);
    newChat.setText(i18n("New Grouse chat"));
    newChat.setIconName(QStringLiteral("id.gauvin.Grouse"));
    newChat.setData(QStringLiteral("newchat"));
    newChat.setRelevance(0.85);
    context.addMatch(newChat);

    if (!serviceUp()) {
        KRunner::QueryMatch launch(this);
        launch.setText(i18n("Start Grouse"));
        launch.setIconName(QStringLiteral("id.gauvin.Grouse"));
        launch.setData(QStringLiteral("launch"));
        launch.setRelevance(0.85);
        context.addMatch(launch);
        return;
    }

    const QString term = context.query().section(QLatin1Char(' '), 1).trimmed();
    if (term.isEmpty())
        return;

    QDBusMessage call = QDBusMessage::createMethodCall(
        QStringLiteral("id.gauvin.Grouse"), QStringLiteral("/id/gauvin/Grouse"),
        QStringLiteral("id.gauvin.Grouse"), QStringLiteral("ListSessions"));
    const QDBusMessage reply = QDBusConnection::sessionBus().call(call, QDBus::Block, 500);
    if (reply.type() != QDBusMessage::ReplyMessage || reply.arguments().isEmpty())
        return;

    // The service returns a flat JSON array of session maps — nested containers
    // don't demarshal cleanly over the bus, so the wire shape is a string.
    const QString json = reply.arguments().constFirst().toString();
    const QJsonArray sessions = QJsonDocument::fromJson(json.toUtf8()).array();
    for (const auto &v : sessions) {
        const QJsonObject session = v.toObject();
        const QString id = session.value(QStringLiteral("sessionId")).toString();
        if (id.isEmpty())
            continue;
        const QString title = session.value(QStringLiteral("title")).toString();
        if (!title.contains(term, Qt::CaseInsensitive))
            continue;

        KRunner::QueryMatch match(this);
        match.setText(title);
        match.setSubtext(session.value(QStringLiteral("snippet")).toString());
        match.setIconName(QStringLiteral("id.gauvin.Grouse"));
        match.setData(id);
        // Prefix matches rank above substring hits.
        match.setRelevance(title.startsWith(term, Qt::CaseInsensitive) ? 0.9 : 0.6);
        context.addMatch(match);
    }
}

void GrouseRunner::run(const KRunner::RunnerContext &, const KRunner::QueryMatch &match)
{
    const QString action = match.data().toString();
    const QString service = QStringLiteral("id.gauvin.Grouse");
    const QString path = QStringLiteral("/id/gauvin/Grouse");
    const QString iface = QStringLiteral("id.gauvin.Grouse");

    if (action == QLatin1String("newchat")) {
        QDBusConnection::sessionBus().asyncCall(
            QDBusMessage::createMethodCall(service, path, iface, QStringLiteral("NewChat")));
    } else if (action == QLatin1String("launch")) {
        launchGrouse();
    } else {
        QDBusMessage msg = QDBusMessage::createMethodCall(service, path, iface,
                                                          QStringLiteral("OpenSession"));
        msg << action;
        QDBusConnection::sessionBus().asyncCall(msg);
    }
}

void GrouseRunner::launchGrouse()
{
    const KService::Ptr svc = KService::serviceByDesktopName(QStringLiteral("id.gauvin.Grouse"));
    if (!svc)
        return;
    // Exec e.g. "flatpak run id.gauvin.Grouse" or "grouse-desktop"; strip %-fields.
    QStringList parts = svc->exec().split(QLatin1Char(' '), Qt::SkipEmptyParts);
    parts.erase(std::remove_if(parts.begin(), parts.end(),
                               [](const QString &a) { return a.startsWith(QLatin1Char('%')); }),
                parts.end());
    if (parts.isEmpty())
        return;
    const QString program = parts.takeFirst();
    QProcess::startDetached(program, parts);
}

#include "grouserunner.moc"
