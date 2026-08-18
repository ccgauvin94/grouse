#pragma once

#include <QAbstractListModel>
#include <QHash>
#include <QSet>
#include <QVariantList>

/**
 * The sidebar's session list as a proper QAbstractListModel with explicit role
 * names. Feeding a raw QVariantList straight to a ListView leaves role names at
 * the mercy of QML's case handling (lowercasing), which is how `messageCount`
 * silently read as undefined. Explicit roles remove that ambiguity.
 *
 * The model emits a FLAT display list: one header row per group (project /
 * unfiled "Chats" / remote peer), each followed by its sessions unless that
 * group is collapsed. Headers carry `header=true` plus `title`/`count`/
 * `collapsed`; session rows carry the session fields.
 */
class SessionListModel : public QAbstractListModel
{
    Q_OBJECT
public:
    enum Role {
        TitleRole = Qt::UserRole + 1,
        MessageCountRole,
        SessionIdRole,
        CwdRole,
        PeerRole,
        SnippetRole,
        ProjectIdRole,
        SectionRole,
        HeaderRole,       // true for group header rows
        CollapsedRole,    // header rows: is this group collapsed?
        CountRole,        // header rows: number of sessions in the group
    };

    explicit SessionListModel(QObject *parent = nullptr);

    int rowCount(const QModelIndex &parent = QModelIndex()) const override;
    QVariant data(const QModelIndex &index, int role) const override;
    QHash<int, QByteArray> roleNames() const override;

    /** Replace the whole list (thin client: the server is the source of truth). */
    void setSessions(const QVariantList &list);

    /** Project list used to order/name the sidebar's project groups. */
    void setProjects(const QVariantList &projects);

    /** cwd of a session id, for session/load (never guess it). Fallback via search. */
    Q_INVOKABLE QString cwdFor(const QString &sessionId) const;

    /** Collapse/expand the group with the given section key ("proj:x", "chats", "peer:y"). */
    Q_INVOKABLE void toggleSection(const QString &key);

private:
    void rebuild();

    QVariantList m_sessions;
    // projectId -> display name; order of projects by name for stable grouping.
    QHash<QString, QString> m_projectNames;
    QStringList m_projectOrder;
    // Section keys the user collapsed; their sessions are hidden until expanded.
    QSet<QString> m_collapsed;
    // Flat display rows (group headers interleaved with session rows).
    QList<QVariantMap> m_rows;
};
