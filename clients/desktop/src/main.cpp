#include <QApplication>
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

// KRunner is a HOST component. The app used to copy its bundled
// grouserunner.so into ~/.local and write a global QT_PLUGIN_PATH snippet on
// every start (with two Flatpak filesystem grants to make that possible). A
// sandboxed app must not mutate the host's plugin dir or session environment:
// host integration is now opt-in via `./build-krunner.sh --install`, which the
// user runs on the host. The app only exports its session-bus service.

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
