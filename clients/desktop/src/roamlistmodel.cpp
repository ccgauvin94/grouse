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
            // The delegate is ONE Item hosting both branches; the hidden branch still
            // evaluates its bindings, so a header row reads the session column's roles.
            // Returning QVariant() there made QML warn "Unable to assign [undefined]"
            // for every header and buried real binding failures in that noise.
            case TitleRole: return p.label;
            case SnippetRole: return QString();
            case MessageCountRole: return p.sessions.size();
            case CwdRole: return QString();
            case SessionIdRole: return QString();   // a header is a peer, not a chat
            default: return QVariant();
            }
        }
        --row;
        if (p.expanded && row < p.sessions.size()) {
            const QVariantMap s = p.sessions.at(row).toMap();
            switch (role) {
            case RowTypeRole: return QStringLiteral("session");
            case LabelRole: return p.label;
            // Coerced, not passed through: a peer's session map may simply not carry a
            // key (an older core, a chat with no snippet yet), and raw s.value() then
            // hands QML an invalid QVariant — the same "undefined" leak as the roles
            // above, just from the data side.
            case SessionIdRole: return s.value(QStringLiteral("sessionId")).toString();
            case TitleRole: return s.value(QStringLiteral("title")).toString();
            case SnippetRole: return s.value(QStringLiteral("snippet")).toString();
            case MessageCountRole: return s.value(QStringLiteral("messageCount")).toInt();
            case CwdRole: return s.value(QStringLiteral("cwd")).toString();
            // Same reason as the header branch: a session row still evaluates the
            // hidden header's `status` bindings, so these must be defined too.
            case StatusRole: return p.status;
            case ConnectedRole: return p.connected;
            case ExpandedRole: return false;   // only a peer header expands
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

bool RoamListModel::peerConnected(const QString &label) const
{
    for (const Peer &p : m_peers) {
        if (p.label == label)
            return p.connected;
    }
    return false;
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
