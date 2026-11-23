// SPDX-License-Identifier: AGPL-3.0-or-later

#include "settingsstore.h"
#include "manager.h"

#include <QDir>
#include <QRegularExpression>

SettingsStore::SettingsStore(Manager &manager)
    : m_manager(manager)
    , m_store(QSettings::UserScope, QStringLiteral("grouse"), QStringLiteral("grouse-desktop"))
{
}

QString SettingsStore::host() const { return m_store.value(QStringLiteral("host"), QStringLiteral("192.168.1.5")).toString(); }
QString SettingsStore::port() const { return m_store.value(QStringLiteral("port"), QStringLiteral("3284")).toString(); }
QString SettingsStore::secretKey() const { return m_store.value(QStringLiteral("secret"), QString()).toString().trimmed(); }
bool SettingsStore::useTls() const { return m_store.value(QStringLiteral("wss"), true).toBool(); }
bool SettingsStore::acceptInvalidCerts() const
{
    // A missing key used to mean trust-all; it now means WebPKI verification.
    // Self-signed hosts need the explicit opt-in from Settings once.
    return m_store.value(QStringLiteral("accept_invalid_certs"), DEFAULT_ACCEPT_INVALID_CERTS).toBool();
}
bool SettingsStore::autoConnectEnabled() const { return m_store.value(QStringLiteral("auto_connect"), true).toBool(); }
bool SettingsStore::notificationsEnabled() const { return m_store.value(QStringLiteral("notify_events"), true).toBool(); }
bool SettingsStore::configuredProvidersOnly() const { return m_store.value(QStringLiteral("configured_providers_only"), true).toBool(); }
bool SettingsStore::runSummaries() const { return m_store.value(QStringLiteral("run_summaries"), true).toBool(); }
bool SettingsStore::rescaleImages() const { return m_store.value(QStringLiteral("image_rescale"), true).toBool(); }
bool SettingsStore::advisor() const { return m_store.value(QStringLiteral("advisor"), false).toBool(); }
bool SettingsStore::debugTools() const { return m_store.value(QStringLiteral("debug_tools"), false).toBool(); }
QString SettingsStore::workingDir() const { return m_store.value(QStringLiteral("cwd"), QString()).toString(); }
bool SettingsStore::roamEnabled() const { return m_store.value(QStringLiteral("roam_enabled"), false).toBool(); }

void SettingsStore::setHost(const QString &v)
{
    m_store.setValue(QStringLiteral("host"), v);
    emit m_manager.settingsChanged();
}
void SettingsStore::setPort(const QString &v)
{
    m_store.setValue(QStringLiteral("port"), v);
    emit m_manager.settingsChanged();
}
void SettingsStore::setSecretKey(const QString &v)
{
    m_store.setValue(QStringLiteral("secret"), v.trimmed());
    emit m_manager.settingsChanged();
}
void SettingsStore::setUseTls(bool v)
{
    m_store.setValue(QStringLiteral("wss"), v);
    emit m_manager.settingsChanged();
}
void SettingsStore::setAcceptInvalidCerts(bool v)
{
    m_store.setValue(QStringLiteral("accept_invalid_certs"), v);
    emit m_manager.settingsChanged();
}
void SettingsStore::setAutoConnectEnabled(bool v)
{
    m_store.setValue(QStringLiteral("auto_connect"), v);
    emit m_manager.settingsChanged();
}
void SettingsStore::setNotificationsEnabled(bool v)
{
    m_store.setValue(QStringLiteral("notify_events"), v);
    emit m_manager.settingsChanged();
}
void SettingsStore::setConfiguredProvidersOnly(bool v)
{
    m_store.setValue(QStringLiteral("configured_providers_only"), v);
    emit m_manager.settingsChanged();
    emit m_manager.providersChanged();   // the pickers filter on this
}
void SettingsStore::setRunSummaries(bool v)
{
    m_store.setValue(QStringLiteral("run_summaries"), v);
    emit m_manager.settingsChanged();
    m_manager.syncRunSummariesToCore();
}
void SettingsStore::setRescaleImages(bool v)
{
    if (rescaleImages() == v)
        return;
    m_store.setValue(QStringLiteral("image_rescale"), v);
    emit m_manager.settingsChanged();
    m_manager.syncRescaleToCore();
}
void SettingsStore::setDebugTools(bool v)
{
    m_store.setValue(QStringLiteral("debug_tools"), v);
    emit m_manager.settingsChanged();
}
void SettingsStore::setAdvisor(bool v)
{
    if (advisor() == v)
        return;
    m_store.setValue(QStringLiteral("advisor"), v);
    emit m_manager.settingsChanged();
    m_manager.syncAdvisorToCore();
}
void SettingsStore::setWorkingDir(const QString &v)
{
    m_store.setValue(QStringLiteral("cwd"), v.trimmed().remove(QRegularExpression(QStringLiteral("/+$"))));
    emit m_manager.settingsChanged();
}
void SettingsStore::setRoamEnabled(bool v)
{
    if (roamEnabled() == v)
        return;
    m_store.setValue(QStringLiteral("roam_enabled"), v);
    emit m_manager.settingsChanged();
    // Enabling mid-session must bring stored peers up without a restart: the
    // Ready hook below only re-arms them once per launch.
    if (v && m_manager.online() && !m_manager.m_roamRestored) {
        m_manager.m_roamRestored = true;
        m_manager.syncRoamIdentityToCore();
        m_manager.restoreRoamPeers();
    }
}

QStringList SettingsStore::sidebarCollapsed() const
{
    return m_store.value(QStringLiteral("sidebar_collapsed")).toStringList();
}
void SettingsStore::setSidebarCollapsed(const QStringList &sections)
{
    m_store.setValue(QStringLiteral("sidebar_collapsed"), sections);
}
QString SettingsStore::roamIdentity() const
{
    return m_store.value(QStringLiteral("roam_identity")).toString();
}
QString SettingsStore::trimmedRoamIdentity() const
{
    return roamIdentity().trimmed();
}
void SettingsStore::setRoamIdentity(const QString &secret)
{
    m_store.setValue(QStringLiteral("roam_identity"), secret);
}
QVariantMap SettingsStore::roamCards() const
{
    return m_store.value(QStringLiteral("roam_cards")).toMap();
}
void SettingsStore::setRoamCards(const QVariantMap &cards)
{
    m_store.setValue(QStringLiteral("roam_cards"), cards);
}
QString SettingsStore::lastAttachDir() const
{
    return m_store.value(QStringLiteral("last_attach_dir"), QDir::homePath()).toString();
}
void SettingsStore::setLastAttachDir(const QString &dir)
{
    m_store.setValue(QStringLiteral("last_attach_dir"), dir);
}
QByteArray SettingsStore::toolCatalogs() const
{
    return m_store.value(QStringLiteral("tool_catalogs")).toString().toUtf8();
}
void SettingsStore::setToolCatalogs(const QByteArray &json)
{
    m_store.setValue(QStringLiteral("tool_catalogs"), QString::fromUtf8(json));
}
QVariantMap SettingsStore::cardPinsFor(const QString &sessionId) const
{
    return m_store.value(QStringLiteral("pinned_apps")).toMap().value(sessionId).toMap();
}
void SettingsStore::setCardPinsFor(const QString &sessionId, const QVariantMap &pins)
{
    QVariantMap all = m_store.value(QStringLiteral("pinned_apps")).toMap();
    all.insert(sessionId, pins);
    m_store.setValue(QStringLiteral("pinned_apps"), all);
}
