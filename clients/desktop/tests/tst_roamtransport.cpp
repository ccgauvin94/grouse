#include <QtTest>

#include "roamtransport.h"

/** RoamTransport against the real grouse-roam-core native library. Skipped
 *  when the library isn't loadable (e.g. CI without GROUSE_ROAM_CORE) — the
 *  guarded isAvailable() path is itself part of what's tested. The dead-relay
 *  dial exercises the full bind→dial→handshake plumbing and must fail
 *  cleanly, never hang or crash. */
class TstRoamTransport : public QObject
{
    Q_OBJECT
private slots:
    void identityRoundTrip();
    void garbageRejected();
    void cardFingerprint();
    void deadRelayFailsCleanly();
};

static bool available()
{
    return RoamTransport::isAvailable();
}

void TstRoamTransport::identityRoundTrip()
{
    if (!available())
        QSKIP("grouse-roam-core not loadable (set GROUSE_ROAM_CORE)");
    const QString secret = RoamTransport::generateIdentity();
    QVERIFY(!secret.isEmpty());
    const QString pk1 = RoamTransport::publicKeyFor(secret);
    const QString pk2 = RoamTransport::publicKeyFor(secret);
    QCOMPARE(pk1, pk2);
    QVERIFY(pk1.size() >= 16);   // hex node id
}

void TstRoamTransport::garbageRejected()
{
    if (!available())
        QSKIP("grouse-roam-core not loadable (set GROUSE_ROAM_CORE)");
    QVERIFY(RoamTransport::publicKeyFor(QStringLiteral("not-base64!!")).isEmpty());
    QVERIFY(RoamTransport::publicKeyFor(QString()).isEmpty());
}

void TstRoamTransport::cardFingerprint()
{
    if (!available())
        QSKIP("grouse-roam-core not loadable (set GROUSE_ROAM_CORE)");
    // A syntactically valid card (endpoint id + a dead relay) still fingerprints.
    const QString card = QStringLiteral("goose+roam://eyJ2ZXJzaW9uIjogMSwgImVuZHBvaW50X2lkIjogIjAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAiLCAicmVsYXlfdXJscyI6IFsiaHR0cHM6Ly8xMjcuMC4wLjE6OSJdfQ");
    const QString fp = RoamTransport::fingerprintFor(card);
    QVERIFY(!fp.isEmpty());
    QVERIFY(fp.contains(QLatin1Char('-')));
    QVERIFY(RoamTransport::fingerprintFor(QStringLiteral("junk")).isEmpty());
}

void TstRoamTransport::deadRelayFailsCleanly()
{
    if (!available())
        QSKIP("grouse-roam-core not loadable (set GROUSE_ROAM_CORE)");
    const QString card = QStringLiteral("goose+roam://eyJ2ZXJzaW9uIjogMSwgImVuZHBvaW50X2lkIjogIjAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAiLCAicmVsYXlfdXJscyI6IFsiaHR0cHM6Ly8xMjcuMC4wLjE6OSJdfQ");
    RoamTransport t(RoamTransport::generateIdentity(), card, QStringLiteral("tst"));
    QSignalSpy errorSpy(&t, &AcpTransport::error);
    QSignalSpy closedSpy(&t, &AcpTransport::closed);
    t.open();
    // The dial times out (~30s); the failure must arrive as error + closed.
    QVERIFY2(errorSpy.wait(60000), "expected a dial failure within 60s");
    QTRY_VERIFY_WITH_TIMEOUT(closedSpy.size() > 0, 5000);
    QVERIFY(!errorSpy.first().first().toString().isEmpty());
}

QTEST_GUILESS_MAIN(TstRoamTransport)
#include "tst_roamtransport.moc"
