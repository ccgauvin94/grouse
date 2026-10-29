#pragma once

#include <QAbstractListModel>
#include <QHash>
#include <QList>
#include <QVariantMap>

class QTimer;

/**
 * The chat transcript as a real QAbstractListModel, updated incrementally with
 * insertRows/dataChanged — never a full reset. The old approach republished a
 * QVariantList on every streamed chunk, which made the ListView treat each
 * update as a brand-new model: full reset, scroll position jumped to the top,
 * and the transcript got slower as it grew. Rows are keyed by role names so a
 * QML delegate reads `model.text`, `model.role`, etc.
 */
class MessageListModel : public QAbstractListModel
{
    Q_OBJECT
    Q_PROPERTY(int count READ count NOTIFY countChanged)
public:
    enum Role {
        IdRole = Qt::UserRole + 1,
        RoleRole,          // "role"
        TextRole,          // "text"
        HtmlRole,          // "html"
        BlocksRole,        // "blocks" — markdownBlocks() output when the text
                           //              has fenced code; the delegate renders
                           //              each chunk separately (html prose /
                           //              code in its own horizontal scroller)
        ThoughtRole,       // "thought"
        TitleRole,         // "title"
        DetailRole,        // "detail"
        OutputRole,        // "output"
        StatusRole,        // "status"
        ToolCallIdRole,    // "toolCallId"
        ImagesRole,        // "images"   — local thumbnail URLs for a sent image attachment
        UsageRole,         // "usage"    — per-message tok/s + cost label (message_usage)
        ChartDataRole,     // "chartData" — Chart.js-style spec JSON for a chart bubble
        AppHtmlRole,       // "appHtml"  — fetched MCP-App template HTML
        AppKeyRole,        // "appKey"   — "$ext|$uri" cache key for an MCP-App bubble
        CallsRole,         // "calls"    — grouped consecutive tool-call maps
        SummaryRole,       // "summary"  — a tool run's one-line label (issue #13)
        StepThoughtRole,   // "stepThought" — the thinking a tool run absorbed; shown
                           //              when the chip opens
        StepsRole,         // "steps"    — a delegate's subagent tool calls [{name,detail}]
        ExpandedRole,      // "expanded" — per-row UI state kept in the model so it
                           //              survives delegate recycling AND the row
                           //              shifting (thinking bubble, tool chip)
    };

    explicit MessageListModel(QObject *parent = nullptr);

    int rowCount(const QModelIndex &parent = QModelIndex()) const override;
    QVariant data(const QModelIndex &index, int role) const override;
    QHash<int, QByteArray> roleNames() const override;

    int count() const { return m_rows.size(); }
    const QList<QVariantMap> &rows() const { return m_rows; }
    QVariantMap row(int index) const;

    void clear();
    void append(const QVariantMap &message);
    /** Insert `messages` (oldest first) at the front, in one begin/endInsertRows. */
    void prepend(const QList<QVariantMap> &messages);

    // -- item-stream API (docs/TRANSCRIPT_MODEL.md) -------------------------
    // The model is the client's item store: rows are keyed by the core's item
    // id, so a re-delivered item updates in place instead of appending.
    /** Index of the row whose `id` is `id`, or -1. */
    int indexForId(const QString &id) const;
    /** Insert or replace by item id (the authoritative `Upsert`). */
    void upsert(const QString &id, const QVariantMap &row);
    void removeById(const QString &id);
    /** Append a live text delta to an item's row (renders plain until finalized). */
    void appendText(const QString &id, const QString &chunk);
    /** Append a live tool-output delta to an item's row. */
    void appendOutput(const QString &id, const QString &chunk);
    /** Replace row `index`'s data and notify QML of every role. */
    void update(int index, const QVariantMap &message);
    /** Flip a row's ExpandedRole (thinking bubble / tool chip open-closed). Takes
     *  a row index but stores by the row's stable uid, so the state follows the
     *  message when a load-older prepend shifts it down. */
    Q_INVOKABLE void toggleExpanded(int row);
    /**
     * Like update(), but the dataChanged notification is deferred and coalesced
     * onto a short timer. Streaming appendChunk accumulates the whole reply into
     * one row, and an immediate dataChanged per token made the delegate relayout
     * the full bubble (HTML re-parse + text shaping) on every chunk — quadratic
     * in the reply length. The row's data is correct right away (caches read
     * rows() directly); only the QML repaint is paced. Call update() when the
     * row is final so the last frame paints immediately.
     */
    void updateDeferred(int index, const QVariantMap &message);

signals:
    void countChanged();

private:
    void commitDeferred();

    void reindex();

    /** Stable identity of a row, or 0 when the index is out of range. */
    quint64 uidAt(int row) const
    {
        return (row >= 0 && row < m_uids.size()) ? m_uids.at(row) : 0;
    }

    QList<QVariantMap> m_rows;
    // Parallel to m_rows. A row's *index* moves whenever rows are inserted or
    // removed around it, so per-row UI state is keyed by this uid instead;
    // starting at 1 keeps 0 free as the "no such row" value.
    QList<quint64> m_uids;
    quint64 m_nextUid = 1;
    QHash<QString, int> m_rowById;   // item id -> row index (non-empty ids only)
    QList<int> m_dirtyRows;
    QHash<quint64, bool> m_expanded;   // row uid -> user-expanded (thinking bubble)
    QTimer *m_deferTimer = nullptr;
};
