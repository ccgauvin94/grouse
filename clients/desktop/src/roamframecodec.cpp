#include "roamframecodec.h"

QStringList RoamFrameCodec::feed(const QByteArray &chunk)
{
    QStringList out;
    for (char c : chunk) {
        switch (c) {
        case '\n':
            out << QString::fromUtf8(m_buf);
            m_buf.clear();
            break;
        case '\r':   // CRLF tolerance (BufReader::lines strips \r)
            break;
        default:
            m_buf.append(c);
        }
    }
    return out;
}

QByteArray RoamFrameCodec::encode(const QString &text) const
{
    return text.toUtf8() + '\n';
}
