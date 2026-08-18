// AcpClient wire tests: every ACP JSON-RPC frame the client sends and how it
// reacts to server frames, checked against an in-process fake goose. The
// protocol footguns (casing, _meta.client, cwd handling, explicit nulls,
// fallbacks) are the contract under test here.

#include <QtTest>
#include <QSignalSpy>

#include "acpclient.h"
#include "fakeserver.h"

class TstAcpClient : public QObject
{
    Q_OBJECT

private:
    FakeGooseServer *m_server = nullptr;
    AcpClient *m_client = nullptr;

    void connectClient(const QString &resumeId = QString(), const QString &resumeCwd = QString())
    {
        if (!resumeId.isEmpty())
            m_client->setResumeSession(resumeId, resumeCwd);
        m_client->setDesiredCwd(QStringLiteral("/srv/work"));
        m_client->connectTo(QStringLiteral("ws://127.0.0.1:%1/acp").arg(m_server->port()),
                            QStringLiteral("test-key"));
    }
    void waitReady()
    {
        QTRY_VERIFY_WITH_TIMEOUT(m_client->ready(), 5000);
    }

private slots:
    void init()
    {
        m_server = new FakeGooseServer;
        QVERIFY(m_server->start());
        m_client = new AcpClient;
        // Default script: accept initialize, open a session.
        m_server->onRequest(QStringLiteral("initialize"), [](const QJsonObject &) { return ok(); });
        m_server->onRequest(QStringLiteral("session/new"), [](const QJsonObject &) {
            return ok(QJsonObject{{"sessionId", "sess-1"},
                                  {"configOptions", QJsonArray()}});
        });
        m_server->onRequest(QStringLiteral("session/load"), [](const QJsonObject &) {
            return ok(QJsonObject{{"sessionId", "sess-9"},
                                  {"configOptions", QJsonArray()}});
        });
    }
    void cleanup()
    {
        delete m_client;
        m_client = nullptr;
        delete m_server;
        m_server = nullptr;
    }

    void initializeCarriesClientCapabilities();
    void newSessionCarriesClientAndCwd();
    void resumeLoadsWithModelCwd();
    void loadFailureFallsBackToNewSession();
    void recipeIdIsCarriedIntoNewSession();
    void promptSendsBlocksAndEndsRun();
    void promptBeforeReadyErrors();
    void promptRejectsEmptyText();
    void steerUsesActiveRunId();
    void permissionRoundTrip();
    void permissionCancelSendsCancelled();
    void recipeParamsAnsweredWithDefaults();
    void unknownServerRequestRepliesError();
    void streamingChunksEmitSignals();
    void chartToolCallEmitted();
    void mcpAppToolCallEmitted();
    void liveToolOutputAccumulates();
    void sessionListParsing();
    void configParsing();
    void setConfigOptionSends();
    void applyDesiredSendsProviderFirst();
    void applyDesiredSkipsUnchanged();
    void cancelSendsNotification();
    void sessionInfoUpdateCarriesActiveRun();
    void sessionProbeParsesReply();
    void sessionProbeErrorReportsStale();
    void projectsParsing();
    void recipesParsing();
    void schedulesParsing();
    void skillsParsing();
    void extensionsParsing();
    void toExtensionDtoConversions();
};

void TstAcpClient::initializeCarriesClientCapabilities()
{
    connectClient();
    QVERIFY(m_server->waitForFrame(QStringLiteral("initialize")));
    const QJsonObject frame = m_server->framesFor(QStringLiteral("initialize")).first();
    QVERIFY(frame.value("id").toInt(-1) > 0);
    const QJsonObject caps = frame.value("params").toObject()
                                 .value("clientCapabilities").toObject();
    QCOMPARE(caps.value("fs").toObject().value("readTextFile").toBool(), false);
    const QJsonObject goose = caps.value("_meta").toObject().value("goose").toObject();
    QVERIFY2(goose.value("recipeParameterRequests").toBool(),
             "recipeParameterRequests: session/new hard-fails for parameterized recipes without it");
    QVERIFY2(goose.value("customNotifications").toBool(), "customNotifications must be advertised");
    QVERIFY2(goose.value("toolCallLabelEnrichment").toBool(), "toolCallLabelEnrichment must be advertised");
    waitReady();
}

void TstAcpClient::newSessionCarriesClientAndCwd()
{
    connectClient();
    QVERIFY(m_server->waitForFrame(QStringLiteral("session/new")));
    const QJsonObject params = m_server->framesFor(QStringLiteral("session/new")).first()
                                   .value("params").toObject();
    // Without _meta.client the session is an `acp` session Desktop never lists.
    QCOMPARE(params.value("_meta").toObject().value("client").toString(),
             QStringLiteral("grouse-desktop"));
    // session/new requires an absolute cwd the server can use; never empty.
    QCOMPARE(params.value("cwd").toString(), QStringLiteral("/srv/work"));
    QVERIFY2(params.value("mcpServers").isArray(), "mcpServers must be an explicit empty array");
    waitReady();
    QCOMPARE(m_client->sessionId(), QStringLiteral("sess-1"));
}

void TstAcpClient::resumeLoadsWithModelCwd()
{
    connectClient(QStringLiteral("sess-9"), QStringLiteral("/real/cwd"));
    QVERIFY(m_server->waitForFrame(QStringLiteral("session/load")));
    const QJsonObject params = m_server->framesFor(QStringLiteral("session/load")).first()
                                   .value("params").toObject();
    // session/load silently rewrites working_dir from the cwd you send.
    QCOMPARE(params.value("sessionId").toString(), QStringLiteral("sess-9"));
    QCOMPARE(params.value("cwd").toString(), QStringLiteral("/real/cwd"));
    waitReady();
    QCOMPARE(m_client->sessionId(), QStringLiteral("sess-9"));
}

void TstAcpClient::loadFailureFallsBackToNewSession()
{
    // A stale/archived session can't be resumed: the client must fall back to a
    // fresh session rather than leave the app stuck on the failed load.
    m_server->onRequest(QStringLiteral("session/load"),
                        [](const QJsonObject &) { return fail(-32602, QStringLiteral("no such session")); });
    connectClient(QStringLiteral("sess-dead"), QStringLiteral("/cwd"));
    waitReady();
    QCOMPARE(m_client->sessionId(), QStringLiteral("sess-1"));
    QVERIFY2(m_server->indexOf(QStringLiteral("session/load")) < m_server->indexOf(QStringLiteral("session/new")),
             "session/load must be attempted before falling back to session/new");
}

void TstAcpClient::recipeIdIsCarriedIntoNewSession()
{
    m_client->setDesiredRecipeId(QStringLiteral("recipe-7"));
    connectClient();
    QVERIFY(m_server->waitForFrame(QStringLiteral("session/new")));
    const QJsonObject meta = m_server->framesFor(QStringLiteral("session/new")).first()
                                 .value("params").toObject().value("_meta").toObject();
    QCOMPARE(meta.value("recipeId").toString(), QStringLiteral("recipe-7"));
    waitReady();
}

void TstAcpClient::promptSendsBlocksAndEndsRun()
{
    m_server->onRequest(QStringLiteral("session/prompt"), [](const QJsonObject &) {
        return ok(QJsonObject{{"stopReason", "end_turn"}});
    });
    connectClient();
    waitReady();
    QSignalSpy endedSpy(m_client, &AcpClient::runEnded);
    const QVariantMap attachment{{"type", "image"}, {"mimeType", "image/png"},
                                 {"data", "aGVsbG8="}};
    m_client->sendPrompt(QStringLiteral("hello"), QVariantList{attachment});
    QVERIFY(m_server->waitForFrame(QStringLiteral("session/prompt")));
    const QJsonObject params = m_server->framesFor(QStringLiteral("session/prompt")).first()
                                   .value("params").toObject();
    QCOMPARE(params.value("sessionId").toString(), QStringLiteral("sess-1"));
    const QJsonArray prompt = params.value("prompt").toArray();
    QCOMPARE(prompt.size(), 2);
    QCOMPARE(prompt.at(0).toObject().value("type").toString(), QStringLiteral("text"));
    QCOMPARE(prompt.at(0).toObject().value("text").toString(), QStringLiteral("hello"));
    // Attachment content blocks pass through verbatim (image block).
    QCOMPARE(prompt.at(1).toObject().value("mimeType").toString(), QStringLiteral("image/png"));
    QCOMPARE(prompt.at(1).toObject().value("data").toString(), QStringLiteral("aGVsbG8="));
    QTRY_COMPARE(endedSpy.count(), 1);
    QCOMPARE(endedSpy.first().first().toString(), QStringLiteral("end_turn"));
}

void TstAcpClient::promptBeforeReadyErrors()
{
    // No connection at all: must fail loudly, not silently drop.
    QSignalSpy errorSpy(m_client, &AcpClient::error);
    m_client->sendPrompt(QStringLiteral("hi"));
    QCOMPARE(errorSpy.count(), 1);
    QVERIFY(errorSpy.first().first().toString().contains(QStringLiteral("not ready")));
}

void TstAcpClient::promptRejectsEmptyText()
{
    connectClient();
    waitReady();
    QSignalSpy errorSpy(m_client, &AcpClient::error);
    m_client->sendPrompt(QStringLiteral("   "));
    QCOMPARE(errorSpy.count(), 1);
    QVERIFY(errorSpy.first().first().toString().contains(QStringLiteral("empty prompt")));
    QVERIFY(m_server->framesFor(QStringLiteral("session/prompt")).isEmpty());
}

void TstAcpClient::steerUsesActiveRunId()
{
    connectClient();
    waitReady();
    QSignalSpy runSpy(m_client, &AcpClient::activeRunChanged);
    m_server->sendNotification(QStringLiteral("session/update"), QJsonObject{
        {"sessionId", "sess-1"},
        {"update", QJsonObject{
            {"sessionUpdate", "session_info_update"},
            {"title", "T"},
            {"updatedAt", "2026-08-09"},
            {"_meta", QJsonObject{{"goose", QJsonObject{{"activeRunId", "run-42"}}}}},
        }},
    });
    QTRY_COMPARE(runSpy.count(), 1);
    QCOMPARE(runSpy.first().at(1).toString(), QStringLiteral("run-42"));

    m_client->steer(QStringLiteral("continue"));
    QVERIFY(m_server->waitForFrame(QStringLiteral("_goose/unstable/session/steer")));
    const QJsonObject params = m_server->framesFor(QStringLiteral("_goose/unstable/session/steer")).first()
                                   .value("params").toObject();
    QCOMPARE(params.value("sessionId").toString(), QStringLiteral("sess-1"));
    QCOMPARE(params.value("expectedRunId").toString(), QStringLiteral("run-42"));
    QCOMPARE(params.value("prompt").toArray().at(0).toObject().value("text").toString(),
             QStringLiteral("continue"));
}

void TstAcpClient::permissionRoundTrip()
{
    connectClient();
    waitReady();
    QSignalSpy permSpy(m_client, &AcpClient::permissionRequest);
    m_server->sendServerRequest(QStringLiteral("session/request_permission"), QJsonObject{
        {"toolCall", QJsonObject{{"toolCallId", "tc-1"}, {"title", "Run shell"},
                                 {"rawInput", QJsonObject{{"command", "ls -la"}}}}},
        {"options", QJsonArray{
            QJsonObject{{"optionId", "allow_once"}, {"name", "Allow once"}, {"kind", "allowed"}},
            QJsonObject{{"optionId", "deny"}, {"name", "Deny"}, {"kind", "denied"}},
        }},
    }, 77);
    QTRY_COMPARE(permSpy.count(), 1);
    QCOMPARE(permSpy.first().at(0).toString(), QStringLiteral("tc-1"));
    QCOMPARE(permSpy.first().at(1).toString(), QStringLiteral("Run shell"));
    QCOMPARE(permSpy.first().at(2).toString(), QStringLiteral("ls -la"));
    QCOMPARE(permSpy.first().at(3).toList().size(), 2);

    m_client->respondPermission(QStringLiteral("tc-1"), QStringLiteral("allow_once"));
    QVERIFY(m_server->waitForFrames(m_server->frameCount() + 1));
    const QJsonObject reply = m_server->frames().last();
    QCOMPARE(reply.value("id").toInt(), 77);
    const QJsonObject outcome = reply.value("result").toObject().value("outcome").toObject();
    QCOMPARE(outcome.value("outcome").toString(), QStringLiteral("selected"));
    QCOMPARE(outcome.value("optionId").toString(), QStringLiteral("allow_once"));
}

void TstAcpClient::permissionCancelSendsCancelled()
{
    connectClient();
    waitReady();
    QSignalSpy permSpy(m_client, &AcpClient::permissionRequest);
    m_server->sendServerRequest(QStringLiteral("session/request_permission"), QJsonObject{
        {"toolCall", QJsonObject{{"toolCallId", "tc-2"}, {"title", "X"}}},
        {"options", QJsonArray()},
    }, 78);
    QTRY_COMPARE(permSpy.count(), 1);
    m_client->respondPermission(QStringLiteral("tc-2"), QString());
    QVERIFY(m_server->waitForFrames(m_server->frameCount() + 1));
    const QJsonObject outcome = m_server->frames().last().value("result").toObject()
                                    .value("outcome").toObject();
    QCOMPARE(outcome.value("outcome").toString(), QStringLiteral("cancelled"));
}

void TstAcpClient::recipeParamsAnsweredWithDefaults()
{
    connectClient();
    waitReady();
    m_server->sendServerRequest(QStringLiteral("_goose/unstable/session/recipe/request-params"), QJsonObject{
        {"parameters", QJsonArray{
            QJsonObject{{"key", "goal"}, {"default", "fix the bug"}},
            QJsonObject{{"key", "strict"}, {"default", "false"}},
        }},
    }, 10);
    QVERIFY(m_server->waitForFrames(m_server->frameCount() + 1));
    const QJsonObject reply = m_server->frames().last();
    QCOMPARE(reply.value("id").toInt(), 10);
    const QJsonObject result = reply.value("result").toObject();
    QCOMPARE(result.value("action").toString(), QStringLiteral("submit"));
    QCOMPARE(result.value("values").toObject().value("goal").toString(), QStringLiteral("fix the bug"));
}

void TstAcpClient::unknownServerRequestRepliesError()
{
    connectClient();
    waitReady();
    m_server->sendServerRequest(QStringLiteral("bogus/method"), QJsonObject(), 9);
    QVERIFY(m_server->waitForFrames(m_server->frameCount() + 1));
    const QJsonObject reply = m_server->frames().last();
    QCOMPARE(reply.value("id").toInt(), 9);
    // A real JSON-RPC error, not a silent empty result.
    QCOMPARE(reply.value("error").toObject().value("code").toInt(), -32601);
}

void TstAcpClient::streamingChunksEmitSignals()
{
    connectClient();
    waitReady();
    QSignalSpy agentSpy(m_client, &AcpClient::agentChunk);
    QSignalSpy userSpy(m_client, &AcpClient::userChunk);
    m_server->sendNotification(QStringLiteral("session/update"), QJsonObject{
        {"sessionUpdate", "agent_message_chunk"},
        {"content", QJsonObject{{"type", "text"}, {"text", "Hello"}}},
        {"_meta", QJsonObject{{"goose", QJsonObject{{"messageId", "m1"}}}}},
    });
    QTRY_COMPARE(agentSpy.count(), 1);
    QCOMPARE(agentSpy.first().at(0).toString(), QStringLiteral("Hello"));
    QCOMPARE(agentSpy.first().at(1).toString(), QStringLiteral("m1"));

    m_server->sendNotification(QStringLiteral("session/update"), QJsonObject{
        {"sessionUpdate", "user_message_chunk"},
        {"content", QJsonObject{{"type", "text"}, {"text", "echo"}}},
        {"_meta", QJsonObject{{"goose", QJsonObject{{"messageId", "m2"}}}}},
    });
    QTRY_COMPARE(userSpy.count(), 1);
    QCOMPARE(userSpy.first().at(0).toString(), QStringLiteral("echo"));
}

void TstAcpClient::chartToolCallEmitted()
{
    connectClient();
    waitReady();
    QSignalSpy chartSpy(m_client, &AcpClient::chartToolCall);
    QSignalSpy plainSpy(m_client, &AcpClient::toolCall);
    // Legacy path: data arrives as a JSON OBJECT (reading only the string form
    // silently disabled every chart once).
    m_server->sendNotification(QStringLiteral("session/update"), QJsonObject{
        {"sessionUpdate", "tool_call"},
        {"title", "Chart"},
        {"toolCallId", "tc-8"},
        {"rawInput", QJsonObject{{"data", QJsonObject{{"type", "bar"},
                                                     {"data", QJsonObject{{"labels", QJsonArray{"a"}}}}}}}},
        {"_meta", QJsonObject{{"goose", QJsonObject{
            {"toolCall", QJsonObject{{"toolName", "autovisualiser__show_chart"}}}}}}},
    });
    QTRY_COMPARE(chartSpy.count(), 1);
    QCOMPARE(chartSpy.first().at(0).toString(), QStringLiteral("Chart"));
    QCOMPARE(chartSpy.first().at(1).toString(), QStringLiteral("tc-8"));
    const QJsonObject spec = QJsonDocument::fromJson(
        chartSpy.first().at(2).toString().toUtf8()).object();
    QCOMPARE(spec.value("type").toString(), QStringLiteral("bar"));
    QCOMPARE(plainSpy.count(), 0);

    // A non-chart tool still arrives as a plain tool call.
    m_server->sendNotification(QStringLiteral("session/update"), QJsonObject{
        {"sessionUpdate", "tool_call"},
        {"title", "Shell"},
        {"toolCallId", "tc-7"},
        {"rawInput", QJsonObject{{"command", "ls"}}},
    });
    QTRY_COMPARE(plainSpy.count(), 1);
    QCOMPARE(plainSpy.first().at(1).toString(), QStringLiteral("ls"));
}

void TstAcpClient::mcpAppToolCallEmitted()
{
    connectClient();
    waitReady();
    QSignalSpy appSpy(m_client, &AcpClient::mcpAppToolCall);
    m_server->sendNotification(QStringLiteral("session/update"), QJsonObject{
        {"sessionUpdate", "tool_call"},
        {"title", "Render"},
        {"toolCallId", "tc-9"},
        {"rawInput", QJsonObject{{"command", "render"}}},
        {"_meta", QJsonObject{{"goose", QJsonObject{
            {"mcpApp", QJsonObject{{"resourceUri", "app://template"}, {"extensionName", "web"}}}}}}},
    });
    QTRY_COMPARE(appSpy.count(), 1);
    QCOMPARE(appSpy.first().at(0).toString(), QStringLiteral("Render"));
    QCOMPARE(appSpy.first().at(2).toString(), QStringLiteral("web|app://template"));
    QCOMPARE(appSpy.first().at(3).toString(), QStringLiteral("app://template"));
    QCOMPARE(appSpy.first().at(4).toString(), QStringLiteral("web"));
}

void TstAcpClient::liveToolOutputAccumulates()
{
    connectClient();
    waitReady();
    QSignalSpy updateSpy(m_client, &AcpClient::toolCallUpdate);
    // live_output chunks ride _meta.toolNotification; the manager ACCUMULATES
    // them instead of replacing the output.
    m_server->sendNotification(QStringLiteral("session/update"), QJsonObject{
        {"sessionUpdate", "tool_call_update"},
        {"toolCallId", "tc-5"},
        {"status", "running"},
        {"_meta", QJsonObject{{"toolNotification", QJsonObject{
            {"type", "live_output"},
            {"params", QJsonObject{{"chunks", QJsonArray{
                QJsonObject{{"output", QJsonObject{{"text", "line1\n"}}}},
                QJsonObject{{"output", QJsonObject{{"text", "line2"}}}},
            }}}},
        }}}},
    });
    QTRY_COMPARE(updateSpy.count(), 1);
    QCOMPARE(updateSpy.first().at(0).toString(), QStringLiteral("tc-5"));
    QCOMPARE(updateSpy.first().at(1).toString(), QStringLiteral("running"));
    QCOMPARE(updateSpy.first().at(2).toString(), QStringLiteral("line1\nline2"));
    QCOMPARE(updateSpy.first().at(3).toBool(), true);
}

void TstAcpClient::sessionListParsing()
{
    m_server->onRequest(QStringLiteral("session/list"), [](const QJsonObject &) {
        // One valid, one archived (client must drop), one scheduled job
        // (dropped), one roam session (peer extracted).
        return ok(QJsonObject{{"sessions", QJsonArray{
            QJsonObject{{"sessionId", "s1"}, {"title", "Chat one"},
                        {"updatedAt", "2026-08-01"}, {"cwd", "/srv/a"},
                        {"_meta", QJsonObject{{"lastMessageAt", "2026-08-01"},
                                              {"lastMessageSnippet", "hi"},
                                              {"messageCount", 3}, {"modelId", "m"}}}},
            QJsonObject{{"sessionId", "s2"}, {"title", "Gone"},
                        {"_meta", QJsonObject{{"archivedAt", "2026-08-02"}}}},
            QJsonObject{{"sessionId", "s3"}, {"title", "Scheduled job: nightly"}},
            QJsonObject{{"sessionId", "roam:alice:s9"}, {"title", "Remote"},
                        {"_meta", QJsonObject{{"projectId", "p1"}}}},
        }}});
    });
    connectClient();
    waitReady();
    QSignalSpy spy(m_client, &AcpClient::sessionsReady);
    m_client->listSessions();
    QVERIFY(m_server->waitForFrame(QStringLiteral("session/list")));
    // session/list must request both user and acp session types.
    const QJsonArray types = m_server->framesFor(QStringLiteral("session/list")).first()
                                 .value("params").toObject().value("_meta").toObject()
                                 .value("types").toArray();
    QCOMPARE(types.size(), 2);
    QTRY_COMPARE(spy.count(), 1);
    const QVariantList sessions = spy.first().at(0).toList();
    QCOMPARE(sessions.size(), 2);
    QCOMPARE(sessions.at(0).toMap().value("sessionId").toString(), QStringLiteral("s1"));
    QCOMPARE(sessions.at(0).toMap().value("messageCount").toInt(), 3);
    QCOMPARE(sessions.at(0).toMap().value("snippet").toString(), QStringLiteral("hi"));
    // roam ids carry the peer name for the REMOTE sidebar grouping.
    QCOMPARE(sessions.at(1).toMap().value("peer").toString(), QStringLiteral("alice"));
}

void TstAcpClient::configParsing()
{
    m_server->onRequest(QStringLiteral("session/set_config_option"), [](const QJsonObject &) {
        return ok(QJsonObject{{"configOptions", QJsonArray{
            QJsonObject{{"id", "provider"}, {"name", "Provider"}, {"currentValue", "openai"},
                        {"options", QJsonArray{
                            QJsonObject{{"value", "openai"}, {"name", "OpenAI"}},
                            QJsonObject{{"value", "anthropic"}, {"name", "Anthropic"}},
                        }}},
        }}});
    });
    QSignalSpy spy(m_client, &AcpClient::configReady);
    connectClient();
    waitReady();
    QCOMPARE(spy.count(), 1);   // session/new reply carried the (empty) config list
    m_client->setConfigOption(QStringLiteral("provider"), QStringLiteral("openai"));
    QVERIFY(m_server->waitForFrame(QStringLiteral("session/set_config_option")));
    QTRY_COMPARE(spy.count(), 2);
    const QVariantMap provider = spy.at(1).at(0).toList().at(0).toMap();
    QCOMPARE(provider.value("id").toString(), QStringLiteral("provider"));
    QCOMPARE(provider.value("currentValue").toString(), QStringLiteral("openai"));
    QCOMPARE(provider.value("choices").toList().size(), 2);
    QCOMPARE(provider.value("choices").toList().at(1).toMap().value("name").toString(),
             QStringLiteral("Anthropic"));
}

void TstAcpClient::setConfigOptionSends()
{
    connectClient();
    waitReady();
    m_client->setConfigOption(QStringLiteral("provider"), QStringLiteral("openai"));
    QVERIFY(m_server->waitForFrame(QStringLiteral("session/set_config_option")));
    const QJsonObject params = m_server->framesFor(QStringLiteral("session/set_config_option")).first()
                                   .value("params").toObject();
    QCOMPARE(params.value("sessionId").toString(), QStringLiteral("sess-1"));
    QCOMPARE(params.value("configId").toString(), QStringLiteral("provider"));
    QCOMPARE(params.value("value").toString(), QStringLiteral("openai"));
}

void TstAcpClient::applyDesiredSendsProviderFirst()
{
    // Landing-page choices are re-applied after a session opens: provider first
    // (the model cascade depends on it), then model.
    m_client->setDesiredOptions(QList<QPair<QString, QString>>{
        {QStringLiteral("provider"), QStringLiteral("openai")},
        {QStringLiteral("model"), QStringLiteral("gpt-4o")},
    });
    m_server->onRequest(QStringLiteral("session/new"), [](const QJsonObject &) {
        return ok(QJsonObject{{"sessionId", "sess-1"}, {"configOptions", QJsonArray{
            QJsonObject{{"id", "provider"}, {"currentValue", "anthropic"}, {"options", QJsonArray()}},
            QJsonObject{{"id", "model"}, {"currentValue", "claude"}, {"options", QJsonArray()}},
        }}});
    });
    connectClient();
    QVERIFY(m_server->waitForFrames(3));   // initialize + session/new + first set_config_option
    const QList<QJsonObject> opts = m_server->framesFor(QStringLiteral("session/set_config_option"));
    QTRY_COMPARE(opts.size(), 2);
    QCOMPARE(opts.at(0).value("params").toObject().value("configId").toString(),
             QStringLiteral("provider"));
    QCOMPARE(opts.at(1).value("params").toObject().value("configId").toString(),
             QStringLiteral("model"));
}

void TstAcpClient::applyDesiredSkipsUnchanged()
{
    m_client->setDesiredOptions(QList<QPair<QString, QString>>{
        {QStringLiteral("provider"), QStringLiteral("openai")},
    });
    m_server->onRequest(QStringLiteral("session/new"), [](const QJsonObject &) {
        return ok(QJsonObject{{"sessionId", "sess-1"}, {"configOptions", QJsonArray{
            QJsonObject{{"id", "provider"}, {"currentValue", "openai"}, {"options", QJsonArray()}},
        }}});
    });
    connectClient();
    QVERIFY(m_server->waitForFrame(QStringLiteral("session/new")));
    QTest::qWait(200);
    QVERIFY2(m_server->framesFor(QStringLiteral("session/set_config_option")).isEmpty(),
             "desired == current must not re-send set_config_option");
    waitReady();
}

void TstAcpClient::cancelSendsNotification()
{
    connectClient();
    waitReady();
    m_client->cancel();
    QVERIFY(m_server->waitForFrame(QStringLiteral("session/cancel")));
    const QJsonObject frame = m_server->framesFor(QStringLiteral("session/cancel")).first();
    // session/cancel is a notification: no request id, no reply expected.
    QVERIFY2(!frame.contains("id"), "cancel must be a notification, not a request");
    QCOMPARE(frame.value("params").toObject().value("sessionId").toString(), QStringLiteral("sess-1"));
}

void TstAcpClient::sessionProbeParsesReply()
{
    connectClient();
    waitReady();
    m_server->onRequest(QStringLiteral("_goose/unstable/session/info"), [](const QJsonObject &) {
        return ok(QJsonObject{{"session", QJsonObject{
            {"sessionId", "sess-1"},
            {"updatedAt", "2026-08-11T10:00:02Z"},
            {"_meta", QJsonObject{{"messageCount", 7}}},
        }}});
    });
    QSignalSpy spy(m_client, &AcpClient::sessionProbe);
    m_client->probeSession(QStringLiteral("sess-1"));
    QTRY_COMPARE(spy.count(), 1);
    QCOMPARE(spy.first().at(0).toString(), QStringLiteral("sess-1"));
    QCOMPARE(spy.first().at(1).toString(), QStringLiteral("2026-08-11T10:00:02Z"));
    QCOMPARE(spy.first().at(2).toInt(), 7);
    // The probe request carried the session id.
    QVERIFY(m_server->waitForFrame(QStringLiteral("_goose/unstable/session/info")));
}

void TstAcpClient::sessionProbeErrorReportsStale()
{
    connectClient();
    waitReady();
    m_server->onRequest(QStringLiteral("_goose/unstable/session/info"), [](const QJsonObject &) {
        return fail(-32602, QStringLiteral("no such session"));
    });
    QSignalSpy spy(m_client, &AcpClient::sessionProbe);
    m_client->probeSession(QStringLiteral("sess-9"));
    QTRY_COMPARE(spy.count(), 1);
    QCOMPARE(spy.first().at(0).toString(), QStringLiteral("sess-9"));
    QCOMPARE(spy.first().at(1).toString(), QString());
    QCOMPARE(spy.first().at(2).toInt(), -1);
}

void TstAcpClient::sessionInfoUpdateCarriesActiveRun()
{
    connectClient();
    waitReady();
    QSignalSpy touchedSpy(m_client, &AcpClient::sessionTouched);
    QSignalSpy runSpy(m_client, &AcpClient::activeRunChanged);
    m_server->sendNotification(QStringLiteral("session/update"), QJsonObject{
        {"sessionId", "sess-1"},
        {"update", QJsonObject{
            {"sessionUpdate", "session_info_update"},
            {"title", "New title"},
            {"updatedAt", "2026-08-09"},
            {"_meta", QJsonObject{{"goose", QJsonObject{{"activeRunId", "run-1"}}}}},
        }},
    });
    QTRY_COMPARE(runSpy.count(), 1);
    QCOMPARE(runSpy.first().at(0).toString(), QStringLiteral("sess-1"));
    QCOMPARE(runSpy.first().at(1).toString(), QStringLiteral("run-1"));
    QCOMPARE(touchedSpy.count(), 1);
    QCOMPARE(touchedSpy.first().at(1).toString(), QStringLiteral("New title"));
}

void TstAcpClient::projectsParsing()
{
    m_server->onRequest(QStringLiteral("_goose/unstable/sources/list"), [](const QJsonObject &) {
        return ok(QJsonObject{{"sources", QJsonArray{
            QJsonObject{{"path", "projects/alpha.md"}, {"name", "Alpha"},
                        {"description", "d"}, {"content", "root: /srv/alpha\nsome text"}},
            QJsonObject{{"path", "projects/beta.md"}, {"name", "Beta"},
                        {"content", "no root line"}},
        }}});
    });
    connectClient();
    waitReady();
    QSignalSpy spy(m_client, &AcpClient::projectsReady);
    m_client->listProjects();
    QVERIFY(m_server->waitForFrame(QStringLiteral("_goose/unstable/sources/list")));
    QTRY_COMPARE(spy.count(), 1);
    const QVariantList projects = spy.first().at(0).toList();
    QCOMPARE(projects.size(), 2);
    // The project id is the file stem of its source path.
    QCOMPARE(projects.at(0).toMap().value("id").toString(), QStringLiteral("alpha"));
    QCOMPARE(projects.at(0).toMap().value("root").toString(), QStringLiteral("/srv/alpha"));
    QCOMPARE(projects.at(1).toMap().value("id").toString(), QStringLiteral("beta"));
}

void TstAcpClient::recipesParsing()
{
    m_server->onRequest(QStringLiteral("_goose/unstable/recipes/list"), [](const QJsonObject &) {
        return ok(QJsonObject{{"recipes", QJsonArray{
            QJsonObject{{"id", "r1"}, {"file_path", "recipes/r1.md"}, {"schedule_cron", "0 9 * * *"},
                        {"recipe", QJsonObject{
                            {"title", "Nightly report"},
                            {"description", "desc"},
                            {"settings", QJsonObject{{"goose_provider", "openai"}, {"goose_model", "gpt-4o"}}},
                            {"parameters", QJsonArray{
                                QJsonObject{{"key", "goal"}, {"default", "x"}, {"input_type", "text"}},
                            }},
                        }}},
        }}});
    });
    connectClient();
    waitReady();
    QSignalSpy spy(m_client, &AcpClient::recipesReady);
    m_client->listRecipes();
    QVERIFY(m_server->waitForFrame(QStringLiteral("_goose/unstable/recipes/list")));
    QTRY_COMPARE(spy.count(), 1);
    const QVariantMap recipe = spy.first().at(0).toList().at(0).toMap();
    QCOMPARE(recipe.value("id").toString(), QStringLiteral("r1"));
    QCOMPARE(recipe.value("title").toString(), QStringLiteral("Nightly report"));
    // Recipes are snake_case on the wire (file_path, schedule_cron).
    QCOMPARE(recipe.value("filePath").toString(), QStringLiteral("recipes/r1.md"));
    QCOMPARE(recipe.value("cron").toString(), QStringLiteral("0 9 * * *"));
    QCOMPARE(recipe.value("provider").toString(), QStringLiteral("openai"));
    QCOMPARE(recipe.value("parameters").toList().at(0).toMap().value("inputType").toString(),
             QStringLiteral("text"));
}

void TstAcpClient::schedulesParsing()
{
    m_server->onRequest(QStringLiteral("_goose/unstable/schedules/list"), [](const QJsonObject &) {
        return ok(QJsonObject{{"jobs", QJsonArray{
            QJsonObject{{"id", "job-1"}, {"cron", "0 9 * * *"}, {"source", "recipes/r1.md"},
                        {"paused", false}, {"currentlyRunning", true}, {"lastRun", "2026-08-08"}},
        }}});
    });
    connectClient();
    waitReady();
    QSignalSpy spy(m_client, &AcpClient::schedulesReady);
    m_client->listSchedules();
    QVERIFY(m_server->waitForFrame(QStringLiteral("_goose/unstable/schedules/list")));
    QTRY_COMPARE(spy.count(), 1);
    const QVariantMap job = spy.first().at(0).toList().at(0).toMap();
    // Schedules are camelCase on the wire (`cron`, `currentlyRunning`), unlike recipes.
    QCOMPARE(job.value("cron").toString(), QStringLiteral("0 9 * * *"));
    QCOMPARE(job.value("running").toBool(), true);
    QCOMPARE(job.value("lastRun").toString(), QStringLiteral("2026-08-08"));
}

void TstAcpClient::skillsParsing()
{
    m_server->onRequest(QStringLiteral("_goose/unstable/sources/list"), [](const QJsonObject &) {
        return ok(QJsonObject{{"sources", QJsonArray{
            QJsonObject{{"name", "review"}, {"description", "Code review"},
                        {"content", "# review"}, {"path", "skills/review.md"},
                        {"global", true}, {"writable", false}},
        }}});
    });
    connectClient();
    waitReady();
    QSignalSpy spy(m_client, &AcpClient::skillsReady);
    m_client->listSkills();
    QVERIFY(m_server->waitForFrame(QStringLiteral("_goose/unstable/sources/list")));
    // The skill request must carry the skill type (and only that).
    QCOMPARE(m_server->framesFor(QStringLiteral("_goose/unstable/sources/list")).last()
                 .value("params").toObject().value("type").toString(),
             QStringLiteral("skill"));
    QTRY_COMPARE(spy.count(), 1);
    const QVariantMap skill = spy.first().at(0).toList().at(0).toMap();
    QCOMPARE(skill.value("name").toString(), QStringLiteral("review"));
    QCOMPARE(skill.value("writable").toBool(), false);   // bundled skills are read-only
    QCOMPARE(skill.value("global").toBool(), true);
}

void TstAcpClient::extensionsParsing()
{
    m_server->onRequest(QStringLiteral("_goose/unstable/config/extensions/list"), [](const QJsonObject &) {
        return ok(QJsonObject{{"extensions", QJsonArray{
            QJsonObject{{"extension", QJsonObject{{"name", "builtin-1"}, {"type", "builtin"}}},
                        {"enabled", true}},
            // mcp remote servers carry the name nested under server.name.
            QJsonObject{{"extension", QJsonObject{
                            {"type", "mcp"},
                            {"server", QJsonObject{{"name", "remote-ext"}}},
                            {"available_tools", QJsonArray{"remote-ext__tool1"}}}},
                        {"enabled", false}},
        }}});
    });
    connectClient();
    waitReady();
    QSignalSpy spy(m_client, &AcpClient::extensionsReady);
    m_client->listConfigExtensions();
    QVERIFY(m_server->waitForFrame(QStringLiteral("_goose/unstable/config/extensions/list")));
    QTRY_COMPARE(spy.count(), 1);
    const QVariantList extensions = spy.first().at(0).toList();
    QCOMPARE(extensions.size(), 2);
    QCOMPARE(extensions.at(1).toMap().value("name").toString(), QStringLiteral("remote-ext"));
    QCOMPARE(extensions.at(1).toMap().value("enabled").toBool(), false);
    QCOMPARE(extensions.at(1).toMap().value("attrib").toBool(), true);   // mcp => attributed
}

void TstAcpClient::toExtensionDtoConversions()
{
    // Listed remote extension -> accept-shaped DTO (feeding the listed shape
    // straight back is -32602).
    const QJsonObject listed{{"type", "streamable_http"}, {"name", "web"},
                             {"uri", "http://localhost:9999/mcp"},
                             {"headers", QJsonObject{{"X-Api-Key", "k"}}}};
    const QJsonObject dto = toExtensionDto(listed);
    QCOMPARE(dto.value("type").toString(), QStringLiteral("mcp"));
    const QJsonObject server = dto.value("server").toObject();
    QCOMPARE(server.value("type").toString(), QStringLiteral("http"));
    QCOMPARE(server.value("url").toString(), QStringLiteral("http://localhost:9999/mcp"));
    QCOMPARE(server.value("headers").toArray().at(0).toObject().value("name").toString(),
             QStringLiteral("X-Api-Key"));

    // stdio -> mcp/stdio with command/args.
    const QJsonObject stdio{{"type", "stdio"}, {"name", "dev"}, {"cmd", "npx"},
                            {"args", QJsonArray{"serve"}}};
    const QJsonObject sdto = toExtensionDto(stdio);
    QCOMPARE(sdto.value("type").toString(), QStringLiteral("mcp"));
    QCOMPARE(sdto.value("server").toObject().value("command").toString(), QStringLiteral("npx"));

    // builtin/platform/mcp pass through untouched.
    const QJsonObject builtin{{"type", "builtin"}, {"name", "b"}};
    QCOMPARE(toExtensionDto(builtin), builtin);
}

QTEST_MAIN(TstAcpClient)
#include "tst_acpclient.moc"
