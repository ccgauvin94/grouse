#include <QtTest/QtTest>
#include <QMetaMethod>
#include <QSettings>
#include <QSignalSpy>
#include <QTemporaryDir>
#include <QJsonDocument>
#include <QJsonArray>
#include <QJsonObject>
#include <QStringList>
#include <QDir>
#include <QUrl>

#include "corebridge.h"
#include "manager.h"
#include "messagelistmodel.h"
#include "sessionlistmodel.h"

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
    void itemStreamDrivesTheModel();
    void openFailureIsVisibleInStatus();
    void configuredProviderFilter();
    void configuredProvidersParse();
    void roamTitleComesFromThePeerList();
    void localSessionsKeepTheirProjectsAfterARoamChat();
    void providerChoicesDisambiguateDuplicateLabels();
    void openSuccessEventFinalizesLoading();
    void fileCardKeyIsSessionScoped();
    void unifiedSessionListGroupsByCoreOwner();
    void runEndRendersTheLastStreamedBubble();
    void tailRendersWithoutAnySeal();
    void fencedCodeBecomesBlocksNotHtml();
    void peerNewSessionUsesThePrefixedId();
    void mainWireReadyOnlyClearsItsOwnTurns();
    void tlsVerificationOptOutIsPersistedAndConfigurable();
    void linkAllowlistMatchesQmlRule();
    void localFileOpensAreValidated();
    void appUrlsAreSessionScopedAndRevokedOnRefetch();
    void permissionQueueKeepsEachRequestAndOwner();
    void formQueueKeepsEachRequestAndOwner();
    void cardPinStatusIsFormattedAndActionable();
    void activityLanesAreGroupedNewestFirst();
    void gooseConfigInvokablesMaskSecretsWithoutCore();
};

/**
 * The item stream is the only transcript channel now (docs/TRANSCRIPT_MODEL.md
 * phase 2). This pins the mapping: kinds become delegate roles, deltas append in
 * place, an Upsert for a known id updates rather than duplicating, and the
 * finalizing Upsert restores the rendered markdown.
 */
void TstManager::itemStreamDrivesTheModel()
{
    Manager mgr;
    auto *m = qobject_cast<MessageListModel *>(mgr.messageModel());
    QVERIFY(m != nullptr);

    mgr.coreOnItem(QStringLiteral(R"({"Reset":{"session_id":"s1"}})"));
    mgr.coreOnItem(QStringLiteral(
        R"({"Upsert":{"item":{"id":"m1","kind":"User","text":"hi","detail":"","output":"","status":"","app_key":"","calls":[]}}})"));
    mgr.coreOnItem(QStringLiteral(
        R"({"Upsert":{"item":{"id":"t1","kind":"Chart","text":"Sankey","detail":"{\"a\":1}","output":"","status":"completed","app_key":"","calls":[]}}})"));
    mgr.coreOnItem(QStringLiteral(R"({"Window":{"oldest_id":"m1","has_older":true}})"));

    QCOMPARE(m->count(), 2);
    QCOMPARE(m->row(0).value("role").toString(), QStringLiteral("user"));
    QCOMPARE(m->row(0).value("text").toString(), QStringLiteral("hi"));
    QVERIFY(!m->row(0).value("html").toString().isEmpty());
    QCOMPARE(m->row(1).value("role").toString(), QStringLiteral("chart"));
    QCOMPARE(m->row(1).value("chartData").toString(), QStringLiteral("{\"a\":1}"));
    QCOMPARE(m->indexForId(QStringLiteral("m1")), 0);
    QVERIFY(mgr.itemHasOlder());

    // A live delta appends to the text and drops the html (plain while
    // streaming); the finalizing Upsert restores the markdown.
    mgr.coreOnItem(QStringLiteral(R"({"AppendText":{"id":"m1","chunk":" there"}})"));
    QCOMPARE(m->row(0).value("text").toString(), QStringLiteral("hi there"));
    QVERIFY(m->row(0).value("html").toString().isEmpty());
    mgr.coreOnItem(QStringLiteral(
        R"({"Upsert":{"item":{"id":"m1","kind":"User","text":"hi there","detail":"","output":"","status":"","app_key":"","calls":[]}}})"));
    QVERIFY(!m->row(0).value("html").toString().isEmpty());

    // An Upsert for a known id updates in place (a re-delivered item must not
    // duplicate the row).
    mgr.coreOnItem(QStringLiteral(
        R"({"Upsert":{"item":{"id":"t1","kind":"Chart","text":"Sankey","detail":"{}","output":"","status":"failed","app_key":"","calls":[]}}})"));
    QCOMPARE(m->count(), 2);
    QCOMPARE(m->row(1).value("status").toString(), QStringLiteral("failed"));

    mgr.coreOnItem(QStringLiteral(R"({"Remove":{"id":"t1"}})"));
    QCOMPARE(m->count(), 1);
    QCOMPARE(m->indexForId(QStringLiteral("t1")), -1);
}

/**
 * The NEWEST bubble renders its markdown even when the core's turn-end seal never
 * lands (measured against a live server: "the last message does not render, the
 * previous ones do once a new one arrives").
 *
 * Why it is desktop-only: this client computes a row's html ONCE, at the row's
 * authoritative upsert, because re-parsing markdown per repaint is quadratic on a
 * long reply. Android renders markdown at paint time, so the same missing seal is
 * invisible there. A turn whose seal is late or lost therefore left the newest
 * bubble showing its source (`**`, fences, `#`) until something re-delivered the
 * row — which is exactly what the next message did.
 *
 * finishTail() renders the tail at run end, idempotently and bounded, so the last
 * bubble can never stay raw.
 */
void TstManager::runEndRendersTheLastStreamedBubble()
{
    Manager mgr;
    auto *m = qobject_cast<MessageListModel *>(mgr.messageModel());
    QVERIFY(m != nullptr);

    mgr.coreOnItem(QStringLiteral(R"({"Reset":{"session_id":"s1"}})"));
    mgr.coreOnItem(QStringLiteral(
        R"({"Upsert":{"item":{"id":"u1","kind":"User","text":"hi","detail":"","output":"","status":"","app_key":"","calls":[]}}})"));
    mgr.coreOnItem(QStringLiteral(
        R"({"Upsert":{"item":{"id":"a1","kind":"Agent","text":"","detail":"","output":"","status":"","app_key":"","calls":[]}}})"));

    // The reply arrives as deltas only: text grows, html is dropped on purpose
    // (plain while streaming — RichText re-parses every chunk otherwise).
    mgr.coreOnItem(QStringLiteral(
        R"({"AppendText":{"id":"a1","chunk":"**bold** and a list:"}})"));
    const int agentRow = m->indexForId(QStringLiteral("a1"));
    QVERIFY(agentRow >= 0);
    QVERIFY(m->row(agentRow).value("html").toString().isEmpty());

    // The seal that would carry the rendered form never arrives.
    mgr.coreOnRunEnded(QString(), QStringLiteral("end_turn"));

    QVERIFY2(!m->row(agentRow).value("html").toString().isEmpty(),
             "a finished turn must not leave its last bubble as raw markdown");
    QVERIFY(m->row(agentRow).value("html").toString().contains(QStringLiteral("font-weight")));
    QCOMPARE(m->row(agentRow).value("role").toString(), QStringLiteral("agent"));

    // Idempotent: a second close-out changes nothing (no double render, no
    // duplicated blocks).
    const QString once = m->row(agentRow).value("html").toString();
    mgr.coreOnRunEnded(QString(), QStringLiteral("end_turn"));
    QCOMPARE(m->row(agentRow).value("html").toString(), once);
    QCOMPARE(m->count(), 2);
}

/**
 * No seal and no run_ended: the quiet-period render is the only thing left.
 * A dropped run_ended (or a stream that simply stops) used to freeze the bubble
 * as raw source until the next message re-sealed it.
 */
void TstManager::tailRendersWithoutAnySeal()
{
    Manager mgr;
    auto *m = qobject_cast<MessageListModel *>(mgr.messageModel());
    QVERIFY(m != nullptr);

    mgr.coreOnItem(QStringLiteral(R"({"Reset":{"session_id":"s1"}})"));
    mgr.coreOnItem(QStringLiteral(
        R"({"Upsert":{"item":{"id":"u1","kind":"User","text":"hi","detail":"","output":"","status":"","app_key":"","calls":[]}}})"));
    mgr.coreOnItem(QStringLiteral(
        R"({"Upsert":{"item":{"id":"a1","kind":"Agent","text":"","detail":"","output":"","status":"","app_key":"","calls":[]}}})"));
    mgr.coreOnItem(QStringLiteral(R"({"AppendText":{"id":"a1","chunk":"**bold**"}})"));
    mgr.coreOnItem(QStringLiteral(R"({"AppendText":{"id":"a1","chunk":" and plain"}})"));

    const int row = m->indexForId(QStringLiteral("a1"));
    QVERIFY(row >= 0);
    QVERIFY(m->row(row).value("html").toString().isEmpty());

    // The reply goes quiet and nothing else ever arrives for it.
    QTRY_VERIFY_WITH_TIMEOUT(!m->row(row).value("html").toString().isEmpty(), 3000);
    QVERIFY(m->row(row).value("html").toString().contains(QStringLiteral("font-weight")));
}

/**
 * The contract the delegate's `messageBlocks` wrapper exists for: a reply with a
 * fenced block is rendered as blocks, and `html` is deliberately left EMPTY.
 * Reading that role off the delegate Item instead of the model (`mdel.blocks`)
 * is always undefined, which left the flat TextEdit showing the raw source and
 * the code panes empty.
 */
void TstManager::fencedCodeBecomesBlocksNotHtml()
{
    Manager mgr;
    auto *m = qobject_cast<MessageListModel *>(mgr.messageModel());
    QVERIFY(m != nullptr);

    mgr.coreOnItem(QStringLiteral(R"({"Reset":{"session_id":"s1"}})"));
    mgr.coreOnItem(QStringLiteral(
        R"({"Upsert":{"item":{"id":"a1","kind":"Agent","text":"intro\n\n```sh\nls -l\n```\n\noutro","detail":"","output":"","status":"","app_key":"","calls":[]}}})"));
    // A paint is Reset + one Upsert per row + Window, and the Manager holds the rows
    // in its buffer until the Window lands them as one model reset — that batching is
    // what removed the blank-then-rebuild jump. Without this op there is no row yet.
    mgr.coreOnItem(QStringLiteral(R"({"Window":{"ids":["a1"]}})"));

    const int row = m->indexForId(QStringLiteral("a1"));
    QVERIFY(row >= 0);
    const QVariantMap r = m->row(row);
    QVERIFY2(!r.value("blocks").toList().isEmpty(), "a fenced block must become blocks");
    QVERIFY2(r.value("html").toString().isEmpty(), "a blocks row carries no html by design");
}

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
        "reloadSessionFromStart",
        "refreshToolGroups", "refreshGlobalExtensions",
    };
    for (const QByteArray &n : noArgs)
        QVERIFY2(QMetaObject::invokeMethod(&mgr, n, Qt::DirectConnection), n.constData());

    // Argument-taking intents.
    QMetaObject::invokeMethod(&mgr, "openSession", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("s1")));
    QMetaObject::invokeMethod(&mgr, "refreshSession", Qt::DirectConnection);
    QMetaObject::invokeMethod(&mgr, "newChatInProject", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("proj1")));
    QMetaObject::invokeMethod(&mgr, "runRecipe", Qt::DirectConnection, Q_ARG(QString, QStringLiteral("r1")));
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

void TstManager::configuredProviderFilter()
{
    // The pure half of Manager::filterConfiguredProviders (the shared rule behind
    // the chat strip, landing page and recipe editor). Android's
    // ProviderChoicesTest pins the identical behavior on the other client.
    const QVariantList catalog{
        QVariantMap{{QStringLiteral("value"), QStringLiteral("openai")},
                    {QStringLiteral("name"), QStringLiteral("OpenAI")}},
        QVariantMap{{QStringLiteral("value"), QStringLiteral("anthropic")},
                    {QStringLiteral("name"), QStringLiteral("Anthropic")}},
        QVariantMap{{QStringLiteral("value"), QStringLiteral("ollama")},
                    {QStringLiteral("name"), QStringLiteral("Ollama")}},
    };
    const QStringList configured{QStringLiteral("openai")};

    // Configured-only hides the rest of the catalog.
    QVariantList got = Manager::filterProviders(catalog, configured, true, QStringLiteral("openai"));
    QCOMPARE(got.size(), 1);
    QCOMPARE(got[0].toMap().value(QStringLiteral("value")).toString(), QStringLiteral("openai"));

    // The current pick survives even when it isn't configured.
    got = Manager::filterProviders(catalog, configured, true, QStringLiteral("ollama"));
    QCOMPARE(got.size(), 2);
    QCOMPARE(got[1].toMap().value(QStringLiteral("value")).toString(), QStringLiteral("ollama"));

    // Unknown inventory (server without providers/list) shows everything.
    got = Manager::filterProviders(catalog, {}, true, QStringLiteral("openai"));
    QCOMPARE(got.size(), 3);

    // Opting out of configured-only shows everything.
    got = Manager::filterProviders(catalog, configured, false, QStringLiteral("openai"));
    QCOMPARE(got.size(), 3);
}

void TstManager::configuredProvidersParse()
{
    // `hasCredentials` is the core's stricter signal: goose's `configured` also marks
    // local endpoints and detected CLIs usable with nothing set, and those must not
    // clutter the picker.
    const QString json = R"([
        {"providerId":"anthropic","configured":true,"hasCredentials":true},
        {"providerId":"lmstudio","configured":true,"hasCredentials":false},
        {"providerId":"aimlapi","configured":false,"hasCredentials":false}
    ])";
    QCOMPARE(Manager::configuredProvidersFromJson(json),
             QStringList{QStringLiteral("anthropic")});

    // A core that predates the field falls back to `configured` (nothing regresses).
    const QString legacy = R"([{"providerId":"openai","configured":true}])";
    QCOMPARE(Manager::configuredProvidersFromJson(legacy),
             QStringList{QStringLiteral("openai")});
}

/**
 * A rejected open must be VISIBLE (grouse#19). The core binds no session and
 * substitutes none, so the status line is the only signal the user gets that
 * the chat they tapped did not load — a silent core would leave them looking at
 * an empty conversation.
 */
void TstManager::openFailureIsVisibleInStatus()
{
    Manager mgr;
    QString message;
    QSignalSpy spy(&mgr, &Manager::statusChanged);
    mgr.coreOnSessionOpenFailed(QStringLiteral("sess-gone"),
                                 QStringLiteral("session/load failed: session not found"));
    QVERIFY2(mgr.status().contains(QStringLiteral("session not found")),
             qPrintable(mgr.status()));
    QVERIFY2(spy.count() >= 1, "the status change was emitted (the UI re-renders)");
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

void TstManager::roamTitleComesFromThePeerList()
{
    Manager mgr;
    // A peer's session list is the only place a roam chat's human title arrives;
    // opening by id used to leave the raw transport handle (`roam:peer:id`) in
    // the window title and the chat header.
    mgr.coreOnRoamSessions(QStringLiteral("Phaethon"),
        QStringLiteral(R"([{"id":"roam:Phaethon:20261024_2","title":"Home Assistant"}])"));
    mgr.openRoamSession(QStringLiteral("Phaethon"),
                        QStringLiteral("roam:Phaethon:20261024_2"), QString());
    QCOMPARE(mgr.currentSessionTitle(), QStringLiteral("Home Assistant"));

    // An untitled peer chat still never shows the namespace.
    mgr.openRoamSession(QStringLiteral("Phaethon"),
                        QStringLiteral("roam:Phaethon:20261024_9"), QString());
    QCOMPARE(mgr.currentSessionTitle(), QStringLiteral("Phaethon · 20261024_9"));

    // A title that arrives after the chat is open (the peer names a session
    // once it has a turn) updates the title in place.
    mgr.openRoamSession(QStringLiteral("Phaethon"),
                        QStringLiteral("roam:Phaethon:20261024_2"), QString());
    QCOMPARE(mgr.currentSessionTitle(), QStringLiteral("Home Assistant"));
    mgr.coreOnRoamSessions(QStringLiteral("Phaethon"),
        QStringLiteral(R"([{"id":"roam:Phaethon:20261024_2","title":"TV + network"}])"));
    QCOMPARE(mgr.currentSessionTitle(), QStringLiteral("TV + network"));

    // A main-wire session is unaffected: its id is already the title fallback.
    mgr.openSession(QStringLiteral("s1"));
    QCOMPARE(mgr.currentSessionTitle(), QStringLiteral("s1"));
}

void TstManager::localSessionsKeepTheirProjectsAfterARoamChat()
{
    Manager mgr;
    // Regression, 2026-11-20: opening a roam chat set m_activePeerLabel, and
    // coreOnSessions stamped EVERY local row with it. Going back to Main then showed
    // the whole local list as one `peer: <label>` group — the Projects band gone, the
    // chats flat — because a row's section is derived from that stamp. The label
    // belongs to the peer's OWN list (coreOnRoamSessions), never to the local one.
    mgr.openRoamSession(QStringLiteral("Phaethon"),
                        QStringLiteral("roam:Phaethon:20260101_1"), QString());
    QVERIFY(mgr.onRoamSession());   // the peer really is the active one

    mgr.coreOnProjects(QStringLiteral(R"([{"id":"hacking","name":"Hacking"}])"));
    mgr.coreOnSessions(QStringLiteral(
        R"([{"id":"s1","title":"In a project","updated_at":"2026-11-20T10:00:00Z","project_id":"hacking"},)"
        R"({"id":"s2","title":"Unfiled","updated_at":"2026-11-20T09:00:00Z"}])"));

    auto *m = qobject_cast<SessionListModel *>(mgr.sessionsModel());
    QVERIFY(m != nullptr);
    QStringList sections;
    for (int r = 0; r < m->rowCount(); ++r) {
        sections << m->data(m->index(r, 0), SessionListModel::SectionRole).toString();
    }
    QVERIFY2(!sections.join('|').contains(QStringLiteral("peer:")),
             qPrintable(sections.join('|')));
    QVERIFY(sections.contains(QStringLiteral("projects")));
    QVERIFY(sections.contains(QStringLiteral("proj:hacking")));
    QVERIFY(sections.contains(QStringLiteral("chats")));
}

void TstManager::providerChoicesDisambiguateDuplicateLabels()
{
    // Two providers can carry the same display label (a CLI-detected entry and its
    // API twin both read "OpenCode Go"): the picker listed it twice with nothing to
    // tell them apart, and simply dropping one would make it unreachable.
    const QVariantList choices{
        QVariantMap{{"value", "opencode-go"}, {"name", "OpenCode Go"}},
        QVariantMap{{"value", "opencode-go-cli"}, {"name", "OpenCode Go"}},
        QVariantMap{{"value", "opencode-zen"}, {"name", "OpenCode Zen"}},
    };
    const QVariantList kept = Manager::filterProviders(choices, QStringList(), false, QString());
    QCOMPARE(kept.size(), 3);
    QCOMPARE(kept.at(0).toMap().value("name").toString(), QStringLiteral("OpenCode Go"));
    QCOMPARE(kept.at(1).toMap().value("name").toString(),
             QStringLiteral("OpenCode Go (opencode-go-cli)"));
    QCOMPARE(kept.at(2).toMap().value("name").toString(), QStringLiteral("OpenCode Zen"));

    // An exact duplicate (same label AND value) picks the same provider twice, so it
    // is dropped; the same label with a different value keeps both, the later one
    // carrying its id so they can be told apart.
    const QVariantList withExactDuplicate{
        QVariantMap{{"value", "opencode-go"}, {"name", "OpenCode Go"}},
        QVariantMap{{"value", "opencode-go"}, {"name", "OpenCode Go"}},
        QVariantMap{{"value", "opencode-go-cli"}, {"name", "OpenCode Go"}},
    };
    const QVariantList deduped = Manager::filterProviders(
        withExactDuplicate, QStringList(), false, QString());
    QCOMPARE(deduped.size(), 2);
    QCOMPARE(deduped.at(0).toMap().value("name").toString(), QStringLiteral("OpenCode Go"));
    QCOMPARE(deduped.at(1).toMap().value("name").toString(),
             QStringLiteral("OpenCode Go (opencode-go-cli)"));

    // The configured-only filter still keeps the current pick visible, and a
    // disabled/off-catalog current value is not silently dropped.
    const QVariantList filtered = Manager::filterProviders(
        choices, QStringList{QStringLiteral("opencode-zen")}, true, QStringLiteral("opencode-go"));
    QCOMPARE(filtered.size(), 2);
}

void TstManager::openSuccessEventFinalizesLoading()
{
    Manager mgr;
    QSignalSpy spy(&mgr, &Manager::statusChanged);
    mgr.openRoamSession(QStringLiteral("Phaethon"),
                        QStringLiteral("roam:Phaethon:20261024_2"), QString());
    QCOMPARE(mgr.status(), QStringLiteral("loading…"));

    // A Window only says rows were painted: background fills and load-older send
    // one too, so it must NOT declare the open successful.
    mgr.coreOnItem(QStringLiteral(R"({"Window":{"oldest_id":"m1","has_older":true}})"));
    QCOMPARE(mgr.status(), QStringLiteral("loading…"));

    // A replay that reports progress but never says `done` used to leave the bar
    // spinning forever once the chat was settled.
    mgr.coreOnReplayProgress(QStringLiteral("roam:Phaethon:20261024_2"), 4, 10, false);
    QVERIFY(mgr.reloading());
    QCOMPARE(mgr.replayLoaded(), 4);

    // The core's positive answer ends BOTH bars, for this chat only.
    mgr.coreOnSessionOpened(QStringLiteral("roam:Phaethon:20261024_2"));
    QCOMPARE(mgr.status(), QStringLiteral("ready"));
    QVERIFY(!mgr.reloading());
    QCOMPARE(mgr.replayLoaded(), 0);
    QCOMPARE(mgr.replayTotal(), 0);
    QVERIFY(spy.count() >= 1);

    // Another chat's success (or a background bind) cannot settle this header.
    mgr.coreOnSessionOpenFailed(QStringLiteral("sess-gone"),
                                QStringLiteral("session/load failed: stale"));
    const QString failure = mgr.status();
    QVERIFY2(failure.contains(QStringLiteral("stale")), qPrintable(failure));
    mgr.coreOnReplayProgress(QStringLiteral("other"), 1, 1, false);
    mgr.coreOnSessionOpened(QStringLiteral("other"));
    mgr.coreOnItem(QStringLiteral(R"({"Window":{"oldest_id":"m0","has_older":false}})"));
    QCOMPARE(mgr.status(), failure);
    QVERIFY(!mgr.reloading());   // the failure dropped the bar too
}

/** File events carry their owning session; the card key must include it. */
void TstManager::fileCardKeyIsSessionScoped()
{
    Manager mgr;
    const QString path = QStringLiteral("/srv/reports/same-name.pdf");
    const QString local = QStringLiteral("roam:Phaethon:20261024_2");
    const QString other = QStringLiteral("20261122_9");
    QVERIFY(mgr.fileCardKey(local, path) != mgr.fileCardKey(other, path));
    QCOMPARE(mgr.fileCardKey(local, path),
             QStringLiteral("roam:Phaethon:20261024_2|/srv/reports/same-name.pdf"));

    QSignalSpy spy(&mgr, &Manager::fileEvent);
    // Serde's externally-tagged enum: fields live inside the variant object.
    mgr.coreOnFile(QStringLiteral(R"({"Progress":{"path":"/srv/reports/same-name.pdf",)"
                                  R"("name":"same-name.pdf","received":10,"total":20,)"
                                  R"("session_id":"roam:Phaethon:20261024_2",)"
                                  R"("request_id":"req-1"}})"));
    QCOMPARE(spy.count(), 1);
    const QJsonObject ev = QJsonDocument::fromJson(spy.at(0).at(0).toString().toUtf8()).object();
    QCOMPARE(ev.value(QStringLiteral("key")).toString(), mgr.fileCardKey(local, path));
    QCOMPARE(ev.value(QStringLiteral("session_id")).toString(), local);

    // An older core omits the session: the chat on screen is the only fallback.
    mgr.openSession(other);
    mgr.coreOnFile(QStringLiteral(R"({"Saved":{"path":"/srv/reports/same-name.pdf",)"
                                  R"("name":"same-name.pdf","mime":"application/pdf",)"
                                  R"("bytes":20,"local_path":"/tmp/copy.pdf"}})"));
    QCOMPARE(spy.count(), 2);
    const QJsonObject older = QJsonDocument::fromJson(spy.at(1).at(0).toString().toUtf8()).object();
    QCOMPARE(older.value(QStringLiteral("key")).toString(), mgr.fileCardKey(other, path));
}

/** One merged list from the core; the model groups by the core's owner answer. */
void TstManager::unifiedSessionListGroupsByCoreOwner()
{
    Manager mgr;
    mgr.coreOnProjects(QStringLiteral(R"([{"id":"hacking","name":"Hacking"}])"));
    mgr.coreOnSessions(QStringLiteral(R"([
        {"id":"local-1","title":"Local","updated_at":"2026-11-22T10:00:00Z","project_id":"hacking"},
        {"id":"roam:Phaethon:peer-1","title":"Remote","updated_at":"2026-11-22T09:00:00Z"}
    ])"));

    auto *m = qobject_cast<SessionListModel *>(mgr.sessionsModel());
    QVERIFY(m != nullptr);
    QStringList sections;
    for (int r = 0; r < m->rowCount(); ++r)
        sections << m->data(m->index(r, 0), SessionListModel::SectionRole).toString();
    QVERIFY(sections.contains(QStringLiteral("proj:hacking")));
    QVERIFY2(sections.contains(QStringLiteral("peer:Phaethon")), qPrintable(sections.join('|')));
}

/**
 * A peer's `on_peer_new_session` carries the RAW id. The core only routes a
 * `roam:<label>:<id>` display id, and the client's owner label must agree with
 * the id on screen — otherwise a main-wire Ready clears the peer's live turn.
 * Android's ConnectionManager.onPeerNewSession does the same prefixing.
 */
void TstManager::peerNewSessionUsesThePrefixedId()
{
    Manager mgr;
    mgr.openRoamSession(QStringLiteral("Phaethon"),
                        QStringLiteral("roam:Phaethon:20260101_1"), QString());
    QVERIFY(mgr.onRoamSession());

    QSignalSpy spy(&mgr, &Manager::currentSessionChanged);
    mgr.coreOnPeerNewSession(QStringLiteral("Phaethon"), QStringLiteral("20261122_7"));
    QCOMPARE(mgr.currentSessionId(), QStringLiteral("roam:Phaethon:20261122_7"));
    QVERIFY(mgr.onRoamSession());   // still the peer's turn, not the main wire's
    QCOMPARE(Manager::roamPeerForSession(mgr.currentSessionId()), QStringLiteral("Phaethon"));
    QVERIFY(spy.count() >= 1);

    // A local id has no owner label — opening one hands the turn back to main.
    mgr.openSession(QStringLiteral("s1"));
    QCOMPARE(mgr.currentSessionId(), QStringLiteral("s1"));
    QVERIFY(!mgr.onRoamSession());
}

/** The pure half of the main-wire Ready guard (the peer-owned turn must survive). */
void TstManager::mainWireReadyOnlyClearsItsOwnTurns()
{
    QVERIFY(Manager::mainWireOwnsTurnState(QString()));
    QVERIFY(!Manager::mainWireOwnsTurnState(QStringLiteral("Phaethon")));
}

/** TLS verification is on by default; trust-all must be an explicit opt-in. */
void TstManager::tlsVerificationOptOutIsPersistedAndConfigurable()
{
    // Isolate QSettings so "no stored value" really means no value, and so the
    // suite never reads or rewrites a developer install's TLS opt-in.
    const QString originalPath = QSettings::path(QSettings::NativeFormat, QSettings::UserScope);
    QTemporaryDir dir;
    QVERIFY(dir.isValid());
    QSettings::setPath(QSettings::NativeFormat, QSettings::UserScope, dir.path());
    struct RestorePath {
        explicit RestorePath(const QString &p) : path(p) {}
        ~RestorePath() { QSettings::setPath(QSettings::NativeFormat, QSettings::UserScope, path); }
        QString path;
    } restore(originalPath);

    QCOMPARE(Manager::DEFAULT_ACCEPT_INVALID_CERTS, false);
    {
        Manager mgr;
        // Fresh install: WebPKI verification, and the wire config says so.
        QVERIFY(!mgr.acceptInvalidCerts());
        const QString config = mgr.serverConfigJson();
        QVERIFY2(config.contains(QStringLiteral("\"accept_invalid_certs\"")), qPrintable(config));
        QVERIFY2(config.contains(QStringLiteral("\"accept_invalid_certs\":false")), qPrintable(config));

        // An explicit stored true still round-trips to a fresh Manager and the core.
        mgr.setAcceptInvalidCerts(true);
        QVERIFY(mgr.acceptInvalidCerts());
        const QString optedOut = mgr.serverConfigJson();
        QVERIFY2(optedOut.contains(QStringLiteral("\"accept_invalid_certs\":true")), qPrintable(optedOut));
        Manager other;
        QVERIFY(other.acceptInvalidCerts());   // persisted in QSettings
        QVERIFY(other.serverConfigJson().contains(QStringLiteral("\"accept_invalid_certs\":true")));
    }
}

/** ChatPage.qml calls Mgr.linkAllowed() at every Text.onLinkActivated; the
 *  markdown renderer uses the same predicate to strip disallowed hrefs. */
void TstManager::linkAllowlistMatchesQmlRule()
{
    Manager mgr;
    const QMetaObject *mo = mgr.metaObject();
    QVERIFY2(mo->indexOfMethod("linkAllowed(QString)") >= 0, "QML must see linkAllowed");
    QVERIFY(mgr.linkAllowed(QStringLiteral("https://example.com")));
    QVERIFY(mgr.linkAllowed(QStringLiteral("http://example.com")));
    QVERIFY(mgr.linkAllowed(QStringLiteral("mailto:someone@example.com")));
    QVERIFY(!mgr.linkAllowed(QStringLiteral("file:///etc/passwd")));
    QVERIFY(!mgr.linkAllowed(QStringLiteral("javascript:alert(1)")));
    QVERIFY(!mgr.linkAllowed(QStringLiteral("grouse://open")));
    QVERIFY(!mgr.linkAllowed(QStringLiteral("//example.com")));
}

/** The local-file handoff is no longer Qt.openUrlExternally("file://"+text):
 *  it goes through Manager, which only opens an existing local regular file. */
void TstManager::localFileOpensAreValidated()
{
    Manager mgr;
    QVERIFY(mgr.metaObject()->indexOfMethod("openLocalFile(QString)") >= 0);
    QVERIFY(!mgr.openLocalFile(QString()));
    QVERIFY(!mgr.openLocalFile(QStringLiteral("relative/path.png")));
    QVERIFY(!mgr.openLocalFile(QDir::tempPath()));          // a directory
    QVERIFY(!mgr.openLocalFile(QStringLiteral("/definitely/not/here.png")));
}

/** A chat never reuses another chat's bridge token: the same (appKey) served
 *  in two sessions must yield two URLs/tokens, switching back is stable, and a
 *  template refetch revokes BOTH cached URLs so no stale tab keeps a live
 *  capability. The port in the URL is the one AppView.qml pins navigation to. */
void TstManager::appUrlsAreSessionScopedAndRevokedOnRefetch()
{
    Manager mgr;
    QVERIFY(mgr.metaObject()->indexOfMethod("appBridgePort()") >= 0);
    const QString appKey = QStringLiteral("ext|uri");
    mgr.coreOnAppResource(appKey, QStringLiteral("<html><head></head></html>"));

    mgr.openSession(QStringLiteral("s1"));
    const QString first = mgr.appViewUrl(appKey);
    QVERIFY2(!first.isEmpty(), "app view URL must be served over the loopback bridge");
    QVERIFY2(QUrl(first).port() == mgr.appBridgePort(), "guard must see the bridge port");
    QVERIFY(mgr.appBridgePort() > 0);

    mgr.openSession(QStringLiteral("s2"));
    const QString second = mgr.appViewUrl(appKey);
    QVERIFY(!second.isEmpty());
    QVERIFY2(first != second, "two chats must not share a token");

    mgr.openSession(QStringLiteral("s1"));
    QCOMPARE(mgr.appViewUrl(appKey), first);   // stable while the template is

    mgr.coreOnAppResource(appKey, QStringLiteral("<html><head></head><body>new</body></html>"));
    const QString refreshed = mgr.appViewUrl(appKey);
    QVERIFY(!refreshed.isEmpty());
    QVERIFY(refreshed != first);
    QVERIFY(refreshed != second);
    QCOMPARE(mgr.appViewUrl(appKey), refreshed);
}

/**
 * ABI 20 requests carry their own id and owner. A second permission must not
 * replace the first (the old one-slot member dropped the parked responder), and
 * a late dialog answer must not deny a DIFFERENT request.
 */
void TstManager::permissionQueueKeepsEachRequestAndOwner()
{
    Manager mgr;
    mgr.coreOnPermission(QStringLiteral(
        R"({"PermissionRequest":{"request_id":"req-1","tool_call_id":"tool-1",)"
        R"("session_id":"roam:nas:s1","owner":{"kind":"roam","label":"nas"},)"
        R"("title":"Use the tool","detail":"dangerous",)"
        R"("options":[{"option_id":"allow_once","name":"Allow","kind":"allow_once"}]}})"));
    mgr.coreOnPermission(QStringLiteral(
        R"({"PermissionRequest":{"request_id":"req-2","tool_call_id":"tool-2",)"
        R"("session_id":"local-1","owner":{"kind":"main","label":""},)"
        R"("title":"Second tool","detail":"",)"
        R"("options":[{"option_id":"reject_once","name":"Deny","kind":"reject_once"}]}})"));

    QCOMPARE(mgr.permissionCount(), 2);
    QCOMPARE(mgr.permissionToolCallId(), QStringLiteral("req-1"));
    QCOMPARE(mgr.permissionRequestId(), QStringLiteral("req-1"));
    QCOMPARE(mgr.permissionOwner(), QStringLiteral("roam:nas"));
    QVERIFY2(mgr.permissionTitle().contains(QStringLiteral("roam:nas")),
             qPrintable(mgr.permissionTitle()));
    QCOMPARE(mgr.permissionOptions().size(), 1);
    QCOMPARE(mgr.permissionOptions().first().toMap().value(QStringLiteral("option_id")).toString(),
             QStringLiteral("allow_once"));

    // Answer the second by id: the first stays exactly where it was.
    mgr.respondPermission(QStringLiteral("req-2"), QStringLiteral("reject_once"));
    QCOMPARE(mgr.permissionCount(), 1);
    QCOMPARE(mgr.permissionToolCallId(), QStringLiteral("req-1"));
    mgr.respondPermission(QStringLiteral("req-1"), QStringLiteral("allow_once"));
    QCOMPARE(mgr.permissionCount(), 0);
    QVERIFY(mgr.permissionTitle().isEmpty());
    // A stale click cannot answer the (now absent) request or its successor.
    mgr.respondPermission(QStringLiteral("req-2"), QString());
    QCOMPARE(mgr.permissionCount(), 0);

    // Redelivery is idempotent: do not replace the visible title/options.
    mgr.coreOnPermission(QStringLiteral(
        R"({"request_id":"req-3","session_id":"s3","title":"Original","options":[]})"));
    mgr.coreOnPermission(QStringLiteral(
        R"({"request_id":"req-3","session_id":"s3","title":"Replacement","options":[]})"));
    QCOMPARE(mgr.permissionCount(), 1);
    QVERIFY2(mgr.permissionTitle().contains(QStringLiteral("Original")),
             qPrintable(mgr.permissionTitle()));

    // Pure helpers used by the queue: legacy tool-call ids and empty ids.
    const QVariantList queue{QVariantMap{{"requestId", QStringLiteral("a")}},
                             QVariantMap{{"toolCallId", QStringLiteral("b")}}};
    QCOMPARE(Manager::requestQueueIndex(queue, QStringLiteral("a")), 0);
    QCOMPARE(Manager::requestQueueIndex(queue, QStringLiteral("b")), 1);
    QCOMPARE(Manager::requestQueueIndex(queue, QString()), -1);
    QCOMPARE(Manager::requestQueueIndex(queue, QStringLiteral("c")), -1);
    const QVariantMap parsed = Manager::parsePermissionRequest(
        QStringLiteral(R"({"request_id":"r","owner":"nas","session_id":"roam:nas:s1"})"));
    QCOMPARE(parsed.value(QStringLiteral("ownerLabel")).toString(), QStringLiteral("roam:nas"));
}

/** Forms are queued by request id too; the owner is part of the visible title. */
void TstManager::formQueueKeepsEachRequestAndOwner()
{
    Manager mgr;
    mgr.coreOnRecipeParams(QStringLiteral("roam:nas"),
                           QStringLiteral("form-1"),
                           QStringLiteral(R"([{"key":"branch","input_type":"string","requirement":"required"}])"));
    mgr.coreOnElicitation(QStringLiteral(R"({"kind":"main","label":"main"})"),
                          QStringLiteral("form-2"),
                          QStringLiteral(R"({"title":"Pick one","properties":{"go":{"type":"boolean"}},"required":[]})"));

    const QVariantMap first = mgr.pendingForm().toMap();
    QCOMPARE(first.value(QStringLiteral("requestId")).toString(), QStringLiteral("form-1"));
    QVERIFY(first.value(QStringLiteral("recipeParams")).toBool());
    QVERIFY2(first.value(QStringLiteral("title")).toString().contains(QStringLiteral("roam:nas")),
             qPrintable(first.value(QStringLiteral("title")).toString()));
    QCOMPARE(first.value(QStringLiteral("fields")).toList().size(), 1);

    // Answer the second by id; the recipe form is still the visible one.
    mgr.answerFormFor(QStringLiteral("form-2"), QVariantMap{}, QStringLiteral("decline"));
    const QVariantMap remains = mgr.pendingForm().toMap();
    QCOMPARE(remains.value(QStringLiteral("requestId")).toString(), QStringLiteral("form-1"));

    // A redelivered form cannot replace the parked one.
    mgr.coreOnRecipeParams(QStringLiteral("roam:nas"), QStringLiteral("form-1"),
                           QStringLiteral(R"([{"key":"other","input_type":"string"}])"));
    QCOMPARE(mgr.pendingForm().toMap().value(QStringLiteral("requestId")).toString(),
             QStringLiteral("form-1"));
    QCOMPARE(mgr.pendingForm().toMap().value(QStringLiteral("fields")).toList().size(), 1);

    // Answer the visible form: nothing is left parked.
    mgr.answerForm(QVariantMap{{QStringLiteral("branch"), QStringLiteral("main")}},
                   QStringLiteral("accept"));
    QVERIFY(mgr.pendingForm().toMap().isEmpty());
    mgr.answerFormFor(QStringLiteral("form-2"), QVariantMap{}, QStringLiteral("accept"));
    QVERIFY(mgr.pendingForm().toMap().isEmpty());
}

/** Card pins are pure-formatted, and a refusal becomes one actionable dialog. */
void TstManager::cardPinStatusIsFormattedAndActionable()
{
    const QVariantList rows = Manager::formatCardPins(
        QVariantMap{{QStringLiteral("nas"), QStringLiteral("0123456789abcdef0123")},
                    {QStringLiteral("home"), QStringLiteral("short")}});
    QCOMPARE(rows.size(), 2);
    QCOMPARE(rows.first().toMap().value(QStringLiteral("peer")).toString(),
             QStringLiteral("home"));
    QCOMPARE(rows.first().toMap().value(QStringLiteral("fingerprint")).toString(),
             QStringLiteral("short"));
    QCOMPARE(rows.last().toMap().value(QStringLiteral("short")).toString(),
             QStringLiteral("0123456789ab…0123"));
    QVERIFY(Manager::formatCardPins(QVariantMap{{QString(), QStringLiteral("fp")}}).isEmpty());

    const QString status = QStringLiteral(
        "error: roam card pin refused: roam card for 'nas' changed: pinned aaaa, "
        "got bbbb. Refusing to dial — clear the pin to re-enroll this peer");
    const QVariantMap refusal = Manager::parseCardPinRefusal(status);
    QVERIFY(!refusal.isEmpty());
    QCOMPARE(refusal.value(QStringLiteral("peer")).toString(), QStringLiteral("nas"));
    QCOMPARE(refusal.value(QStringLiteral("pinned")).toString(), QStringLiteral("aaaa"));
    QCOMPARE(refusal.value(QStringLiteral("got")).toString(), QStringLiteral("bbbb"));
    QVERIFY(Manager::parseCardPinRefusal(QStringLiteral("connecting: dialing")).isEmpty());
    QCOMPARE(Manager::parseCardPinRefusal(
                 QStringLiteral("roam card pin refused: store is unusable"),
                 QStringLiteral("home")).value(QStringLiteral("peer")).toString(),
             QStringLiteral("home"));

    Manager mgr;
    mgr.coreOnRoamPeerStatus(QStringLiteral("nas"), status);
    QCOMPARE(mgr.roamPinRefusal().value(QStringLiteral("peer")).toString(),
             QStringLiteral("nas"));
    mgr.coreOnRoamPeerStatus(QStringLiteral("nas"), status);   // a burst: no second dialog
    QCOMPARE(mgr.roamPinRefusal().value(QStringLiteral("message")).toString(),
             refusal.value(QStringLiteral("message")).toString());
    mgr.dismissRoamPinRefusal();
    QVERIFY(mgr.roamPinRefusal().isEmpty());
    QVERIFY(mgr.roamCardPins().isEmpty());                    // no core loaded
    QVERIFY(!mgr.clearRoamCardPin(QStringLiteral("nas"), QStringLiteral("typo")));
}

/** The background-work panel shows live lanes first and history after a divider,
 *  both newest first, even though the core's getter emits oldest first. The core's
 *  `running` flag decides the group: a stale `in_progress` status from a stopped
 *  turn must stay in history. */
void TstManager::activityLanesAreGroupedNewestFirst()
{
    auto lane = [](const QString &id, bool running, const QString &status) {
        QJsonObject o;
        o.insert(QStringLiteral("id"), id);
        o.insert(QStringLiteral("title"), id);
        o.insert(QStringLiteral("status"), status);
        o.insert(QStringLiteral("running"), running);
        return o;
    };

    QJsonArray raw;
    raw.append(lane(QStringLiteral("done-1"), false, QStringLiteral("completed")));
    raw.append(lane(QStringLiteral("live-1"), true, QStringLiteral("in_progress")));
    // Stale in_progress, left behind by a dropped turn: finished, not live.
    raw.append(lane(QStringLiteral("stale"), false, QStringLiteral("in_progress")));
    raw.append(lane(QStringLiteral("done-2"), false, QStringLiteral("completed")));
    raw.append(lane(QStringLiteral("live-2"), true, QStringLiteral("in_progress")));
    raw.append(lane(QString(), true, QStringLiteral("in_progress")));   // id-less dropped

    QVERIFY(buildActivityLanes(QJsonArray()).isEmpty());

    const QVariantList lanes = buildActivityLanes(raw);
    QStringList ids, sections;
    for (const QVariant &v : lanes) {
        const QVariantMap row = v.toMap();
        ids.append(row.value(QStringLiteral("id")).toString());
        sections.append(row.value(QStringLiteral("section")).toString());
        QCOMPARE(row.value(QStringLiteral("finished")).toBool(),
                 row.value(QStringLiteral("section")).toString() == QStringLiteral("finished"));
    }
    const QStringList expectedIds{QStringLiteral("live-2"), QStringLiteral("live-1"),
                                  QStringLiteral("stale"), QStringLiteral("done-2"),
                                  QStringLiteral("done-1")};
    const QStringList expectedSections{QStringLiteral("running"), QStringLiteral("running"),
                                       QStringLiteral("finished"), QStringLiteral("finished"),
                                       QStringLiteral("finished")};
    QCOMPARE(ids, expectedIds);
    QCOMPARE(sections, expectedSections);
    QCOMPARE(raw.size(), 6);   // the snapshot handed over by the core is not reordered
}

/** The config editor is a thin facade: four intents over the C ABI, with the
 *  secret flag kept client-side because read-all lists only non-secret keys and
 *  on_config_value carries no marker. This pins the QML surface AND the rule
 *  that a secret reply is never exposed as a value — without needing a core. */
void TstManager::gooseConfigInvokablesMaskSecretsWithoutCore()
{
    Manager mgr;
    const QMetaObject *mo = mgr.metaObject();
    QVERIFY(mo->indexOfMethod("refreshGooseConfig()") >= 0);
    QVERIFY(mo->indexOfMethod("readGooseConfig(QString,bool)") >= 0);
    QVERIFY(mo->indexOfMethod("setGooseConfig(QString,QString,bool)") >= 0);
    QVERIFY(mo->indexOfMethod("removeGooseConfig(QString,bool)") >= 0);
    QVERIFY(mo->indexOfProperty("gooseConfig") >= 0);

    QVERIFY(QMetaObject::invokeMethod(&mgr, "refreshGooseConfig", Qt::DirectConnection));
    QVERIFY(QMetaObject::invokeMethod(&mgr, "readGooseConfig", Qt::DirectConnection,
                                      Q_ARG(QString, QStringLiteral("GOOSE_MODEL")),
                                      Q_ARG(bool, false)));
    QVERIFY(QMetaObject::invokeMethod(&mgr, "setGooseConfig", Qt::DirectConnection,
                                      Q_ARG(QString, QStringLiteral("GOOGLE_API_KEY")),
                                      Q_ARG(QString, QStringLiteral("topsecret")),
                                      Q_ARG(bool, true)));
    QVERIFY(QMetaObject::invokeMethod(&mgr, "removeGooseConfig", Qt::DirectConnection,
                                      Q_ARG(QString, QStringLiteral("gone")),
                                      Q_ARG(bool, false)));

    // Simulate an explicit plain read and a secret read. The plain one is
    // namespaced by the core; read-all's bare replies are handled by the same
    // ordinary path (checked after refresh below).
    mgr.coreOnConfigValue(QStringLiteral("config:GOOSE_MODEL"), QStringLiteral("deepseek"));
    // A secret read_ex reply is namespaced by the core; the editor strips the
    // marker before it reaches the shared config map.
    mgr.coreOnConfigValue(QStringLiteral("secret:GOOGLE_API_KEY"), QStringLiteral("topsecret"));

    const QVariantMap raw = mgr.serverConfig().toMap();
    QCOMPARE(raw.value(QStringLiteral("GOOSE_MODEL")).toString(), QStringLiteral("deepseek"));
    QVERIFY2(raw.value(QStringLiteral("GOOGLE_API_KEY")).toString() != QStringLiteral("topsecret"),
             "a secret's wire value must never land in the QML config map");

    const QVariantList rows = mgr.gooseConfigRows();
    QCOMPARE(rows.size(), 2);
    const QVariantMap secretRow = rows.at(0).toMap();
    QCOMPARE(secretRow.value(QStringLiteral("key")).toString(), QStringLiteral("GOOGLE_API_KEY"));
    QVERIFY(secretRow.value(QStringLiteral("secret")).toBool());
    QVERIFY2(secretRow.value(QStringLiteral("value")).toString() != QStringLiteral("topsecret"),
             "the editor must never prefill a secret with a value");
    const QVariantMap plainRow = rows.at(1).toMap();
    QCOMPARE(plainRow.value(QStringLiteral("key")).toString(), QStringLiteral("GOOSE_MODEL"));
    QVERIFY(!plainRow.value(QStringLiteral("secret")).toBool());
    QCOMPARE(plainRow.value(QStringLiteral("value")).toString(), QStringLiteral("deepseek"));

    // A refresh clears the cache before read_all; the remembered secret flag
    // keeps its (masked) row addressable, since read-all won't name it again.
    QVERIFY(QMetaObject::invokeMethod(&mgr, "refreshGooseConfig", Qt::DirectConnection));
    QVERIFY(mgr.serverConfig().toMap().isEmpty());
    const QVariantList after = mgr.gooseConfigRows();
    QCOMPARE(after.size(), 1);
    QVERIFY(after.at(0).toMap().value(QStringLiteral("secret")).toBool());
}

QTEST_MAIN(TstManager)
#include "tst_manager.moc"
