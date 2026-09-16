#include "notifier.h"

#include <QDBusConnection>
#include <QDBusInterface>
#include <QGuiApplication>
#include <QStringList>
#include <QVariantMap>

namespace Notifier {

bool appVisible()
{
    return QGuiApplication::applicationState() == Qt::ApplicationActive;
}

bool shouldNotify()
{
    // ApplicationInactive covers "another window has focus" and "minimized";
    // ApplicationSuspended (session locked) counts too — the notification is
    // waiting when the user comes back.
    return !appVisible();
}

void send(const QString &summary, const QString &body)
{
    QDBusInterface iface(QStringLiteral("org.freedesktop.Notifications"),
                         QStringLiteral("/org/freedesktop/Notifications"),
                         QStringLiteral("org.freedesktop.Notifications"),
                         QDBusConnection::sessionBus());
    if (!iface.isValid()) {
        qInfo("Grouse notify: no notification service (%s)",
              qUtf8Printable(iface.lastError().message()));
        return;
    }
    qInfo("Grouse notify: %s", qUtf8Printable(summary));

    QVariantMap hints;
    // Attributes the popup to our .desktop file so the shell shows the right icon
    // and clicking it raises the window (the standard hint the shell reads).
    hints.insert(QStringLiteral("desktop-entry"), QStringLiteral("id.gauvin.Grouse"));

    iface.call(QStringLiteral("Notify"),
               QStringLiteral("Grouse"),             // app_name
               0u,                                   // replaces_id
               QStringLiteral("id.gauvin.Grouse"),   // app_icon
               summary,
               body,
               QStringList(),                        // actions
               hints,
               8000);                                // expire_timeout (ms)
}

} // namespace Notifier
