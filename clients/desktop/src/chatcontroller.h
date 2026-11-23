// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once

#include <QElapsedTimer>
#include <QHash>
#include <functional>
#include <QList>
#include <QSet>
#include <QString>
#include <QVariantList>
#include <QVariantMap>

class CoreBridge;
class Manager;
class MessageListModel;
class QThreadPool;
class RoamListModel;
class SessionListModel;

/** Connection, chat switching, turn/queue state and the attachment pipeline.
 *
 *  Extracted from Manager so connect/open, park/steer/flush and file selection
 *  live in one place. Manager stays the QML facade and the coreOn* dispatcher;
 *  the shared state is held by Manager and referenced here (no copies), which
 *  keeps every Q_PROPERTY getter and every signal emission observable exactly
 *  where QML expects it.
 */
class ChatController
{
public:
    struct PendingSend {
        QString text;
        QVariantList images;
        QStringList files;
        QString sessionId;
        int id = 0;
    };

    explicit ChatController(Manager &manager);
    ~ChatController();

    // --- connection / roam ----------------------------------------------------
    void connectToServer();
    void autoConnect();
    void disconnect();
    void connectRoam(const QString &card, const QString &label);
    void syncRoamIdentityToCore();
    void disconnectRoam(const QString &label);
    void persistRoamCard(const QString &label, const QString &card);
    void forgetRoamCard(const QString &label);
    void restoreRoamPeers();
    QVariantList roamCardPins();
    bool clearRoamCardPin(const QString &peer, const QString &confirmation);
    bool redialRoamPeer(const QString &label);
    void testConnection();

    // --- chats ---------------------------------------------------------------
    void openSession(const QString &sessionId);
    QString sessionStamp(const QString &sessionId) const;
    void refreshSession();
    void reloadSessionFromStart();
    void refreshSessionWith(int full);
    void newChat();
    void newChatInMode(const QString &mode);
    void beginChat();
    void openRoamSession(const QString &label, const QString &sessionId, const QString &cwd);
    void newRoamSession(const QString &label);
    void newRoamSessionIn(const QString &label, const QString &cwd);
    void newRoamSessionWithMode(const QString &label, const QString &cwd, const QString &mode);
    void setActiveTab(const QString &tab);
    QString roamIdentity();
    QString roamPublicKey() const;
    QString roamCard() const;

    // --- turns / queue -------------------------------------------------------
    void sendPrompt(const QString &text, const QVariantList &files);
    void dispatchSend(const QString &text, const QVariantList &blocks,
                      const QStringList &files = {});
    bool wireUpForCurrentChat() const;
    void cancelTurn();
    void compactConversation();
    void steerQueued(int id);
    void cancelQueued(int id);
    int queuedCount() const;
    QVariant queuedSends() const;
    void restoreTurnState(const QString &sessionId);
    void setActiveRunId(const QString &runId);
    void releaseTurnForLostWire(const QString &lostSessionId);
    void flushQueue();
    void enqueue(const PendingSend &p);
    void bindSessionAfterAttach();
    bool mainWireConnected() const;

    // --- attachments ---------------------------------------------------------
    QStringList pickAttachmentFiles();
    QStringList clipboardAttachmentFiles();
    QVariantList buildAttachmentBlocks(const QVariantList &paths);
    QStringList attachmentPaths(const QVariantList &paths);

    // --- core-event fold (Manager's coreOn* names forward here) --------------
    void onStatus(const QString &json);
    void onSessionOpened(const QString &sessionId);
    void onSessionOpenFailed(const QString &sessionId, const QString &message);
    void onRunEnded(const QString &sessionId, const QString &stopReason);
    void onActiveRunChanged(const QString &sessionId, const QString &runId);
    void onCompactionStatus(const QString &message);

    /** Blocking core intents run on ONE worker, in click order. */
    void runOnCore(std::function<void()> work);
    void waitForCore();

private:
    Manager &m_manager;
    CoreBridge *&m_bridge;
    RoamListModel *&m_roamModel;
    MessageListModel *&m_messageModel;
    QVariantList &m_sessions;
    QHash<QString, QString> &m_roamTitles;
    QStringList &m_tools;
    QString &m_status;
    bool &m_online;
    bool &m_prompting;
    bool &m_roamRestored;
    QString &m_activePeerLabel;
    QString &m_promptingSessionId;
    QSet<QString> &m_inFlight;
    QHash<QString, QString> &m_activeRunIds;
    QString &m_activeRunId;
    bool &m_compacting;
    QString &m_compactionStatus;
    QString &m_currentSessionId;
    QString &m_currentSessionTitle;
    bool &m_landing;
    QString &m_lastCwd;
    bool &m_wireOnlyConnect;
    bool &m_testPending;
    QString &m_pendingRecipeId;
    QString &m_pendingProjectFiling;
    QElapsedTimer &m_openClock;
    QString &m_openFor;
    QThreadPool *&m_io;
    QList<PendingSend> &m_pendingQueue;
    int &m_sendSeq;
    QHash<QString, QList<QVariantList>> &m_pendingAttachments;

    // Thin forwards to the QML facade / other extracted units.
    QString host() const;
    QString port() const;
    QString secretKey() const;
    bool useTls() const;
    bool autoConnectEnabled() const;
    bool roamEnabled() const;
    QString workingDir() const;
    bool mainWireOwnsTurnState(const QString &activePeerLabel) const;
    void timingLog(const QString &line);
    void setStatus(const QString &s);
    void setOnline(bool o);
    void setMyDayVisible(bool v);
    void resetReloadProgress();
    void resetContextUsage();
    void refreshSessions();
    void refreshProjects();
    void refreshRecipes();
    void refreshProviders();
    void refreshToolGroups();
    void syncRunSummariesToCore();
    void syncAdvisorToCore();
    void syncRescaleToCore();
    void maybeShowCardPinRefusal(const QString &status, const QString &peerLabel);
    void dismissRoamPinRefusal();
    void onError(const QString &text, bool background);
    void notifyTurnFinished();
    void refreshActivity();
    void finishTail(int lookback = 4);
    void dismissAdvisorReport();
    void loadToolCache(const QString &sessionId);
    void publishToolGroups();
    QString sessionPeerLabel(const QString &sessionId) const;
    QString activeSessionId() const;
    bool sessionIsOpen() const;
    QString serverConfigJson();
    QString promptJson(const QString &text, const QVariantList &blocks) const;
    QString scopedKey(const QString &sessionId, const QString &localKey) const;

    QVariantMap roamCards() const;
    void setRoamCards(const QVariantMap &cards);
    QString storedRoamIdentity() const;
    QString trimmedRoamIdentity() const;
    void setRoamIdentity(const QString &secret);
    QString lastAttachDir() const;
    void setLastAttachDir(const QString &dir);
};
