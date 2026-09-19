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
    void turnOwnerMatches_ownerRule();
    void sessionExtensionsParseWrappedServerShape();
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

/**
 * Which wire may release a wedged in-flight turn (Android parity).
 *
 * A host killed mid-turn emits neither RunEnded nor a cleared run id, so the
 * `prompting` latch stays set, every later send is parked behind a turn that can
 * never finish, and the UI promises "will send when this turn finishes". Releasing
 * it must be the OWNER's wire, though: clearing it on any drop would let an
 * unrelated chat's disconnect unblock a turn that is genuinely still running (and
 * the queue would then send into a live turn, interleaving transcripts).
 */
void TstManager::turnOwnerMatches_ownerRule()
{
    const QString chatA = QStringLiteral("roam:Phaethon:20260822_1");
    const QString chatB = QStringLiteral("20260917_8");

    // The owning wire releases the turn.
    QVERIFY(turnOwnerMatches(chatA, chatB, chatA));
    // A drop in another chat must not.
    QVERIFY(!turnOwnerMatches(chatA, chatA, chatB));
    // No recorded owner: ownership falls back to the chat on screen.
    QVERIFY(turnOwnerMatches(QString(), chatA, chatA));
    QVERIFY(!turnOwnerMatches(QString(), chatB, chatA));
    // Nothing to release with no session at all.
    QVERIFY(!turnOwnerMatches(QString(), QString(), chatA));
}

void TstManager::sessionExtensionsParseWrappedServerShape()
{
    // Current goose session/extensions/list WRAPS each entry:
    // {"extension": {...}, "extensionKey": "..."}. The display name can differ
    // from the key ("Extension Manager" is keyed extensionmanager) — the panel
    // keys groups by extensionKey (enabled + the remove round-trip) while still
    // showing the display name.
    //
    // The expander rule under test: arrow iff >=2 sub-tools KNOWN — and the list
    // is known WITHOUT a peek when the row is attached with no session allowlist
    // (its active namespaced tools ARE the whole catalogue) or when cached.
    // Unknown rows (detached, never peeked) stay peekable.
    Manager mgr;
    // coreOn* are plain public methods (the bridge calls them directly).
    mgr.coreOnExtensions(QStringLiteral(R"JSON([
        {"extension":{"type":"platform","name":"Extension Manager"},"enabled":true,"configKey":"extensionmanager"},
        {"extension":{"type":"platform","name":"chatrecall"},"enabled":true,"configKey":"chatrecall"},
        {"extension":{"type":"mcp","server":{"name":"fetch"}},"enabled":false,"configKey":"fetch"}
    ])JSON"));
    mgr.coreOnSessionExtensions(QStringLiteral("s1"), QStringLiteral(R"JSON([
        {"extension":{"type":"platform","name":"Extension Manager"},"extensionKey":"extensionmanager"},
        {"extension":{"type":"platform","name":"chatrecall"},"extensionKey":"chatrecall"}
    ])JSON"));
    mgr.coreOnTools(QStringLiteral("s1"), QStringLiteral(R"JSON([
        "extensionmanager__a","extensionmanager__b","chatrecall__chatrecall"
    ])JSON"));

    const QVariantList groups = mgr.toolGroups().toList();
    auto byKey = [&groups](const QString &k) {
        for (const auto &v : groups)
            if (v.toMap().value("key").toString() == k)
                return v.toMap();
        return QVariantMap{};
    };

    const QVariantMap em = byKey(QStringLiteral("extensionmanager"));
    QCOMPARE(em.value("name").toString(), QStringLiteral("Extension Manager"));
    QVERIFY(em.value("enabled").toBool());
    QVERIFY(em.value("known").toBool());        // derived from active tools
    QVERIFY(em.value("expandable").toBool());   // 2 known sub-tools

    const QVariantMap cr = byKey(QStringLiteral("chatrecall"));
    QVERIFY(cr.value("known").toBool());
    QVERIFY(!cr.value("expandable").toBool());  // ONE tool: no arrow, no peek

    const QVariantMap fe = byKey(QStringLiteral("fetch"));
    QVERIFY(!fe.value("enabled").toBool());
    QVERIFY(!fe.value("known").toBool());
    QVERIFY(fe.value("expandable").toBool());   // unknown: the peek arrow

    QVERIFY(byKey(QStringLiteral("nothere")).isEmpty());
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
    QMetaObject::invokeMethod(&mgr, "saveRecipe", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("r1")), Q_ARG(QString, QStringLiteral("{\"title\":\"T\"}")));
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
