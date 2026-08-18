#include "roamlistmodel.h"

RoamListModel::RoamListModel(QObject *parent)
    : QAbstractListModel(parent)
{
}

int RoamListModel::rowCount(const QModelIndex &parent) const
{
    if (parent.isValid())
        return 0;
    int rows = 0;
    for (const Peer &p : m_peers)
        rows += 1 + (p.expanded ? p.sessions.size() : 0);
    return rows;
}

QHash<int, QByteArray> RoamListModel::roleNames() const
{
    return {
        {RowTypeRole, "rowType"},
        {LabelRole, "label"},
        {StatusRole, "status"},
        {ConnectedRole, "connected"},
        {SessionIdRole, "sessionId"},
        {TitleRole, "title"},
        {SnippetRole, "snippet"},
        {MessageCountRole, "messageCount"},
        {CwdRole, "cwd"},
        {ExpandedRole, "expanded"},
    };
}

QVariant RoamListModel::data(const QModelIndex &index, int role) const
{
    if (!index.isValid())
        return QVariant();
    int row = index.row();
    for (const Peer &p : m_peers) {
        if (row == 0) {
            // header row
            switch (role) {
            case RowTypeRole: return QStringLiteral("header");
            case LabelRole: return p.label;
            case StatusRole: return p.status;
            case ConnectedRole: return p.connected;
            case ExpandedRole: return p.expanded;
            default: return QVariant();
            }
        }
        --row;
        if (p.expanded && row < p.sessions.size()) {
            const QVariantMap s = p.sessions.at(row).toMap();
            switch (role) {
            case RowTypeRole: return QStringLiteral("session");
            case LabelRole: return p.label;
            case SessionIdRole: return s.value(QStringLiteral("sessionId"));
            case TitleRole: return s.value(QStringLiteral("title"));
            case SnippetRole: return s.value(QStringLiteral("snippet"));
            case MessageCountRole: return s.value(QStringLiteral("messageCount"));
            case CwdRole: return s.value(QStringLiteral("cwd"));
            default: return QVariant();
            }
        }
        row -= p.expanded ? p.sessions.size() : 0;
    }
    return QVariant();
}

void RoamListModel::addPeer(const QString &label)
{
    beginResetModel();
    for (const Peer &p : m_peers) {
        if (p.label == label) {
            endResetModel();
            return;   // already present
        }
    }
    Peer p;
    p.label = label;
    p.status = QStringLiteral("connecting…");
    m_peers.append(p);
    endResetModel();
}

void RoamListModel::setPeerStatus(const QString &label, const QString &status, bool connected)
{
    for (Peer &p : m_peers) {
        if (p.label == label) {
            p.status = status;
            p.connected = connected;
            beginResetModel();
            endResetModel();
            return;
        }
    }
}

void RoamListModel::setPeerSessions(const QString &label, const QVariantList &sessions)
{
    for (Peer &p : m_peers) {
        if (p.label == label) {
            p.sessions = sessions;
            beginResetModel();
            endResetModel();
            return;
        }
    }
}

void RoamListModel::removePeer(const QString &label)
{
    for (int i = 0; i < m_peers.size(); ++i) {
        if (m_peers.at(i).label == label) {
            beginResetModel();
            m_peers.removeAt(i);
            endResetModel();
            return;
        }
    }
}

void RoamListModel::clear()
{
    if (m_peers.isEmpty())
        return;
    beginResetModel();
    m_peers.clear();
    endResetModel();
}

int RoamListModel::headerRow(const QString &label) const
{
    int row = 0;
    for (const Peer &p : m_peers) {
        if (p.label == label)
            return row;
        row += 1 + (p.expanded ? p.sessions.size() : 0);
    }
    return -1;
}

void RoamListModel::togglePeer(const QString &label)
{
    for (Peer &p : m_peers) {
        if (p.label == label) {
            p.expanded = !p.expanded;
            beginResetModel();
            endResetModel();
            return;
        }
    }
}
