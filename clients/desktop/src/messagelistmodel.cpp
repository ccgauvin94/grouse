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
        {SummaryRole, "summary"},
        {StepThoughtRole, "stepThought"},
        {StepsRole, "steps"},
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
    case SummaryRole: return m.value("summary");
    case StepThoughtRole: return m.value("stepThought");
    case StepsRole: return m.value("steps");
    // Keyed by ROW index, not the "id" value: text bubbles carry string message
    // ids ("" for live ones), so an id.toInt() key collapsed every thought
    // bubble onto one shared entry.
    case ExpandedRole: return m_expanded.value(index.row(), false);
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
    m_rowById.clear();
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
    const QString id = message.value(QStringLiteral("id")).toString();
    if (!id.isEmpty())
        m_rowById.insert(id, at);
    endInsertRows();
    emit countChanged();
}

void MessageListModel::prepend(const QList<QVariantMap> &messages)
{
    if (messages.isEmpty())
        return;
    // Present the rows oldest-first at the front: prepending the tail first
    // leaves `messages` in order.
    beginInsertRows(QModelIndex(), 0, messages.size() - 1);
    for (int i = messages.size() - 1; i >= 0; --i)
        m_rows.prepend(messages.at(i));
    endInsertRows();
    reindex();   // every existing row shifted right
    emit countChanged();
}

void MessageListModel::reindex()
{
    m_rowById.clear();
    for (int i = 0; i < m_rows.size(); ++i) {
        const QString id = m_rows.at(i).value(QStringLiteral("id")).toString();
        if (!id.isEmpty())
            m_rowById.insert(id, i);
    }
}

int MessageListModel::indexForId(const QString &id) const
{
    return id.isEmpty() ? -1 : m_rowById.value(id, -1);
}

void MessageListModel::upsert(const QString &id, const QVariantMap &row)
{
    const int existing = indexForId(id);
    if (existing >= 0 && existing < m_rows.size()) {
        updateDeferred(existing, row);
        return;
    }
    // Position comes from the core's `seq`, never from arrival: a prompt echo can
    // land after the reply it prompted, and a page of replayed rows drains in
    // after newer ones. `seq == 0` is a legitimate position (the first row after
    // a repaint), so the rule is purely ordinal — insert before the first row
    // that outranks this one. A row with no `seq` (a local echo or error banner)
    // reads as 0 and therefore stays wherever it already is.
    const int seq = row.value(QStringLiteral("seq"), 0).toInt();
    int at = m_rows.size();
    for (int i = 0; i < m_rows.size(); ++i) {
        if (m_rows.at(i).value(QStringLiteral("seq"), 0).toInt() > seq) {
            at = i;
            break;
        }
    }
    if (at >= m_rows.size()) {
        append(row);
        return;
    }
    beginInsertRows(QModelIndex(), at, at);
    m_rows.insert(at, row);
    endInsertRows();
    reindex();   // every row after the insertion point shifted right
    emit countChanged();
}

void MessageListModel::removeById(const QString &id)
{
    const int i = indexForId(id);
    if (i < 0 || i >= m_rows.size())
        return;
    beginRemoveRows(QModelIndex(), i, i);
    m_rows.removeAt(i);
    endRemoveRows();
    reindex();
    emit countChanged();
}

void MessageListModel::appendText(const QString &id, const QString &chunk)
{
    const int i = indexForId(id);
    if (i < 0 || i >= m_rows.size())
        return;
    QVariantMap row = m_rows.at(i);
    row["text"] = row.value(QStringLiteral("text")).toString() + chunk;
    // Clear the rendered html: the delegate falls back to PlainText while the
    // reply streams, and the core's finalizing Upsert restores the markdown.
    row["html"] = QString();
    updateDeferred(i, row);
}

void MessageListModel::appendOutput(const QString &id, const QString &chunk)
{
    const int i = indexForId(id);
    if (i < 0 || i >= m_rows.size())
        return;
    QVariantMap row = m_rows.at(i);
    row["output"] = row.value(QStringLiteral("output")).toString() + chunk;
    updateDeferred(i, row);
}

void MessageListModel::toggleExpanded(int row)
{
    if (row < 0 || row >= m_rows.size())
        return;
    m_expanded[row] = !m_expanded.value(row, false);
    const QModelIndex idx = index(row);
    emit dataChanged(idx, idx, {ExpandedRole});
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
