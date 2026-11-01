#include <QtTest/QtTest>
#include <QMetaMethod>
#include <QSignalSpy>

#include "corebridge.h"
#include "manager.h"
#include "messagelistmodel.h"

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
    void providerChoicesDisambiguateDuplicateLabels();
    void roamOpenClearsTheLoadingStatus();
    void runEndRendersTheLastStreamedBubble();
    void tailRendersWithoutAnySeal();
    void fencedCodeBecomesBlocksNotHtml();
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

void TstManager::roamOpenClearsTheLoadingStatus()
{
    Manager mgr;
    QSignalSpy spy(&mgr, &Manager::statusChanged);
    mgr.openRoamSession(QStringLiteral("Phaethon"),
                        QStringLiteral("roam:Phaethon:20261024_2"), QString());
    QCOMPARE(mgr.status(), QStringLiteral("loading…"));

    // A peer's open is only confirmed by the painted window: a SUCCESSFUL open raises
    // no other event, so without this the status stayed "loading…" forever (it showed
    // under the chat name and in the composer).
    mgr.coreOnItem(QStringLiteral(R"({"Window":{"oldest_id":"m1","has_older":true}})"));
    QCOMPARE(mgr.status(), QStringLiteral("ready"));

    // A later Window (a load-older batch) must not disturb a status that says
    // something meaningful — here, the visible failure a rejected open reports.
    mgr.coreOnSessionOpenFailed(QStringLiteral("sess-gone"),
                                QStringLiteral("session/load failed: stale"));
    const QString failure = mgr.status();
    QVERIFY2(failure.contains(QStringLiteral("stale")), qPrintable(failure));
    mgr.coreOnItem(QStringLiteral(R"({"Window":{"oldest_id":"m0","has_older":false}})"));
    QCOMPARE(mgr.status(), failure);
    Q_UNUSED(spy);
}

QTEST_MAIN(TstManager)
#include "tst_manager.moc"
