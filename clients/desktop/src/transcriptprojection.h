// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once

#include <QElapsedTimer>
#include <QHash>
#include <QJsonObject>
#include <QList>
#include <QString>
#include <QVariantList>
#include <QVariantMap>

class CoreBridge;
class Manager;
class MessageListModel;
class QTimer;

/** The core's rich-item stream -> transcript rows (docs/TRANSCRIPT_MODEL.md).
 *
 *  Extracted from Manager: row shaping, attachment thumbnails, file cards,
 *  paint buffering and the item window. The QML-visible model, signals and
 *  coreOn* entry points stay on Manager; they forward into these methods.
 */
class TranscriptProjection
{
public:
    explicit TranscriptProjection(Manager &manager);

    void flushPaint();
    void requestMessagesUpdate();
    void finishTail(int lookback = 4);
    void renderTail();

    QVariantList attachmentsForItem(const QString &id, const QString &text, int count);
    QVariantMap rowFromItem(const QJsonObject &item);
    QVariantMap attachmentCard(const QString &path) const;
    QVariantMap serverFileCard(const QString &name, const QString &mime, qint64 bytes) const;
    static QString fileCardKey(const QString &sessionId, const QString &path);

    void applyItem(const QString &json);
    void applyFile(const QString &json);
    void loadOlder(int count);
    void fetchFile(const QString &sessionId, const QString &path, const QString &name,
                   const QString &mime, const QString &sha256);

private:
    // Aliases onto Manager's QML-facing state: no copy, so every write is still
    // immediately visible to the models and property getters.
    Manager &m_manager;
    CoreBridge *&m_bridge;
    MessageListModel *&m_messageModel;
    QHash<QString, QString> &m_appHtml;
    QString &m_currentSessionId;
    int &m_currentIndex;
    bool &m_painting;
    QList<QVariantMap> &m_paintRows;
    QTimer *&m_paintTimer;
    QTimer *&m_updateTimer;
    QTimer *&m_tailTimer;
    QString &m_tailRendered;
    QString &m_itemOldestId;
    bool &m_itemHasOlder;
    QList<QVariantMap> &m_olderPending;
    bool &m_loadingOlder;
    QHash<QString, QList<QVariantList>> &m_pendingAttachments;
    QHash<QString, QVariantList> &m_attachmentsByRow;
    QElapsedTimer &m_openClock;
    QString &m_openFor;
    QElapsedTimer &m_paintGap;

    // Thin forwards so the moved bodies read exactly as they did in Manager.
    void timingLog(const QString &line);
    void refreshActivity();
    void onError(const QString &text, bool background);
    QString scopedKey(const QString &sessionId, const QString &localKey) const;
    QString sessionPeerLabel(const QString &sessionId) const;
    QString fileCardKeyFor(const QString &sessionId, const QString &path) const;
};
