#include "roamtransport.h"

#include <QLibrary>
#include <QCoreApplication>
#include <QDebug>

namespace {

// Native ABI of libgrouse_roam_core (see the crate's src/capi.rs). All
// strings are malloc-allocated UTF-8 freed with stringFree; out_err receives a
// message on failure (freed the same way) and is left null on success.
struct RoamApi {
    char *(*identityGenerate)();
    char *(*identityPublicKey)(const char *, char **);
    char *(*cardFingerprint)(const char *, char **);
    void *(*roamConnect)(const char *, const char *, const char *, char **);
    long long (*streamRead)(void *, void *, unsigned long, char **);
    int (*streamWrite)(void *, const void *, unsigned long, char **);
    void (*streamShutdown)(void *);
    void (*streamCancel)(void *);
    void (*streamFree)(void *);
    void (*stringFree)(char *);
};

QLibrary *loadLibrary()
{
    static QLibrary *lib = [] {
        // Resolution order: dev override, then the app image (flatpak: /app/lib,
        // native: <appdir>/../lib), then the system search path.
        QStringList candidates;
        const QString overridePath = qEnvironmentVariable("GROUSE_ROAM_CORE");
        if (!overridePath.isEmpty())
            candidates << overridePath;
        candidates << QCoreApplication::applicationDirPath() + QStringLiteral("/../lib/libgrouse_roam_core.so")
                   << QStringLiteral("/app/lib/libgrouse_roam_core.so")
                   << QStringLiteral("grouse_roam_core");
        QLibrary *resolved = nullptr;
        for (const QString &c : candidates) {
            auto *l = new QLibrary(c);
            if (l->load()) {
                resolved = l;
                break;
            }
            delete l;
        }
        return resolved;
    }();
    return lib;
}

const RoamApi *api()
{
    static RoamApi *api = [] {
        QLibrary *lib = loadLibrary();
        if (!lib)
            return static_cast<RoamApi *>(nullptr);
        auto *a = new RoamApi;
        a->identityGenerate = reinterpret_cast<char *(*)()>(lib->resolve("grc_identity_generate"));
        a->identityPublicKey = reinterpret_cast<char *(*)(const char *, char **)>(lib->resolve("grc_identity_public_key"));
        a->cardFingerprint = reinterpret_cast<char *(*)(const char *, char **)>(lib->resolve("grc_card_fingerprint"));
        a->roamConnect = reinterpret_cast<void *(*)(const char *, const char *, const char *, char **)>(lib->resolve("grc_roam_connect"));
        a->streamRead = reinterpret_cast<long long (*)(void *, void *, unsigned long, char **)>(lib->resolve("grc_stream_read"));
        a->streamWrite = reinterpret_cast<int (*)(void *, const void *, unsigned long, char **)>(lib->resolve("grc_stream_write"));
        a->streamShutdown = reinterpret_cast<void (*)(void *)>(lib->resolve("grc_stream_shutdown"));
        a->streamCancel = reinterpret_cast<void (*)(void *)>(lib->resolve("grc_stream_cancel"));
        a->streamFree = reinterpret_cast<void (*)(void *)>(lib->resolve("grc_stream_free"));
        a->stringFree = reinterpret_cast<void (*)(char *)>(lib->resolve("grc_string_free"));
        const bool ok = a->identityGenerate && a->identityPublicKey && a->cardFingerprint
            && a->roamConnect && a->streamRead && a->streamWrite && a->streamShutdown
            && a->streamCancel && a->streamFree && a->stringFree;
        if (!ok) {
            delete a;
            return static_cast<RoamApi *>(nullptr);
        }
        return a;
    }();
    return api;
}

QString takeString(char *s)
{
    if (!s)
        return QString();
    const QString out = QString::fromUtf8(s);
    api()->stringFree(s);
    return out;
}

} // namespace

bool RoamTransport::isAvailable()
{
    return api() != nullptr;
}

QString RoamTransport::generateIdentity()
{
    const RoamApi *a = api();
    return a ? takeString(a->identityGenerate()) : QString();
}

QString RoamTransport::publicKeyFor(const QString &secret)
{
    const RoamApi *a = api();
    if (!a)
        return QString();
    char *err = nullptr;
    const QByteArray s = secret.toUtf8();
    const QString key = takeString(a->identityPublicKey(s.constData(), &err));
    if (err)
        api()->stringFree(err);
    return key;
}

QString RoamTransport::fingerprintFor(const QString &card)
{
    const RoamApi *a = api();
    if (!a)
        return QString();
    char *err = nullptr;
    const QByteArray c = card.toUtf8();
    const QString fp = takeString(a->cardFingerprint(c.constData(), &err));
    if (err)
        api()->stringFree(err);
    return fp;
}

RoamTransport::RoamTransport(const QString &secret, const QString &card, const QString &label,
                             QObject *parent)
    : AcpTransport(parent)
    , m_secret(secret)
    , m_card(card)
    , m_label(label)
{
}

RoamTransport::~RoamTransport()
{
    close();
    if (m_worker) {
        // cancel() unblocks the reader promptly, so the join is bounded.
        if (!m_worker->wait(5000))
            qWarning() << "RoamTransport: worker thread did not exit; leaking it";
        else
            delete m_worker;
    }
}

void RoamTransport::open()
{
    if (m_worker || !isAvailable())
        return;
    m_closed.storeRelaxed(0);
    m_worker = QThread::create([this] { workerRun(); });
    m_worker->start();
}

void RoamTransport::workerRun()
{
    const RoamApi *a = api();
    char *err = nullptr;
    const QByteArray secret = m_secret.toUtf8();
    const QByteArray card = m_card.toUtf8();
    const QByteArray label = m_label.toUtf8();

    void *h = a->roamConnect(secret.constData(), card.constData(),
                             m_label.isEmpty() ? nullptr : label.constData(), &err);
    if (!h) {
        const QString msg = takeString(err);
        emit error(msg.isEmpty() ? QStringLiteral("roam connect failed") : msg);
        emit closed(msg);
        return;
    }
    {
        QMutexLocker lock(&m_handleMutex);
        if (m_closed.loadRelaxed()) {
            // Closed while dialing: hand the handle back immediately.
            a->streamFree(h);
            return;
        }
        m_handle = h;
    }
    emit opened();

    RoamFrameCodec codec;
    QByteArray buf(16384, Qt::Uninitialized);
    QString closeReason;
    for (;;) {
        char *rerr = nullptr;
        const long long n = a->streamRead(h, buf.data(), buf.size(), &rerr);
        if (n < 0) {
            if (m_closed.loadRelaxed())
                break;   // cancelled by close()
            const QString msg = takeString(rerr);
            closeReason = msg.isEmpty() ? QStringLiteral("roam stream error") : msg;
            emit error(closeReason);
            break;
        }
        if (n == 0) {
            closeReason = m_closed.loadRelaxed() ? QString() : QStringLiteral("roam stream closed");
            break;
        }
        const QStringList frames = codec.feed(QByteArray(buf.constData(), int(n)));
        for (const QString &frame : frames)
            emit textReceived(frame);
    }

    {
        QMutexLocker lock(&m_handleMutex);
        m_handle = nullptr;
    }
    a->streamFree(h);
    emit closed(closeReason);
}

void RoamTransport::close()
{
    if (m_closed.fetchAndStoreOrdered(1))
        return;
    QMutexLocker lock(&m_handleMutex);
    if (m_handle) {
        api()->streamCancel(m_handle);   // unblock the reader
        api()->streamShutdown(m_handle); // FIN to the peer
    }
}

bool RoamTransport::sendText(const QString &text)
{
    if (m_closed.loadRelaxed())
        return false;
    const QByteArray frame = RoamFrameCodec().encode(text);
    QMutexLocker lock(&m_handleMutex);
    if (!m_handle)
        return false;
    char *err = nullptr;
    const int ok = api()->streamWrite(m_handle, frame.constData(), frame.size(), &err);
    if (!ok && err)
        api()->stringFree(err);
    return ok != 0;
}
