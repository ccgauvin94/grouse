#include "messagelistmodel.h"

#include <QTimer>

MessageListModel::MessageListModel(QObject *parent)
    : QAbstractListModel(parent)
{
    m_deferTimer = new QTimer(this);
    m_deferTimer->setSingleShot(true);
    m_deferTimer->setInterval(120);
    connect(m_deferTimer, &QTimer::timeout, this, &MessageListModel::commitDeferred);
}

int MessageListModel::rowCount(const QModelIndex &parent) const
{
    return parent.isValid() ? 0 : m_rows.size();
}

QHash<int, QByteArray> MessageListModel::roleNames() const
{
    return {
        {IdRole, "id"},
        {RoleRole, "role"},
        {TextRole, "text"},
        {HtmlRole, "html"},
        {ThoughtRole, "thought"},
        {TitleRole, "title"},
        {DetailRole, "detail"},
        {OutputRole, "output"},
        {StatusRole, "status"},
        {ToolCallIdRole, "toolCallId"},
        {ImagesRole, "images"},
        {UsageRole, "usage"},
        {ChartDataRole, "chartData"},
        {AppHtmlRole, "appHtml"},
        {AppKeyRole, "appKey"},
        {CallsRole, "calls"},
        {ExpandedRole, "expanded"},
    };
}

QVariant MessageListModel::data(const QModelIndex &index, int role) const
{
    if (!index.isValid() || index.row() < 0 || index.row() >= m_rows.size())
        return {};
    const QVariantMap m = m_rows.at(index.row());
    switch (role) {
    case IdRole: return m.value("id");
    case RoleRole: return m.value("role");
    case TextRole: return m.value("text");
    case HtmlRole: return m.value("html");
    case ThoughtRole: return m.value("thought");
    case TitleRole: return m.value("title");
    case DetailRole: return m.value("detail");
    case OutputRole: return m.value("output");
    case StatusRole: return m.value("status");
    case ToolCallIdRole: return m.value("toolCallId");
    case ImagesRole: return m.value("images");
    case UsageRole: return m.value("usage");
    case ChartDataRole: return m.value("chartData");
    case AppHtmlRole: return m.value("appHtml");
    case AppKeyRole: return m.value("appKey");
    case CallsRole: return m.value("calls");
    case ExpandedRole: return m_expanded.value(m.value("id").toInt(), false);
    default: return {};
    }
}

QVariantMap MessageListModel::row(int index) const
{
    if (index < 0 || index >= m_rows.size())
        return {};
    return m_rows.at(index);
}

void MessageListModel::clear()
{
    if (m_rows.isEmpty())
        return;
    beginResetModel();
    m_rows.clear();
    m_dirtyRows.clear();
    m_expanded.clear();
    m_deferTimer->stop();
    endResetModel();
    emit countChanged();
}

void MessageListModel::append(const QVariantMap &message)
{
    const int at = m_rows.size();
    beginInsertRows(QModelIndex(), at, at);
    m_rows << message;
    endInsertRows();
    emit countChanged();
}

void MessageListModel::toggleExpanded(int id)
{
    for (int i = 0; i < m_rows.size(); ++i) {
        if (m_rows.at(i).value("id").toInt() == id) {
            m_expanded[id] = !m_expanded.value(id, false);
            const QModelIndex idx = index(i);
            emit dataChanged(idx, idx, {ExpandedRole});
            return;
        }
    }
}

void MessageListModel::update(int index, const QVariantMap &message)
{
    if (index < 0 || index >= m_rows.size())
        return;
    m_rows[index] = message;
    const QModelIndex mi = createIndex(index, 0);
    emit dataChanged(mi, mi);
}

void MessageListModel::updateDeferred(int index, const QVariantMap &message)
{
    if (index < 0 || index >= m_rows.size())
        return;
    m_rows[index] = message;
    if (!m_dirtyRows.contains(index))
        m_dirtyRows << index;
    if (!m_deferTimer->isActive())
        m_deferTimer->start();
}

void MessageListModel::commitDeferred()
{
    if (m_dirtyRows.isEmpty())
        return;
    const QList<int> rows = m_dirtyRows;
    m_dirtyRows.clear();
    for (const int index : rows) {
        // The transcript may have been cleared while a commit was pending.
        if (index < 0 || index >= m_rows.size())
            continue;
        const QModelIndex mi = createIndex(index, 0);
        emit dataChanged(mi, mi);
    }
}
