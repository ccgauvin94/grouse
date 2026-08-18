#ifndef ROAMFRAMECODEC_H
#define ROAMFRAMECODEC_H

#include <QByteArray>
#include <QStringList>

/** ACP byte-stream framing (the ByteStreams component of agent-client-protocol):
 *  one JSON-RPC message per line, newline-terminated — the same framing goose
 *  uses on stdio and on the roam transport. JSON is serialized compactly, so a
 *  message can never contain a raw newline. Chunk-safe: partial frames stay
 *  buffered until their newline arrives; CRLF tolerated (BufReader::lines
 *  strips \r). Mirrors the Android client's RoamFrameCodec.
 */
class RoamFrameCodec
{
public:
    /** Feed raw stream bytes; returns every complete frame (newline removed). */
    QStringList feed(const QByteArray &chunk);
    /** One outbound frame: the JSON message plus its terminating newline. */
    QByteArray encode(const QString &text) const;

private:
    QByteArray m_buf;
};

#endif // ROAMFRAMECODEC_H
