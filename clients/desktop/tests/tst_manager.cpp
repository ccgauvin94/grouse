#include <QtTest/QtTest>
#include <QMetaMethod>
#include <QSignalSpy>

#include "corebridge.h"
#include "manager.h"

/**
 * Thin-client Manager tests.
 *
 * The Manager now owns NO wire client: every Q_INVOKABLE routes through the
 * grouse-core C ABI via CoreBridge (dlopen of libgrouse_core.so). Because the
 * bridge is a process singleton with a private constructor, this suite
 * exercises the Manager's full Q_INVOKABLE / Q_PROPERTY surface through
 * QMetaObject and asserts the thin-client invariants hold. When a real
 * libgrouse_core.so is reachable (via GROUSE_CORE, or the standard search
 * paths), the intent calls reach the core; otherwise they no-op safely.
 *
 * The wire-level behavior (connect->chat->permission->tool) is covered by the
 * grouse-core crate's own Rust tests (`cargo test -p grouse-core`), which this
 * suite intentionally does not duplicate.
 */
class TstManager : public QObject
{
    Q_OBJECT
private slots:
    void modelAndStateInvariants();
    void invokableSurfaceRunsWithoutCrash();
    void bridgeLoadsWhenCorePresent();
};

void TstManager::modelAndStateInvariants()
{
    Manager mgr;
    QVERIFY(mgr.messageModel() != nullptr);
    QVERIFY(mgr.sessionsModel() != nullptr);
    QVERIFY(mgr.roamModel() != nullptr);
    // Fresh state: not connected, nothing committed, empty catalogs.
    QCOMPARE(mgr.online(), false);
    QVERIFY(mgr.landingPage());
    QCOMPARE(mgr.prompting(), false);
    QVERIFY(mgr.sessions().toList().isEmpty());
    QVERIFY(mgr.config().toList().isEmpty());
    QCOMPARE(mgr.queuedCount(), 0);
}

void TstManager::invokableSurfaceRunsWithoutCrash()
{
    Manager mgr;
    // Exercise the thin-client intent surface directly through the meta-object
    // system. Whether or not the core .so loads, none of these may crash.
    const QMetaObject *mo = mgr.metaObject();
    QVERIFY(mo != nullptr);

    // No-argument intents.
    const QList<QByteArray> noArgs = {
        "connectToServer", "disconnect", "newChat", "beginChat", "cancelTurn",
        "refreshSessions", "refreshProjects", "refreshRecipes", "refreshSkills",
        "refreshToolGroups", "refreshGlobalExtensions",
    };
    for (const QByteArray &n : noArgs)
        QVERIFY2(QMetaObject::invokeMethod(&mgr, n, Qt::DirectConnection), n.constData());

    // Argument-taking intents.
    QMetaObject::invokeMethod(&mgr, "openSession", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("s1")));
    QMetaObject::invokeMethod(&mgr, "setActiveTab", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("main")));
    QMetaObject::invokeMethod(&mgr, "toggleRoamPeer", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("peer")));
    QMetaObject::invokeMethod(&mgr, "renameSession", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("s1")), Q_ARG(QString, QStringLiteral("t")));
    QMetaObject::invokeMethod(&mgr, "archiveSession", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("s1")));
    QMetaObject::invokeMethod(&mgr, "unarchiveSession", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("s1")));
    QMetaObject::invokeMethod(&mgr, "deleteSession", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("s1")));
    QMetaObject::invokeMethod(&mgr, "sendPrompt", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("hi")));
    QMetaObject::invokeMethod(&mgr, "setConfigOption", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("model")), Q_ARG(QString, QStringLiteral("claude")));
    QMetaObject::invokeMethod(&mgr, "setSchedulePaused", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("j1")), Q_ARG(bool, true));
    QMetaObject::invokeMethod(&mgr, "runScheduleNow", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("j1")));
    QMetaObject::invokeMethod(&mgr, "scheduleRecipe", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("r1")), Q_ARG(QString, QStringLiteral("0 9 * * *")));
    QMetaObject::invokeMethod(&mgr, "deleteRecipe", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("r1")));
    QMetaObject::invokeMethod(&mgr, "moveSessionToProject", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("s1")), Q_ARG(QString, QStringLiteral("p1")));
    QMetaObject::invokeMethod(&mgr, "newChatInProject", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("p1")));
    QMetaObject::invokeMethod(&mgr, "setGlobalExtensionEnabled", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("ext")), Q_ARG(bool, true));
    QMetaObject::invokeMethod(&mgr, "readServerConfig", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("model")));
    QMetaObject::invokeMethod(&mgr, "refreshSupportedModels", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("anthropic")));

    // Result-returning intents stay callable and return a string.
    QString id, pk;
    QVERIFY(QMetaObject::invokeMethod(&mgr, "roamIdentity", Qt::DirectConnection, Q_RETURN_ARG(QString, id)));
    QVERIFY(QMetaObject::invokeMethod(&mgr, "roamPublicKey", Qt::DirectConnection, Q_RETURN_ARG(QString, pk)));
}

void TstManager::bridgeLoadsWhenCorePresent()
{
    // Only meaningful when a libgrouse_core.so is reachable (e.g. the CI smoke
    // sets GROUSE_CORE). If the core cannot load, the thin client still
    // constructs safely and we assert the bridge reports unavailable — never a
    // crash, never a dangling wire.
    CoreBridge *bridge = CoreBridge::instance();
    QVERIFY(bridge != nullptr);
    if (bridge->isAvailable())
        QVERIFY(bridge->handle() != nullptr);
}

QTEST_MAIN(TstManager)
#include "tst_manager.moc"
