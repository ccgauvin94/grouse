// SPDX-License-Identifier: AGPL-3.0-or-later
#include <QtTest/QtTest>
#include <QTcpSocket>
#include <QSignalSpy>

#include "appbridge.h"

/// Drives AppBridgeServer the way a browser would: raw HTTP over loopback.
class TstAppBridge : public QObject
{
    Q_OBJECT
private slots:
    void servesTemplateWithShim();
    void stateCarriesHostContext();
    void messageRelaysToSignal();
    void unknownTokenIsFourOhFour();
    void validBrowserHeadersPass();
    void foreignHostIsForbidden();
    void wrongLoopbackPortIsForbidden();
    void foreignOriginIsForbidden();
    void crossSiteFetchIsForbidden();
    void wrongContentTypeIsUnsupported();
    void tokensAreSessionScopedAndRevocable();
    void tokenExpiresAfterIdleTtl();
};

namespace {
// Drives the bridge like a browser: a single HTTP/1.1 request/response, reading
// until the server closes (Connection: close). Same-thread server, so the
// client MUST pump the event loop (waitForReadyRead does not — it's designed
// for threads without one); without processEvents the server's newConnection
// is never delivered and every request times out.
// `extra` lets a test forge hostile Host/Origin/Sec-Fetch-Site/Content-Type
// headers; `host` overrides the mandatory loopback Host.
QByteArray request(quint16 port, const QByteArray &line, const QByteArray &body = {},
                   const QByteArray &extra = {}, const QByteArray &host = "127.0.0.1")
{
    QTcpSocket s;
    s.connectToHost(QHostAddress::LocalHost, port);
    while (s.state() != QAbstractSocket::ConnectedState && s.state() != QAbstractSocket::UnconnectedState)
        QCoreApplication::processEvents();
    if (s.state() != QAbstractSocket::ConnectedState)
        return {};
    QByteArray out = line + "\r\nHost: " + host + "\r\n";
    if (!body.isEmpty() && !extra.contains("content-type:", Qt::CaseInsensitive))
        out += "Content-Type: application/json\r\n";
    if (!extra.isEmpty())
        out += extra;
    out += "Content-Length: " + QByteArray::number(body.size()) + "\r\n";
    out += "Connection: close\r\n\r\n";
    out += body;
    s.write(out);
    QByteArray resp;
    QElapsedTimer t;
    t.start();
    while (t.elapsed() < 5000) {
        QCoreApplication::processEvents();
        resp += s.readAll();
        if (s.state() == QAbstractSocket::UnconnectedState)
            break;   // peer closed after the full response (Connection: close)
    }
    resp += s.readAll();
    return resp;
}

QByteArray bodyOf(const QByteArray &response)
{
    return response.mid(response.indexOf("\r\n\r\n") + 4);
}
} // namespace

void TstAppBridge::servesTemplateWithShim()
{
    AppBridgeServer srv;
    QVERIFY(srv.start());
    const QString token = srv.registerApp(QStringLiteral("ext|uri"), QStringLiteral("sess"),
                                          QStringLiteral("<html><head><title>x</title></head><body>hi</body></html>"),
                                          QStringLiteral("{}"), QString(), QStringLiteral("dark"));
    QVERIFY(!token.isEmpty());
    const QByteArray r = request(srv.port(), "GET /page/" + token.toUtf8() + " HTTP/1.1");
    QVERIFY(r.startsWith("HTTP/1.1 200"));
    // The shim must be injected, and the token baked in, but the app's own
    // markup preserved.
    QVERIFY(r.contains("__grouseHost"));
    QVERIFY(r.contains("ui/initialize"));
    QVERIFY(r.contains(QByteArray("var TK=\"") + token.toUtf8() + "\""));
    QVERIFY(r.contains("hi</body></html>"));
}

void TstAppBridge::stateCarriesHostContext()
{
    AppBridgeServer srv;
    QVERIFY(srv.start());
    const QString token = srv.registerApp(QStringLiteral("ext|uri"), QStringLiteral("sess"),
                                          QStringLiteral("<html><head></head></html>"),
                                          QStringLiteral("{\"hours\":48}"),
                                          QStringLiteral("job health text"), QStringLiteral("light"));
    const QByteArray r = request(srv.port(), "GET /state/" + token.toUtf8() + " HTTP/1.1");
    const QJsonObject o = QJsonDocument::fromJson(bodyOf(r)).object();
    QCOMPARE(o.value("hostContext").toObject().value("theme").toString(), QStringLiteral("light"));
    QCOMPARE(o.value("toolInput").toObject().value("hours").toInt(), 48);
    QCOMPARE(o.value("toolResult").toObject().value("content").toArray()
             .at(0).toObject().value("text").toString(), QStringLiteral("job health text"));
}

void TstAppBridge::messageRelaysToSignal()
{
    AppBridgeServer srv;
    QVERIFY(srv.start());
    const QString token = srv.registerApp(QStringLiteral("ext|uri"), QStringLiteral("sess-42"),
                                          QStringLiteral("<html><head></head></html>"),
                                          QStringLiteral("{}"), QString(), QStringLiteral("dark"));
    QSignalSpy spy(&srv, &AppBridgeServer::appMessage);
    const QByteArray r = request(srv.port(), "POST /message/" + token.toUtf8() + " HTTP/1.1",
                                 "{\"text\":\"Summarize the dashboard\"}");
    QVERIFY(r.startsWith("HTTP/1.1 200"));
    QCOMPARE(spy.count(), 1);
    QCOMPARE(spy.at(0).at(0).toString(), QStringLiteral("sess-42"));
    QCOMPARE(spy.at(0).at(1).toString(), QStringLiteral("Summarize the dashboard"));
}

void TstAppBridge::unknownTokenIsFourOhFour()
{
    AppBridgeServer srv;
    QVERIFY(srv.start());
    const QByteArray r = request(srv.port(), "GET /page/t9 HTTP/1.1");
    QVERIFY(r.startsWith("HTTP/1.1 404"));
}

void TstAppBridge::validBrowserHeadersPass()
{
    AppBridgeServer srv;
    QVERIFY(srv.start());
    const QString token = srv.registerApp(QStringLiteral("ext|uri"), QStringLiteral("s1"),
                                          QStringLiteral("<html></html>"), QString(), QString(),
                                          QStringLiteral("dark"));
    const QByteArray extra = QByteArray("Origin: http://127.0.0.1:")
                             + QByteArray::number(srv.port())
                             + "\r\nSec-Fetch-Site: same-origin\r\n";
    const QByteArray r = request(srv.port(), "GET /page/" + token.toUtf8() + " HTTP/1.1",
                                 {}, extra);
    QVERIFY2(r.startsWith("HTTP/1.1 200"), r.constData());
}

void TstAppBridge::foreignHostIsForbidden()
{
    AppBridgeServer srv;
    QVERIFY(srv.start());
    const QString token = srv.registerApp(QStringLiteral("ext|uri"), QStringLiteral("s1"),
                                          QStringLiteral("<html></html>"), QString(), QString(),
                                          QStringLiteral("dark"));
    const QByteArray r = request(srv.port(), "GET /page/" + token.toUtf8() + " HTTP/1.1", {},
                                 {}, "evil.example");
    QVERIFY2(r.startsWith("HTTP/1.1 403"), r.constData());
    QVERIFY(!r.contains("__grouseHost"));
}

void TstAppBridge::wrongLoopbackPortIsForbidden()
{
    AppBridgeServer srv;
    QVERIFY(srv.start());
    const QString token = srv.registerApp(QStringLiteral("ext|uri"), QStringLiteral("s1"),
                                          QStringLiteral("<html></html>"), QString(), QString(),
                                          QStringLiteral("dark"));
    // A DNS-rebinding alias that points at loopback still names the WRONG
    // service if the port is not this bridge's ephemeral port.
    const QByteArray host = QByteArray("localhost:") + QByteArray::number(srv.port() + 1);
    const QByteArray r = request(srv.port(), "GET /page/" + token.toUtf8() + " HTTP/1.1",
                                 {}, {}, host);
    QVERIFY2(r.startsWith("HTTP/1.1 403"), r.constData());
}

void TstAppBridge::foreignOriginIsForbidden()
{
    AppBridgeServer srv;
    QVERIFY(srv.start());
    const QString token = srv.registerApp(QStringLiteral("ext|uri"), QStringLiteral("s1"),
                                          QStringLiteral("<html></html>"), QString(), QString(),
                                          QStringLiteral("dark"));
    const QByteArray r = request(srv.port(), "GET /page/" + token.toUtf8() + " HTTP/1.1",
                                 {}, "Origin: https://evil.example\r\n");
    QVERIFY2(r.startsWith("HTTP/1.1 403"), r.constData());
}

void TstAppBridge::crossSiteFetchIsForbidden()
{
    AppBridgeServer srv;
    QVERIFY(srv.start());
    const QString token = srv.registerApp(QStringLiteral("ext|uri"), QStringLiteral("s1"),
                                          QStringLiteral("<html></html>"), QString(), QString(),
                                          QStringLiteral("dark"));
    const QByteArray r = request(srv.port(), "GET /page/" + token.toUtf8() + " HTTP/1.1",
                                 {}, "Sec-Fetch-Site: cross-site\r\n");
    QVERIFY2(r.startsWith("HTTP/1.1 403"), r.constData());
}

void TstAppBridge::wrongContentTypeIsUnsupported()
{
    AppBridgeServer srv;
    QVERIFY(srv.start());
    const QString token = srv.registerApp(QStringLiteral("ext|uri"), QStringLiteral("s1"),
                                          QStringLiteral("<html></html>"), QString(), QString(),
                                          QStringLiteral("dark"));
    QSignalSpy spy(&srv, &AppBridgeServer::appMessage);
    const QByteArray r = request(srv.port(), "POST /message/" + token.toUtf8() + " HTTP/1.1",
                                 "{\"text\":\"nope\"}", "Content-Type: text/plain\r\n");
    QVERIFY2(r.startsWith("HTTP/1.1 415"), r.constData());
    QCOMPARE(spy.count(), 0);   // no prompt may reach the chat on a bad POST
}

void TstAppBridge::tokensAreSessionScopedAndRevocable()
{
    AppBridgeServer srv;
    QVERIFY(srv.start());
    // The SAME app in two chats must mint two tokens, each carrying its own
    // session id into appMessage. appKey alone is not the scope.
    const QString a = srv.registerApp(QStringLiteral("ext|uri"), QStringLiteral("chat-A"),
                                      QStringLiteral("<html></html>"), QString(), QString(),
                                      QStringLiteral("dark"));
    const QString b = srv.registerApp(QStringLiteral("ext|uri"), QStringLiteral("chat-B"),
                                      QStringLiteral("<html></html>"), QString(), QString(),
                                      QStringLiteral("dark"));
    QVERIFY(!a.isEmpty() && !b.isEmpty());
    QVERIFY(a != b);
    QCOMPARE(srv.activeTokenCount(), 2);
    QSignalSpy spy(&srv, &AppBridgeServer::appMessage);
    QVERIFY(request(srv.port(), "POST /message/" + a.toUtf8() + " HTTP/1.1",
                    "{\"text\":\"from A\"}").startsWith("HTTP/1.1 200"));
    QVERIFY(request(srv.port(), "POST /message/" + b.toUtf8() + " HTTP/1.1",
                    "{\"text\":\"from B\"}").startsWith("HTTP/1.1 200"));
    QCOMPARE(spy.count(), 2);
    QCOMPARE(spy.at(0).at(0).toString(), QStringLiteral("chat-A"));
    QCOMPARE(spy.at(1).at(0).toString(), QStringLiteral("chat-B"));

    // A refetched template in chat A revokes A's token only; B stays usable.
    const QString a2 = srv.registerApp(QStringLiteral("ext|uri"), QStringLiteral("chat-A"),
                                       QStringLiteral("<html>new</html>"), QString(), QString(),
                                       QStringLiteral("dark"));
    QVERIFY(a2 != a && a2 != b);
    QCOMPARE(srv.activeTokenCount(), 2);
    QVERIFY(request(srv.port(), "GET /page/" + a.toUtf8() + " HTTP/1.1").startsWith("HTTP/1.1 404"));
    QVERIFY(request(srv.port(), "GET /page/" + b.toUtf8() + " HTTP/1.1").startsWith("HTTP/1.1 200"));
    QVERIFY(request(srv.port(), "GET /page/" + a2.toUtf8() + " HTTP/1.1").startsWith("HTTP/1.1 200"));

    srv.revokeToken(b);
    QVERIFY(request(srv.port(), "GET /page/" + b.toUtf8() + " HTTP/1.1").startsWith("HTTP/1.1 404"));
    QCOMPARE(srv.activeTokenCount(), 1);
}

void TstAppBridge::tokenExpiresAfterIdleTtl()
{
    AppBridgeServer srv;
    QVERIFY(srv.start());
    srv.setTokenTtlMs(200);
    const QString token = srv.registerApp(QStringLiteral("ext|uri"), QStringLiteral("chat-A"),
                                          QStringLiteral("<html></html>"), QString(), QString(),
                                          QStringLiteral("dark"));
    QVERIFY(request(srv.port(), "GET /page/" + token.toUtf8() + " HTTP/1.1").startsWith("HTTP/1.1 200"));
    // A valid request slides the TTL: stay just under it twice, then idle past it.
    QTest::qWait(80);
    QVERIFY(request(srv.port(), "GET /page/" + token.toUtf8() + " HTTP/1.1").startsWith("HTTP/1.1 200"));
    QTest::qWait(260);
    QVERIFY(!srv.isTokenValid(token));
    QVERIFY(request(srv.port(), "GET /page/" + token.toUtf8() + " HTTP/1.1").startsWith("HTTP/1.1 404"));
    QCOMPARE(srv.activeTokenCount(), 0);
}

QTEST_GUILESS_MAIN(TstAppBridge)
#include "tst_appbridge.moc"
