#pragma once

#include <QObject>
#include <QVariantList>

class Manager;

/**
 * Session-bus bridge that lets the KRunner plugin drive the app without
 * touching its UI: list sessions, open one, start a new chat, query status.
 * Registers the service name `id.gauvin.Grouse` at `/id/gauvin/Grouse`.
 * If no session bus is available (or another instance owns the name) the
 * adapter simply stays unregistered and the runner falls back to launching.
 */
class DbusAdapter : public QObject
{
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "id.gauvin.Grouse")
public:
    explicit DbusAdapter(Manager *mgr, QObject *parent = nullptr);

    bool registered() const { return m_registered; }

public slots:
    /** Sessions as a JSON array of session maps. QtDBus marshals nested
     *  containers opaquely (a QVariantList/Map of maps arrives as unreadable
     *  QDBusArguments on the far side), so the wire shape is a flat string. */
    Q_SCRIPTABLE QString ListSessions() const;
    Q_SCRIPTABLE void OpenSession(const QString &sessionId);
    Q_SCRIPTABLE void NewChat();
    Q_SCRIPTABLE bool Online() const;
    Q_SCRIPTABLE QString Status() const;

private:
    Manager *m_mgr;
    bool m_registered = false;
};
