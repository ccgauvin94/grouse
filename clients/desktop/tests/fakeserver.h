#pragma once

// In-process stand-in for `goose serve`: a QWebSocketServer that speaks enough
// ACP JSON-RPC for the client tests. Handlers are scripted per method; every
// request frame the client sends is recorded so tests can assert on the wire
// shape (params, casing, _meta fields — the protocol footguns live there).
//
// Reply model:
//   - a request (frame with "id" + "method") gets {result, error} from the
//     scripted handler; default is an empty result.
//   - a notification (frame with "method", no "id") is recorded and forwarded
//     to the notificationReceived signal; no reply is sent.
//   - a response (frame with "id", no "method") is recorded and forwarded to
//     clientResponse (used to assert the client's answers to server requests).

// Deliberately NOT a Q_OBJECT (no signals/slots needed): this header is shared
// by several test binaries and AUTOMOC cannot moc a header-only class. It only
// needs QObject as a parent for the QWebSocketServer.

#include <QCoreApplication>
#include <QDeadlineTimer>
#include <QHostAddress>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QList>
#include <QObject>
#include <QThread>
#include <QWebSocket>
#include <QWebSocketServer>
#include <functional>

struct Reply {
    QJsonObject result;
    QJsonObject error;
    bool hasError() const { return !error.isEmpty(); }
};
inline Reply ok(const QJsonObject &r = QJsonObject()) { return {r, {}}; }
inline Reply fail(int code, const QString &message)
{
    return {{}, {{"code", code}, {"message", message}}};
}

class FakeGooseServer : public QObject
{
public:
    using Handler = std::function<Reply(const QJsonObject &params)>;

    /** Listen on 127.0.0.1 with an OS-assigned port. Returns false on failure. */
    bool start()
    {
        m_server = new QWebSocketServer(QStringLiteral("fake-goose"),
                                        QWebSocketServer::NonSecureMode, this);
        connect(m_server, &QWebSocketServer::newConnection,
                this, &FakeGooseServer::onNewConnection);
        return m_server->listen(QHostAddress::LocalHost, 0);
    }

    quint16 port() const { return m_server ? m_server->serverPort() : 0; }

    void setDefaultHandler(Handler h) { m_default = std::move(h); }
    void onRequest(const QString &method, Handler h) { m_handlers[method] = std::move(h); }

    /** Send a session/update notification (streaming chunks, usage, ...). */
    void sendNotification(const QString &method, const QJsonObject &params)
    {
        sendFrame({{"jsonrpc", "2.0"}, {"method", method}, {"params", params}});
    }
    /** Send a request the SERVER makes of the client (permission, recipe params). */
    void sendServerRequest(const QString &method, const QJsonObject &params, int id)
    {
        sendFrame({{"jsonrpc", "2.0"}, {"id", id}, {"method", method}, {"params", params}});
    }

    // --- recorded traffic -----------------------------------------------------
    const QList<QJsonObject> &frames() const { return m_frames; }
    int frameCount() const { return m_frames.size(); }
    /** All frames (requests + notifications) for a method, in arrival order. */
    QList<QJsonObject> framesFor(const QString &method) const
    {
        QList<QJsonObject> out;
        for (const auto &f : m_frames)
            if (f.value("method").toString() == method)
                out << f;
        return out;
    }
    /** Index of the first frame for `method` at or after `from`, or -1. */
    int indexOf(const QString &method, int from = 0) const
    {
        for (int i = from; i < m_frames.size(); ++i)
            if (m_frames.at(i).value("method").toString() == method)
                return i;
        return -1;
    }

    /** Pump the event loop until a frame for `method` arrives. */
    bool waitForFrame(const QString &method, int timeoutMs = 5000)
    {
        return waitFor([&] { return indexOf(method) >= 0; }, timeoutMs);
    }
    /** Pump the event loop until `n` frames have been recorded. */
    bool waitForFrames(int n, int timeoutMs = 5000)
    {
        return waitFor([&] { return m_frames.size() >= n; }, timeoutMs);
    }

private:
    template <typename Pred>
    bool waitFor(Pred pred, int timeoutMs)
    {
        const QDeadlineTimer deadline(timeoutMs);
        while (!pred()) {
            if (deadline.hasExpired())
                return false;
            QCoreApplication::processEvents(QEventLoop::AllEvents, 20);
            QThread::msleep(10);
        }
        return true;
    }

    void onNewConnection()
    {
        while (QWebSocket *s = m_server->nextPendingConnection()) {
            m_sockets << s;
            connect(s, &QWebSocket::textMessageReceived, this,
                    [this](const QString &msg) {
                        handleFrame(QJsonDocument::fromJson(msg.toUtf8()).object());
                    });
            connect(s, &QWebSocket::disconnected, this, [this, s] {
                m_sockets.removeAll(s);
                s->deleteLater();
            });
        }
    }

    void handleFrame(const QJsonObject &frame)
    {
        m_frames << frame;
        const QString method = frame.value("method").toString();
        const int id = frame.value("id").toInt(-1);
        const QJsonObject params = frame.value("params").toObject();
        if (method.isEmpty())
            return;   // response to a server request: recorded in m_frames
        if (id == -1)
            return;   // notification: recorded in m_frames, nothing to answer
        const Reply reply = [&] {
            auto it = m_handlers.find(method);
            if (it != m_handlers.end())
                return it.value()(params);
            if (m_default)
                return m_default(params);
            return Reply{};
        }();
        if (reply.hasError())
            sendFrame({{"jsonrpc", "2.0"}, {"id", id}, {"error", reply.error}});
        else
            sendFrame({{"jsonrpc", "2.0"}, {"id", id}, {"result", reply.result}});
    }

    void sendFrame(const QJsonObject &o)
    {
        const QByteArray data = QJsonDocument(o).toJson(QJsonDocument::Compact);
        for (QWebSocket *s : std::as_const(m_sockets))
            s->sendTextMessage(QString::fromUtf8(data));
    }

    QWebSocketServer *m_server = nullptr;
    QList<QWebSocket *> m_sockets;
    QList<QJsonObject> m_frames;
    Handler m_default;
    QHash<QString, Handler> m_handlers;
};
