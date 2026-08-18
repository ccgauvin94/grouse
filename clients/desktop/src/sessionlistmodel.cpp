#include "sessionlistmodel.h"

#include <algorithm>

SessionListModel::SessionListModel(QObject *parent)
    : QAbstractListModel(parent)
{
}

int SessionListModel::rowCount(const QModelIndex &parent) const
{
    return parent.isValid() ? 0 : m_rows.size();
}

QHash<int, QByteArray> SessionListModel::roleNames() const
{
    return {
        {TitleRole, "title"},
        {MessageCountRole, "messagecount"},
        {SessionIdRole, "sessionid"},
        {CwdRole, "cwd"},
        {PeerRole, "peer"},
        {SnippetRole, "snippet"},
        {ProjectIdRole, "projectid"},
        {SectionRole, "section"},
        {HeaderRole, "header"},
        {CollapsedRole, "collapsed"},
        {CountRole, "count"},
    };
}

QVariant SessionListModel::data(const QModelIndex &index, int role) const
{
    if (!index.isValid() || index.row() < 0 || index.row() >= m_rows.size())
        return {};
    const QVariantMap m = m_rows.at(index.row());
    switch (role) {
    case TitleRole: return m.value("title");
    case MessageCountRole: return m.value("messageCount");
    case SessionIdRole: return m.value("sessionId");
    case CwdRole: return m.value("cwd");
    case PeerRole: return m.value("peer");
    case SnippetRole: return m.value("snippet");
    case ProjectIdRole: return m.value("projectId");
    case SectionRole: return m.value("section");
    case HeaderRole: return m.value("header").toBool();
    case CollapsedRole: return m.value("collapsed").toBool();
    case CountRole: return m.value("count").toInt();
    default: return {};
    }
}

void SessionListModel::setProjects(const QVariantList &projects)
{
    QHash<QString, QString> names;
    QStringList order;
    for (const auto &v : projects) {
        const QVariantMap p = v.toMap();
        const QString id = p.value("id").toString();
        if (id.isEmpty())
            continue;
        names.insert(id, p.value("name").toString());
        order << id;
    }
    std::stable_sort(order.begin(), order.end(),
                     [&](const QString &a, const QString &b) { return names[a].toLower() < names[b].toLower(); });
    if (names == m_projectNames)
        return;
    m_projectNames = names;
    m_projectOrder = order;
    rebuild();
}

void SessionListModel::rebuild()
{
    beginResetModel();
    m_rows.clear();

    // Stamp each session with its group key: "proj:<id>", "chats" (unfiled),
    // or "peer:<name>".
    for (auto &v : m_sessions) {
        QVariantMap m = v.toMap();
        const QString peer = m.value("peer").toString();
        const QString proj = m.value("projectId").toString();
        if (!peer.isEmpty())
            m["section"] = QStringLiteral("peer:") + peer;
        else if (!proj.isEmpty())
            m["section"] = QStringLiteral("proj:") + proj;
        else
            m["section"] = QStringLiteral("chats");
        v = m;
    }

    auto projectRank = [&](const QString &id) -> int {
        for (int i = 0; i < m_projectOrder.size(); ++i)
            if (m_projectOrder.at(i) == id)
                return i;
        return m_projectOrder.size();   // unknown project sorts after every known one
    };

    // Group sessions by section, preserving their order (already recency-sorted
    // by the server, but sort again to be safe).
    QHash<QString, QList<QVariant>> groups;
    for (const auto &v : std::as_const(m_sessions)) {
        const QString key = v.toMap().value("section").toString();
        if (key.isEmpty())
            continue;
        groups[key].append(v);
    }
    for (auto &list : groups) {
        std::stable_sort(list.begin(), list.end(),
                         [](const QVariant &a, const QVariant &b) {
                             const QVariantMap am = a.toMap();
                             const QVariantMap bm = b.toMap();
                             const QString at = am.value("lastMessageAt").toString().isEmpty()
                                 ? am.value("updatedAt").toString()
                                 : am.value("lastMessageAt").toString();
                             const QString bt = bm.value("lastMessageAt").toString().isEmpty()
                                 ? bm.value("updatedAt").toString()
                                 : bm.value("lastMessageAt").toString();
                             return at > bt;
                         });
    }

    // Section ordering: projects alphabetically (projectRank), then "chats",
    // then remote peers alphabetically.
    auto sectionRank = [&](const QString &key) -> int {
        if (key.startsWith(QLatin1String("proj:")))
            return projectRank(key.mid(5));
        if (key.startsWith(QLatin1String("peer:")))
            return m_projectOrder.size() + 1;
        return m_projectOrder.size();   // "chats"
    };
    QStringList keys = groups.keys();
    std::sort(keys.begin(), keys.end(), [&](const QString &a, const QString &b) {
        const int ra = sectionRank(a), rb = sectionRank(b);
        if (ra != rb)
            return ra < rb;
        return a < b;   // alpha within the same rank (peers)
    });

    for (const auto &key : keys) {
        const QList<QVariant> &sess = groups.value(key);
        const bool collapsed = m_collapsed.contains(key);
        QString title;
        if (key.startsWith(QLatin1String("proj:"))) {
            const QString id = key.mid(5);
            title = m_projectNames.value(id, id);
        } else if (key.startsWith(QLatin1String("peer:"))) {
            title = key.mid(5);
        } else {
            title = QStringLiteral("Chats");
        }
        m_rows << QVariantMap{{"header", true}, {"section", key}, {"title", title},
                              {"count", sess.size()}, {"collapsed", collapsed}};
        if (collapsed)
            continue;
        for (const auto &s : sess)
            m_rows << s.toMap();
    }
    endResetModel();
}

void SessionListModel::setSessions(const QVariantList &list)
{
    // An unchanged list must not reset the model: a QAbstractListModel reset
    // invalidates every row and the sidebar ListView jumps back to the top.
    // Clicking a session re-lists sessions via onReady -> refreshSessions, so a
    // no-op here is what keeps the scroll position stable across that click.
    bool same = list.size() == m_sessions.size();
    if (same) {
        for (int i = 0; i < list.size(); ++i) {
            const QVariantMap a = list.at(i).toMap();
            const QVariantMap b = m_sessions.at(i).toMap();
            if (a.value("sessionId") != b.value("sessionId")
                || a.value("updatedAt") != b.value("updatedAt")
                || a.value("lastMessageAt") != b.value("lastMessageAt")
                || a.value("title") != b.value("title")
                || a.value("snippet") != b.value("snippet")
                || a.value("messageCount") != b.value("messageCount")
                || a.value("peer") != b.value("peer")
                || a.value("projectId") != b.value("projectId")) {
                same = false;
                break;
            }
        }
    }
    if (same)
        return;
    m_sessions = list;
    rebuild();
}

void SessionListModel::toggleSection(const QString &key)
{
    if (key.isEmpty())
        return;
    if (m_collapsed.contains(key))
        m_collapsed.remove(key);
    else
        m_collapsed.insert(key);
    rebuild();
}

QString SessionListModel::cwdFor(const QString &sessionId) const
{
    for (const auto &v : m_sessions) {
        if (v.toMap().value("sessionId").toString() == sessionId)
            return v.toMap().value("cwd").toString();
    }
    return {};
}
