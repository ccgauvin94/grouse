#ifndef ROAMLISTMODEL_H
#define ROAMLISTMODEL_H

#include <QAbstractListModel>
#include <QVariantList>

/** Sidebar model for the Roam tab: one header row per active roam endpoint
 *  with its sessions beneath it (drop-down style, mirroring the main
 *  SessionListModel's header/session rows). Sessions carry their peer's label
 *  so opening one can route to the right connection. Lists are small, so
 *  changes reset the model — no incremental diff needed.
 */
class RoamListModel : public QAbstractListModel
{
    Q_OBJECT
public:
    enum Roles {
        RowTypeRole = Qt::UserRole + 1,   // "header" | "session"
        LabelRole,                        // peer label (header + session rows)
        StatusRole,                       // header: connection status text
        ConnectedRole,                    // header: bool
        SessionIdRole,                    // session rows
        TitleRole,
        SnippetRole,
        MessageCountRole,
        CwdRole,
        ExpandedRole,                     // header: drop-down open?
    };

    struct Peer {
        QString label;
        QString status;
        bool connected = false;
        bool expanded = false;
        QVariantList sessions;   // session maps (sessionId/title/snippet/messageCount)
    };

    explicit RoamListModel(QObject *parent = nullptr);

    int rowCount(const QModelIndex &parent = QModelIndex()) const override;
    QVariant data(const QModelIndex &index, int role) const override;
    QHash<int, QByteArray> roleNames() const override;

    // ---- peer lifecycle ----------------------------------------------------
    void addPeer(const QString &label);
    void setPeerStatus(const QString &label, const QString &status, bool connected);
    void setPeerSessions(const QString &label, const QVariantList &sessions);
    void removePeer(const QString &label);
    void clear();
    /** Row index of a peer's header row, or -1. */
    int headerRow(const QString &label) const;
    /** Toggle a peer's drop-down. */
    void togglePeer(const QString &label);

private:
    QList<Peer> m_peers;
};

#endif // ROAMLISTMODEL_H
