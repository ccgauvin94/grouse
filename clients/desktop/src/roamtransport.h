#ifndef ROAMTRANSPORT_H
#define ROAMTRANSPORT_H

#include "acptransport.h"
#include "roamframecodec.h"

#include <QAtomicInt>
#include <QMutex>
#include <QThread>

class QLibrary;

/** ACP transport over an iroh roam stream (the grouse-roam-core native
 *  library, dlopen'd). Mirrors the Android client's RoamStreamLink: the
 *  stream is pre-connected and authenticated (the dial + `roam peers accept`
 *  already happened), so open() just pumps newline-framed ACP frames.
 *
 *  Threading: a worker thread dials (blocking, seconds), then reads. close()
 *  interrupts a blocked read via the native cancel, so the worker exits
 *  promptly and the destructor can join it. sendText blocks on the write half
 *  (same as the Android client).
 */
class RoamTransport : public AcpTransport
{
    Q_OBJECT
public:
    RoamTransport(const QString &secret, const QString &card, const QString &label,
                  QObject *parent = nullptr);
    ~RoamTransport() override;

    void open() override;
    void close() override;
    bool sendText(const QString &text) override;

    /** True when the native library loaded and all symbols resolved. */
    static bool isAvailable();
    /** Fresh iroh secret key (base64). Empty on failure. */
    static QString generateIdentity();
    /** Hex public key for a secret — what a host sees in `peers list`. */
    static QString publicKeyFor(const QString &secret);
    /** Fingerprint of a connection card, for the pairing UI. */
    static QString fingerprintFor(const QString &card);

private:
    void workerRun();

    QString m_secret;
    QString m_card;
    QString m_label;
    QAtomicInt m_closed = 0;
    QMutex m_handleMutex;      // guards m_handle for close-vs-reader handoff
    void *m_handle = nullptr;  // native RoamStream handle, owned by the worker
    QThread *m_worker = nullptr;
};

#endif // ROAMTRANSPORT_H
