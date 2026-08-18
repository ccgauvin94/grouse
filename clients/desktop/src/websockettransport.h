#ifndef WEBSOCKETTRANSPORT_H
#define WEBSOCKETTRANSPORT_H

#include "acptransport.h"

#include <QUrl>

class QWebSocket;

/** ACP transport over a WebSocket to `ws(s)://host:port/acp`, authenticated by
 *  the X-Secret-Key header. TLS is deliberately trust-all: goosed uses a
 *  self-signed cert, we are tailnet-only.
 */
class WebSocketTransport : public AcpTransport
{
    Q_OBJECT
public:
    WebSocketTransport(const QUrl &url, const QString &secretKey, QObject *parent = nullptr);
    ~WebSocketTransport() override;

    void open() override;
    void close() override;
    bool sendText(const QString &text) override;

private:
    QUrl m_url;
    QString m_secretKey;
    QWebSocket *m_ws = nullptr;
};

#endif // WEBSOCKETTRANSPORT_H
