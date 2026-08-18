// Manager integration tests: Manager <-> fake goose over a real WebSocket,
// covering the flows the QML layer drives: connect, open, prompt/stream,
// permissions, queueing, and error surfacing.

#include <QtTest>
#include <QSignalSpy>
#include <QTemporaryDir>

#include "manager.h"
#include "messagelistmodel.h"
#include "sessionlistmodel.h"
#include "fakeserver.h"

class TstManager : public QObject
{
    Q_OBJECT

private:
    FakeGooseServer *m_server = nullptr;
    Manager *m_mgr = nullptr;
    QTemporaryDir m_home;   // XDG_CONFIG_HOME / XDG_CACHE_HOME redirection

    void setManagerEnv()
    {
        qputenv("XDG_CONFIG_HOME", (m_home.path() + "/config").toUtf8());
        qputenv("XDG_CACHE_HOME", (m_home.path() + "/cache").toUtf8());
    }

    /** Default script: initialize, open sess-1, empty lists everywhere else. */
    void defaultScript()
    {
        m_server->onRequest(QStringLiteral("initialize"), [](const QJsonObject &) { return ok(); });
        m_server->onRequest(QStringLiteral("session/new"), [](const QJsonObject &) {
            return ok(QJsonObject{{"sessionId", "sess-1"},
                                  {"configOptions", QJsonArray()}});
        });
        m_server->onRequest(QStringLiteral("session/load"), [](const QJsonObject &) {
            return ok(QJsonObject{{"sessionId", "sess-1"},
                                  {"configOptions", QJsonArray()}});
        });
        m_server->onRequest(QStringLiteral("session/list"), [](const QJsonObject &) {
            return ok(QJsonObject{{"sessions", QJsonArray{
                QJsonObject{{"sessionId", "sess-1"}, {"title", "Chat one"},
                            {"updatedAt", "2026-08-01"}, {"cwd", "/srv/work"},
                            {"_meta", QJsonObject{{"lastMessageAt", "2026-08-01"},
                                                  {"lastMessageSnippet", "hi"},
                                                  {"messageCount", 2}, {"modelId", "m"}}}},
            }}});
        });
        m_server->onRequest(QStringLiteral("_goose/unstable/sources/list"), [](const QJsonObject &) {
            return ok(QJsonObject{{"sources", QJsonArray()}});
        });
        m_server->onRequest(QStringLiteral("_goose/unstable/recipes/list"), [](const QJsonObject &) {
            return ok(QJsonObject{{"recipes", QJsonArray()}});
        });
        m_server->onRequest(QStringLiteral("_goose/unstable/schedules/list"), [](const QJsonObject &) {
            return ok(QJsonObject{{"jobs", QJsonArray()}});
        });
    }

    void connectManager()
    {
        m_mgr->setHost(QStringLiteral("127.0.0.1"));
        m_mgr->setPort(QString::number(m_server->port()));
        m_mgr->setUseTls(false);
        m_mgr->setSecretKey(QStringLiteral("test-key"));
        m_mgr->setWorkingDir(QStringLiteral("/srv/work"));
        m_mgr->connectToServer();
        QTRY_VERIFY_WITH_TIMEOUT(m_mgr->online(), 5000);
    }

private slots:
    void initTestCase()
    {
        setManagerEnv();
    }
    void init()
    {
        m_server = new FakeGooseServer;
        QVERIFY(m_server->start());
        defaultScript();
        m_mgr = new Manager;
    }
    void cleanup()
    {
        delete m_mgr;
        m_mgr = nullptr;
        delete m_server;
        m_server = nullptr;
    }

    void connectOpensSessionAndLists();
    void openSessionLoadsWithModelCwd();
    void sendPromptStreamsIntoModel();
    void permissionRoundTrip();
    void promptBeforeConnectQueuesThenFlushes();
    void cancelTurnDropsQueue();
    void setConfigOptionRoutes();
    void foregroundErrorBecomesBubble();
    void disconnectResetsState();
    void testConnectionSucceedsAgainstFakeServer();
    void testConnectionReportsRefused();
};

void TstManager::connectOpensSessionAndLists()
{
    connectManager();
    QCOMPARE(m_mgr->currentSessionId(), QStringLiteral("sess-1"));
    QCOMPARE(m_mgr->status(), QStringLiteral("ready"));
    // session/new carried the cwd + client marker the server depends on.
    const QJsonObject newParams = m_server->framesFor(QStringLiteral("session/new")).first()
                                      .value("params").toObject();
    QCOMPARE(newParams.value("cwd").toString(), QStringLiteral("/srv/work"));
    QCOMPARE(newParams.value("_meta").toObject().value("client").toString(),
             QStringLiteral("grouse-desktop"));
    // onReady refreshes the sidebar lists; the model shows header + session.
    auto *sessions = static_cast<SessionListModel *>(m_mgr->sessionsModel());
    QTRY_COMPARE(sessions->rowCount(), 2);
    QCOMPARE(sessions->data(sessions->index(0, 0), SessionListModel::HeaderRole).toBool(),
             true);
    QCOMPARE(sessions->data(sessions->index(1, 0), SessionListModel::SessionIdRole).toString(),
             QStringLiteral("sess-1"));
    QCOMPARE(m_mgr->sessions().toList().size(), 1);
}

void TstManager::openSessionLoadsWithModelCwd()
{
    connectManager();
    auto *sessions = static_cast<SessionListModel *>(m_mgr->sessionsModel());
    QTRY_COMPARE(sessions->rowCount(), 2);

    m_mgr->openSession(QStringLiteral("sess-1"));
    QVERIFY(m_server->waitForFrame(QStringLiteral("session/load")));
    const QJsonObject params = m_server->framesFor(QStringLiteral("session/load")).first()
                                   .value("params").toObject();
    // The session/load cwd must come from the model (sessions list), never
    // guessed — a wrong cwd silently re-files the chat.
    QCOMPARE(params.value("sessionId").toString(), QStringLiteral("sess-1"));
    QCOMPARE(params.value("cwd").toString(), QStringLiteral("/srv/work"));
    QTRY_COMPARE(m_mgr->currentSessionId(), QStringLiteral("sess-1"));
}

void TstManager::sendPromptStreamsIntoModel()
{
    m_server->onRequest(QStringLiteral("session/prompt"), [this](const QJsonObject &) {
        // A reply that streams: two agent chunks, then the run ends.
        m_server->sendNotification(QStringLiteral("session/update"), QJsonObject{
            {"sessionUpdate", "agent_message_chunk"},
            {"content", QJsonObject{{"type", "text"}, {"text", "Hello "}}},
            {"_meta", QJsonObject{{"goose", QJsonObject{{"messageId", "m1"}}}}},
        });
        m_server->sendNotification(QStringLiteral("session/update"), QJsonObject{
            {"sessionUpdate", "agent_message_chunk"},
            {"content", QJsonObject{{"type", "text"}, {"text", "world"}}},
            {"_meta", QJsonObject{{"goose", QJsonObject{{"messageId", "m1"}}}}},
        });
        return ok(QJsonObject{{"stopReason", "end_turn"}});
    });
    connectManager();
    auto *model = static_cast<MessageListModel *>(m_mgr->messageModel());

    m_mgr->sendPrompt(QStringLiteral("hello"));
    QVERIFY(m_server->waitForFrame(QStringLiteral("session/prompt")));
    const QJsonObject params = m_server->framesFor(QStringLiteral("session/prompt")).first()
                                   .value("params").toObject();
    QCOMPARE(params.value("sessionId").toString(), QStringLiteral("sess-1"));
    QCOMPARE(params.value("prompt").toArray().at(0).toObject().value("text").toString(),
             QStringLiteral("hello"));

    // The user bubble is appended synchronously; the agent reply streams in.
    QTRY_VERIFY(model->count() >= 2);
    QCOMPARE(model->row(0).value("role").toString(), QStringLiteral("user"));
    QCOMPARE(model->row(0).value("text").toString(), QStringLiteral("hello"));
    // runEnded finalizes the agent bubble: markdown rendered, prompting off.
    QTRY_VERIFY(!m_mgr->prompting());
    const QVariantMap agent = model->row(1);
    QCOMPARE(agent.value("role").toString(), QStringLiteral("agent"));
    QCOMPARE(agent.value("text").toString(), QStringLiteral("Hello world"));
    QVERIFY(!agent.value("html").toString().isEmpty());
}

void TstManager::permissionRoundTrip()
{
    connectManager();
    QSignalSpy permSpy(m_mgr, &Manager::permissionRequested);
    m_server->sendServerRequest(QStringLiteral("session/request_permission"), QJsonObject{
        {"toolCall", QJsonObject{{"toolCallId", "tc-1"}, {"title", "Run shell"},
                                 {"rawInput", QJsonObject{{"command", "ls"}}}}},
        {"options", QJsonArray{
            QJsonObject{{"optionId", "allow_once"}, {"name", "Allow once"}, {"kind", "allowed"}},
            QJsonObject{{"optionId", "deny"}, {"name", "Deny"}, {"kind", "denied"}},
        }},
    }, 5);
    QTRY_COMPARE(permSpy.count(), 1);
    QCOMPARE(m_mgr->permissionTitle(), QStringLiteral("Run shell"));
    QCOMPARE(m_mgr->permissionToolCallId(), QStringLiteral("tc-1"));
    QCOMPARE(m_mgr->permissionOptions().size(), 2);

    m_mgr->respondPermission(QStringLiteral("tc-1"), QStringLiteral("allow_once"));
    QVERIFY(m_server->waitForFrames(m_server->frameCount() + 1));
    const QJsonObject reply = m_server->frames().last();
    QCOMPARE(reply.value("id").toInt(), 5);
    QCOMPARE(reply.value("result").toObject().value("outcome").toObject()
                 .value("outcome").toString(),
             QStringLiteral("selected"));
    // A tool-role summary bubble appears in the transcript.
    auto *model = static_cast<MessageListModel *>(m_mgr->messageModel());
    QTRY_VERIFY(model->count() >= 1);
    const QVariantMap tool = model->row(model->count() - 1);
    QCOMPARE(tool.value("role").toString(), QStringLiteral("tool"));
    QCOMPARE(tool.value("output").toString(), QStringLiteral("permission granted"));
}

void TstManager::promptBeforeConnectQueuesThenFlushes()
{
    // Typing while the connection is still forming: the prompt must queue, then
    // flush once onReady arrives (session/new done). Manager::sendPrompt returns
    // early with NO client at all, so the queue path needs an in-flight connect.
    m_mgr->setHost(QStringLiteral("127.0.0.1"));
    m_mgr->setPort(QString::number(m_server->port()));
    m_mgr->setUseTls(false);
    m_mgr->setSecretKey(QStringLiteral("test-key"));
    m_mgr->setWorkingDir(QStringLiteral("/srv/work"));
    m_mgr->connectToServer();          // client exists, session not ready yet
    m_mgr->sendPrompt(QStringLiteral("queued"));
    QCOMPARE(m_mgr->queuedCount(), 1); // not ready -> queued, not sent
    QTRY_VERIFY_WITH_TIMEOUT(m_mgr->online(), 5000);
    QVERIFY(m_server->waitForFrame(QStringLiteral("session/prompt")));
    QCOMPARE(m_mgr->queuedCount(), 0);
    const QJsonObject params = m_server->framesFor(QStringLiteral("session/prompt")).first()
                                   .value("params").toObject();
    QCOMPARE(params.value("prompt").toArray().at(0).toObject().value("text").toString(),
             QStringLiteral("queued"));
}

void TstManager::cancelTurnDropsQueue()
{
    m_mgr->setHost(QStringLiteral("127.0.0.1"));
    m_mgr->setPort(QString::number(m_server->port()));
    m_mgr->setUseTls(false);
    m_mgr->setSecretKey(QStringLiteral("test-key"));
    m_mgr->setWorkingDir(QStringLiteral("/srv/work"));
    m_mgr->connectToServer();
    m_mgr->sendPrompt(QStringLiteral("never sent"));
    QCOMPARE(m_mgr->queuedCount(), 1);
    m_mgr->cancelTurn();
    QCOMPARE(m_mgr->queuedCount(), 0);
    QTRY_VERIFY_WITH_TIMEOUT(m_mgr->online(), 5000);
    QTest::qWait(300);
    QVERIFY2(m_server->framesFor(QStringLiteral("session/prompt")).isEmpty(),
             "cancel must drop the queue: no prompt may reach the server");
}

void TstManager::setConfigOptionRoutes()
{
    connectManager();
    m_mgr->setConfigOption(QStringLiteral("provider"), QStringLiteral("openai"));
    QVERIFY(m_server->waitForFrame(QStringLiteral("session/set_config_option")));
    const QJsonObject params = m_server->framesFor(QStringLiteral("session/set_config_option")).first()
                                   .value("params").toObject();
    QCOMPARE(params.value("sessionId").toString(), QStringLiteral("sess-1"));
    QCOMPARE(params.value("configId").toString(), QStringLiteral("provider"));
    QCOMPARE(params.value("value").toString(), QStringLiteral("openai"));
}

void TstManager::foregroundErrorBecomesBubble()
{
    m_server->onRequest(QStringLiteral("session/prompt"),
                        [](const QJsonObject &) { return fail(-32602, QStringLiteral("bad params")); });
    connectManager();
    auto *model = static_cast<MessageListModel *>(m_mgr->messageModel());
    m_mgr->sendPrompt(QStringLiteral("hello"));
    QVERIFY(m_server->waitForFrame(QStringLiteral("session/prompt")));
    // A foreground error (session/prompt) must surface as an error bubble.
    QTRY_VERIFY(model->count() >= 2);
    const QVariantMap last = model->row(model->count() - 1);
    QCOMPARE(last.value("role").toString(), QStringLiteral("error"));
    QVERIFY(last.value("text").toString().contains(QStringLiteral("session/prompt")));
    QVERIFY(m_mgr->status().contains(QStringLiteral("session/prompt")));
}

void TstManager::disconnectResetsState()
{
    connectManager();
    m_mgr->disconnect();
    QCOMPARE(m_mgr->online(), false);
    QCOMPARE(m_mgr->landingPage(), true);
    QCOMPARE(m_mgr->status(), QStringLiteral("disconnected"));
}

void TstManager::testConnectionSucceedsAgainstFakeServer()
{
    // The default script answers initialize with an empty result: a full
    // round-trip on a throwaway socket must report success.
    m_mgr->setHost(QStringLiteral("127.0.0.1"));
    m_mgr->setPort(QString::number(m_server->port()));
    m_mgr->setUseTls(false);
    m_mgr->setSecretKey(QStringLiteral("test-key"));
    QSignalSpy spy(m_mgr, &Manager::connectionTested);
    m_mgr->testConnection();
    QTRY_COMPARE(spy.count(), 1);
    QCOMPARE(spy.first().at(0).toBool(), true);
    QVERIFY(!spy.first().at(1).toString().isEmpty());
}

void TstManager::testConnectionReportsRefused()
{
    // Nothing listens on port 1: the probe must surface the socket error, not
    // hang or stay silent.
    m_mgr->setHost(QStringLiteral("127.0.0.1"));
    m_mgr->setPort(QStringLiteral("1"));
    m_mgr->setUseTls(false);
    m_mgr->setSecretKey(QStringLiteral("test-key"));
    QSignalSpy spy(m_mgr, &Manager::connectionTested);
    m_mgr->testConnection();
    QTRY_COMPARE_WITH_TIMEOUT(spy.count(), 1, 3000);
    QCOMPARE(spy.first().at(0).toBool(), false);
    QVERIFY(spy.first().at(1).toString().contains(QStringLiteral("failed")));
}

QTEST_MAIN(TstManager)
#include "tst_manager.moc"
