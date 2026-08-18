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
        ExpandedRole,      // "expanded" — per-row UI state kept in the model so it
                           //              survives delegate recycling (thinking bubble)
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
    /** Replace row `index`'s data and notify QML of every role. */
    void update(int index, const QVariantMap &message);
    /** Flip a row's ExpandedRole (thinking bubble open/closed). */
    Q_INVOKABLE void toggleExpanded(int id);
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

    QList<QVariantMap> m_rows;
    QList<int> m_dirtyRows;
    QHash<int, bool> m_expanded;   // row id -> user-expanded (thinking bubble)
    QTimer *m_deferTimer = nullptr;
};
