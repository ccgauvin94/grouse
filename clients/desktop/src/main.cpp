#include <QApplication>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QQmlEngine>
#include <QQuickStyle>
#include <QQuickWindow>
#include <QtQml>

#ifdef GROUSE_WEBENGINE
#include <QtWebEngineQuick>
#endif

#include "dbusadapter.h"
#include "manager.h"
#include "pushclient.h"

namespace {

// All QML is embedded in the binary (qt_add_resources), so Qt's compiled-QML
// disk cache has nothing legitimate to speed up here — and it is actively
// unsafe: entries for qrc: URLs are keyed on timestamps rcc fixes at the
// epoch, so replacing the Flatpak deploy in place never invalidates them.
// A byte-verified-new binary then renders OLD QML from cache (2026-09-18:
// the tool-panel fixes were "not working" for exactly this reason while two
// windows on one desktop rendered the same qrc differently). Disable it.
struct DisableQmlDiskCache {
    DisableQmlDiskCache() { qputenv("QML_DISABLE_DISK_CACHE", "1"); }
};
const DisableQmlDiskCache disableQmlCacheEarly;

// KRunner runs on the host, outside the flatpak sandbox, so the runner
// plugin ships inside the app image (/app/lib/grouserunner.so) and is
// installed here on first run — the user never touches the host. The
// manifest grants write access to exactly these two locations. Best-effort:
// a sandbox without the grants logs and continues.
void installHostIntegration()
{
    // QLibraryInfo reports the Qt install prefix (/usr in the flatpak
    // runtime), not the app prefix (/app) — anchor on the binary instead.
    const QString bundled = QCoreApplication::applicationDirPath()
        + QStringLiteral("/../lib/grouserunner.so");
    if (!QFileInfo::exists(bundled))
        return;   // native build — the plugin is installed by build-krunner.sh

    const QString pluginDir = QDir::homePath()
        + QStringLiteral("/.local/lib/qt6/plugins/kf6/krunner");
    const QString pluginDest = pluginDir + QStringLiteral("/grouserunner.so");
    if (QDir().mkpath(pluginDir)) {
        QFile::remove(pluginDest);
        if (!QFile::copy(bundled, pluginDest))
            qWarning() << "Could not install KRunner plugin to" << pluginDest;
    } else {
        qWarning() << "Could not create" << pluginDir;
    }

    // Make KRunner scan the user plugin dir from the next session onward.
    const QString envDir = QDir::homePath() + QStringLiteral("/.config/plasma-workspace/env");
    if (QDir().mkpath(envDir)) {
        const QString envFile = envDir + QStringLiteral("/grouse-krunner.sh");
        const QByteArray envBody =
            "# Installed by Grouse — makes KRunner scan the user plugin dir.\n"
            "export QT_PLUGIN_PATH=\"${HOME}/.local/lib/qt6/plugins${QT_PLUGIN_PATH:+:${QT_PLUGIN_PATH}}\"\n";
        QFile f(envFile);
        if (f.open(QIODevice::WriteOnly | QIODevice::Truncate)) {
            f.write(envBody);
        } else {
            qWarning() << "Could not write" << envFile;
        }
    } else {
        qWarning() << "Could not create" << envDir;
    }
}

} // namespace

int main(int argc, char *argv[])
{
#ifdef GROUSE_WEBENGINE
    // Both calls must precede the application object (Qt warns otherwise:
    // "called with QCoreApplication object already created"). When
    // QtWebEngineQuick is not available (no base app / no system package) this
    // whole branch is compiled out and MCP Apps fall back to the loopback
    // bridge in the external browser.
    QCoreApplication::setAttribute(Qt::AA_ShareOpenGLContexts);
    QtWebEngineQuick::initialize();
    Manager::setInlineAppsSupported(true);
#endif
    // QApplication (not QGuiApplication): the native file picker for attachments
    // is a QFileDialog, which needs the widgets app object to host it.
    QApplication app(argc, argv);
    QCoreApplication::setOrganizationName(QStringLiteral("grouse"));
    QCoreApplication::setApplicationName(QStringLiteral("grouse-desktop"));
    // Wayland's app_id comes from this, and Plasma matches the window's icon
    // against <app_id>.desktop: without it the app id is the binary name and
    // the titlebar/taskbar show the generic icon even though the .desktop and
    // SVG ship correctly (id.gauvin.Grouse).
    app.setDesktopFileName(QStringLiteral("id.gauvin.Grouse"));

    // This is a static desktop UI. Native text rendering uses the platform's
    // font hinting instead of rasterizing small labels as Qt Quick textures.
    QQuickWindow::setTextRenderType(QQuickWindow::NativeTextRendering);
    QQuickStyle::setStyle(QStringLiteral("org.kde.desktop"));

    // Grouse design tokens (design/tokens.json v1.1.0), exposed as the
    // Grouse.Theme singleton for the flagged token-consumer surfaces.
    qmlRegisterSingletonType(QUrl(QStringLiteral("qrc:/GrouseTheme.qml")),
                             "Grouse", 1, 0, "Theme");

    Manager manager;
    // Session-bus service for the KRunner plugin (id.gauvin.Grouse).
    DbusAdapter dbus(&manager);
    // Ship the KRunner plugin to the host when running as a flatpak.
    installHostIntegration();

    // UnifiedPush RECEIVE only (see docs/NOTIFICATIONS.md): Grouse ships no sender.
    PushClient push(&manager);
    QObject::connect(&push, &PushClient::endpointRegistered,
                     &manager, &Manager::publishPushEndpoint);

    // Bus-activated for a push: handle it without a window, then quit. There is no
    // QML engine and no connection here — a notification is the entire job.
    if (app.arguments().contains(QStringLiteral("--unifiedpush-background"))) {
        push.start();
        push.runBackground(20000);
        return app.exec();
    }

    QQmlApplicationEngine engine;
    engine.rootContext()->setContextProperty(QStringLiteral("Mgr"), &manager);
    engine.rootContext()->setContextProperty(QStringLiteral("Push"), &push);
    engine.load(QUrl(QStringLiteral("qrc:/main.qml")));

    // Register with the session's distributor (idempotent; keeps the endpoint fresh).
    push.start();

    // Connect at startup when prior settings exist (resuming the last chat).
    manager.autoConnect();

    return app.exec();
}
