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

#include "dbusadapter.h"
#include "manager.h"

namespace {

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
    // QApplication (not QGuiApplication): the native file picker for attachments
    // is a QFileDialog, which needs the widgets app object to host it.
    QApplication app(argc, argv);
    QCoreApplication::setOrganizationName(QStringLiteral("grouse"));
    QCoreApplication::setApplicationName(QStringLiteral("grouse-desktop"));

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

    QQmlApplicationEngine engine;
    engine.rootContext()->setContextProperty(QStringLiteral("Mgr"), &manager);
    engine.load(QUrl(QStringLiteral("qrc:/main.qml")));

    // Connect at startup when prior settings exist (resuming the last chat).
    manager.autoConnect();

    return app.exec();
}
