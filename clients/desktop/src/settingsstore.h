// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once

#include <QSettings>
#include <QString>
#include <QStringList>
#include <QVariantMap>

class Manager;

/** Every QSettings key and default the desktop client uses.
 *
 *  Extracted from Manager so a key is spelled in exactly one place. The setters
 *  only persist; the Manager's forwarding slot emits settingsChanged (and any
 *  core sync) so the QML surface is untouched.
 */
class SettingsStore
{
public:
    static constexpr bool DEFAULT_ACCEPT_INVALID_CERTS = false;

    explicit SettingsStore(Manager &manager);

    QString host() const;
    QString port() const;
    QString secretKey() const;
    bool useTls() const;
    bool acceptInvalidCerts() const;
    bool autoConnectEnabled() const;
    bool notificationsEnabled() const;
    bool configuredProvidersOnly() const;
    bool runSummaries() const;
    bool rescaleImages() const;
    bool advisor() const;
    bool debugTools() const;
    QString workingDir() const;
    bool roamEnabled() const;

    void setHost(const QString &v);
    void setPort(const QString &v);
    void setSecretKey(const QString &v);
    void setUseTls(bool v);
    void setAcceptInvalidCerts(bool v);
    void setAutoConnectEnabled(bool v);
    void setNotificationsEnabled(bool v);
    void setConfiguredProvidersOnly(bool v);
    void setRunSummaries(bool v);
    void setRescaleImages(bool v);
    void setAdvisor(bool v);
    void setDebugTools(bool v);
    void setWorkingDir(const QString &v);
    void setRoamEnabled(bool v);

    // Non-property state kept in the same store (sidebar, roam, caches, pins).
    QStringList sidebarCollapsed() const;
    void setSidebarCollapsed(const QStringList &sections);
    QString roamIdentity() const;                 // verbatim, as generated/stored
    QString trimmedRoamIdentity() const;           // used when dialing the core
    void setRoamIdentity(const QString &secret);
    QVariantMap roamCards() const;
    void setRoamCards(const QVariantMap &cards);
    QString lastAttachDir() const;
    void setLastAttachDir(const QString &dir);
    QByteArray toolCatalogs() const;
    void setToolCatalogs(const QByteArray &json);
    QVariantMap cardPinsFor(const QString &sessionId) const;
    void setCardPinsFor(const QString &sessionId, const QVariantMap &pins);

private:
    Manager &m_manager;
    QSettings m_store;
};
