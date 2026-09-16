#pragma once

#include <QObject>
#include <QString>
#include <QVariantMap>

class QTimer;
class Manager;

/**
 * UnifiedPush receive path for the desktop client.
 *
 * Grouse ships NO push sender. A stock `goose serve` has none, and inventing
 * server-side plumbing of our own would break the Grouse/Goose contract (see
 * docs/NOTIFICATIONS.md). This class only *registers* with whatever UnifiedPush
 * distributor the user runs, *receives* what the operator's own senders choose to
 * POST to that endpoint, and shows it — exactly like the phone client, which also
 * only receives. If nothing POSTs here, nothing happens and the app is unaffected.
 *
 * The wire contract is the phone's: a JSON envelope {type,session,text}; bare text
 * (no envelope) is a briefing.
 */
class PushClient : public QObject
{
    Q_OBJECT
    // The distributor calls back on the UnifiedPush connector interface; the export
    // name is what it looks up (the method names below stay capitalised for the same
    // reason — they are the spec's, not Qt's convention).
    Q_CLASSINFO("D-Bus Interface", "org.unifiedpush.Connector2")
    Q_PROPERTY(bool enabled READ enabled WRITE setEnabled NOTIFY enabledChanged)
    Q_PROPERTY(QString endpoint READ endpoint NOTIFY endpointChanged)
    Q_PROPERTY(QString status READ status NOTIFY statusChanged)
public:
    /** `manager` is the core bridge to the shared notification policy (notify.rs) and to
     *  the config write that publishes the endpoint. */
    explicit PushClient(Manager *manager, QObject *parent = nullptr);

    bool enabled() const { return m_enabled; }
    void setEnabled(bool on);
    QString endpoint() const { return m_endpoint; }
    QString status() const { return m_status; }

    /** Own our bus name, export the connector, and ask the distributor to register.
     *  Idempotent and safe to call on every startup (the endpoint can rotate). */
    void start();
    /** Bus-activated background mode: register, handle pushes without a window, and
     *  quit once one arrives (or after quitAfterMs of silence). */
    void runBackground(int quitAfterMs);

public slots:
    // org.unifiedpush.Connector2 — the distributor calls these. They must not block,
    // and every one of them returns the empty dictionary the spec requires.
    QVariantMap Message(const QVariantMap &args);
    QVariantMap NewEndpoint(const QVariantMap &args);
    QVariantMap Unregistered(const QVariantMap &args);

signals:
    void enabledChanged();
    void statusChanged();
    void endpointChanged();
    /** A fresh endpoint, for the config publication the operator's senders may use. */
    void endpointRegistered(const QString &endpoint);

private:
    /** Decode + decide through the core, then render. One policy for both clients. */
    void handlePush(const QString &raw);
    void setStatus(const QString &s);
    void setEndpoint(const QString &url);
    bool exportConnector();
    QString pickDistributor() const;
    QString token();          // persisted connection token (UUIDv4)
    void registerWithDistributor();
    void unregister();

    Manager *m_manager = nullptr;
    bool m_onlineHooked = false;
    bool m_enabled = true;
    QString m_endpoint;
    QString m_status = QStringLiteral("not registered");
    QTimer *m_quitCountdown = nullptr;
};
