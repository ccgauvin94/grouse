#ifndef ACPTRANSPORT_H
#define ACPTRANSPORT_H

#include <QObject>
#include <QString>

/** Transport seam for the ACP wire: one JSON-RPC message per text frame,
 *  newline framing on byte streams (RoamTransport) or WS text messages
 *  (WebSocketTransport). AcpClient owns one transport per connection and
 *  speaks the same protocol over either.
 */
class AcpTransport : public QObject
{
    Q_OBJECT
public:
    explicit AcpTransport(QObject *parent = nullptr) : QObject(parent) {}

    /** Begin connecting. On success emits opened(); on failure emits error()
     *  followed by closed() with the reason. */
    virtual void open() = 0;
    /** Tear down the transport. Never emits afterwards. */
    virtual void close() = 0;
    /** Send one text frame (already JSON, no framing). False = transport dead. */
    virtual bool sendText(const QString &text) = 0;

signals:
    /** Ready to initialize the ACP session. */
    void opened();
    /** One complete inbound frame (message body only). */
    void textReceived(const QString &text);
    /** Transport went away: reason for the UI ("" for a clean close). */
    void closed(const QString &reason);
    /** Non-fatal? All transport errors here are terminal for the connection. */
    void error(const QString &message);
};

#endif // ACPTRANSPORT_H
