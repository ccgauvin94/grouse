#include <QtTest>

#include "roamframecodec.h"

/** ACP byte-stream framing (ByteStreams): newline-delimited JSON-RPC, mirroring
 *  the Android client's RoamFrameCodecTest. Defends the framing contract the
 *  roam transport rides on — chunk boundaries must never matter. */
class TstRoamCodec : public QObject
{
    Q_OBJECT
private slots:
    void wholeFrame();
    void splitAcrossFeeds();
    void severalFramesInOneChunk();
    void crlfTolerated();
    void encodeTerminatesWithNewline();
    void utf8Preserved();
};

void TstRoamCodec::wholeFrame()
{
    RoamFrameCodec c;
    const QByteArray bytes = R"({"jsonrpc":"2.0","id":1,"method":"x"})" "\n";
    QCOMPARE(c.feed(bytes), QStringList{QStringLiteral(R"({"jsonrpc":"2.0","id":1,"method":"x"})")});
}

void TstRoamCodec::splitAcrossFeeds()
{
    RoamFrameCodec c;
    const QByteArray body = R"({"jsonrpc":"2.0","id":2})";
    QCOMPARE(c.feed(body.left(5)), QStringList());        // partial, no newline yet
    QCOMPARE(c.feed(body.mid(5)), QStringList());
    QCOMPARE(c.feed("\n"), QStringList{QStringLiteral(R"({"jsonrpc":"2.0","id":2})")});
}

void TstRoamCodec::severalFramesInOneChunk()
{
    RoamFrameCodec c;
    const QByteArray chunk = R"({"a":1})" "\n" R"({"a":2})" "\n";
    QCOMPARE(c.feed(chunk), QStringList({QStringLiteral(R"({"a":1})"),
                                         QStringLiteral(R"({"a":2})")}));
}

void TstRoamCodec::crlfTolerated()
{
    RoamFrameCodec c;
    QCOMPARE(c.feed("{\"a\":1}\r\n"), QStringList{QStringLiteral(R"({"a":1})")});
}

void TstRoamCodec::encodeTerminatesWithNewline()
{
    RoamFrameCodec c;
    QCOMPARE(c.encode(QStringLiteral(R"({"jsonrpc":"2.0"})")), QByteArray(R"({"jsonrpc":"2.0"})" "\n"));
}

void TstRoamCodec::utf8Preserved()
{
    RoamFrameCodec c;
    const QString body = QStringLiteral("{\"text\":\"grüße — über\"}");
    QCOMPARE(c.feed(c.encode(body)), QStringList{body});
}

QTEST_GUILESS_MAIN(TstRoamCodec)
#include "tst_roamcodec.moc"
