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
};

namespace {
// Drives the bridge like a browser: a single HTTP/1.1 request/response, reading
// until the server closes (Connection: close). Same-thread server, so the
// client MUST pump the event loop (waitForReadyRead does not — it's designed
// for threads without one); without processEvents the server's newConnection
// is never delivered and every request times out.
QByteArray request(quint16 port, const QByteArray &line, const QByteArray &body = {})
{
    QTcpSocket s;
    s.connectToHost(QHostAddress::LocalHost, port);
    while (s.state() != QAbstractSocket::ConnectedState && s.state() != QAbstractSocket::UnconnectedState)
        QCoreApplication::processEvents();
    if (s.state() != QAbstractSocket::ConnectedState)
        return {};
    QByteArray out = line + "\r\nHost: 127.0.0.1\r\n";
    if (!body.isEmpty())
        out += "Content-Type: application/json\r\nContent-Length: "
               + QByteArray::number(body.size()) + "\r\n";
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
} // namespace

void TstAppBridge::servesTemplateWithShim()
{
    AppBridgeServer srv;
    QVERIFY(srv.start());
    srv.registerApp("t1", "sess", "<html><head><title>x</title></head><body>hi</body></html>",
                    "{}", "", "dark");
    const QByteArray r = request(srv.port(), "GET /page/t1 HTTP/1.1");
    QVERIFY(r.startsWith("HTTP/1.1 200"));
    // The shim must be injected, and the token baked in, but the app's own
    // markup preserved.
    QVERIFY(r.contains("__grouseHost"));
    QVERIFY(r.contains("ui/initialize"));
    QVERIFY(r.contains("var TK=\"t1\""));
    QVERIFY(r.endsWith("hi</body></html>") || r.contains("hi</body></html>"));
}

void TstAppBridge::stateCarriesHostContext()
{
    AppBridgeServer srv;
    QVERIFY(srv.start());
    srv.registerApp("t2", "sess", "<html><head></head></html>",
                    "{\"hours\":48}", "job health text", "light");
    const QByteArray r = request(srv.port(), "GET /state/t2 HTTP/1.1");
    const QByteArray json = r.mid(r.indexOf("\r\n\r\n") + 4);
    const QJsonObject o = QJsonDocument::fromJson(json).object();
    QCOMPARE(o.value("hostContext").toObject().value("theme").toString(), QStringLiteral("light"));
    QCOMPARE(o.value("toolInput").toObject().value("hours").toInt(), 48);
    QCOMPARE(o.value("toolResult").toObject().value("content").toArray()
             .at(0).toObject().value("text").toString(), QStringLiteral("job health text"));
}

void TstAppBridge::messageRelaysToSignal()
{
    AppBridgeServer srv;
    QVERIFY(srv.start());
    srv.registerApp("t3", "sess-42", "<html><head></head></html>", "{}", "", "dark");
    QSignalSpy spy(&srv, &AppBridgeServer::appMessage);
    const QByteArray r = request(srv.port(), "POST /message/t3 HTTP/1.1",
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

QTEST_GUILESS_MAIN(TstAppBridge)
#include "tst_appbridge.moc"
