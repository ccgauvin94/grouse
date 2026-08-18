#include "websockettransport.h"

#include <QNetworkRequest>
#include <QSslConfiguration>
#include <QSslSocket>
#include <QtWebSockets/QWebSocket>

WebSocketTransport::WebSocketTransport(const QUrl &url, const QString &secretKey, QObject *parent)
    : AcpTransport(parent)
    , m_url(url)
    , m_secretKey(secretKey)
{
}

WebSocketTransport::~WebSocketTransport()
{
    if (m_ws)
        m_ws->deleteLater();
}

void WebSocketTransport::open()
{
    if (m_ws)
        m_ws->deleteLater();
    m_ws = new QWebSocket(QString(), QWebSocketProtocol::VersionLatest, this);
    // Trust-all TLS: goosed uses a self-signed cert, we are tailnet-only and authed by key.
    m_ws->setSslConfiguration([] {
        QSslConfiguration cfg;
        cfg.setPeerVerifyMode(QSslSocket::VerifyNone);
        return cfg;
    }());

    connect(m_ws, &QWebSocket::connected, this, &AcpTransport::opened);
    connect(m_ws, &QWebSocket::textMessageReceived, this,
            [this](const QString &msg) { emit textReceived(msg); });
    connect(m_ws, &QWebSocket::disconnected, this,
            [this] { emit closed(QString()); });
    connect(m_ws, QOverload<QAbstractSocket::SocketError>::of(&QWebSocket::errorOccurred),
            this, [this] { emit error(m_ws->errorString()); });

    QNetworkRequest req;
    req.setUrl(m_url);
    req.setHeader(QNetworkRequest::ContentTypeHeader, QStringLiteral("application/json"));
    req.setRawHeader("X-Secret-Key", m_secretKey.toUtf8());
    m_ws->open(req);
}

void WebSocketTransport::close()
{
    if (m_ws)
        m_ws->close();
}

bool WebSocketTransport::sendText(const QString &text)
{
    if (!m_ws || m_ws->state() != QAbstractSocket::ConnectedState)
        return false;
    m_ws->sendTextMessage(text);
    return true;
}
