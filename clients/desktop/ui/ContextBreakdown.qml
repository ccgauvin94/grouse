import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// What fills the context window if this chat were prompted right now — the SERVER's own
// attribution (`_goose/unstable/context/report`). This is a different measurement from the
// donut: goose tokenizes the request it WOULD send next, the donut shows the provider's
// billed count for the request it already sent. They sit ~7k apart on a 165k conversation,
// so the header says "estimated" rather than letting the two look like a bug.
//
// A ColumnLayout that sizes to its content: the side panel owns the scrolling, so this has
// no viewport of its own.
ColumnLayout {
    id: breakdown
    spacing: Kirigami.Units.smallSpacing

    /// Which category groups are open. Empty is the compact view, which is the point — a
    /// busy chat has ~40 segments and none of them belong on screen at once.
    property var open: ({})

    function toggle(category) {
        var next = Object.assign({}, breakdown.open)
        if (next[category]) delete next[category]
        else next[category] = true
        breakdown.open = next
    }

    // goose's `category` is the CLASS and `label` is the THING (a tool, a hint file, an
    // extension), so a flat list prints the class over and over. Group by category, keep
    // request order of first appearance (NOT size order: the system prompt is first
    // because it IS first), and nest the labels under it.
    function groups() {
        const rows = Mgr.contextReport || []
        const order = []
        for (let i = 0; i < rows.length; i++)
            if (order.indexOf(rows[i].category) < 0) order.push(rows[i].category)
        return order.map(function (cat) {
            const kids = rows.filter(function (r) { return r.category === cat })
            let tokens = 0
            for (let i = 0; i < kids.length; i++) tokens += kids[i].tokens || 0
            return { category: cat, tokens: tokens, children: kids }
        })
    }

    // Same convention as the composer's numbers (40.3k, 1.0M) so the panel and the donut
    // read alike. Public because the panel's Context header prints the same totals.
    function fmtK(n) {
        if (n >= 1000000) {
            const m = n / 1000000
            return (m >= 10 ? String(Math.round(m)) : String(Math.round(m * 10) / 10)) + "M"
        }
        if (n >= 1000) {
            const v = n / 1000
            return (v >= 100 ? String(Math.round(v)) : String(Math.round(v * 10) / 10)) + "k"
        }
        return String(n)
    }

    // An unknown category falls back to its own wire value so a future goose category
    // renders instead of vanishing.
    function categoryLabel(category) {
        switch (category) {
        case "system_prompt": return qsTr("System prompt")
        case "extension_instructions": return qsTr("Extension instructions")
        case "additional_instructions": return qsTr("Hints and project instructions")
        case "turn_context": return qsTr("Turn context")
        case "tool_definitions": return qsTr("Tool definitions")
        case "deferred_tools": return qsTr("Deferred (not loaded)")
        case "compaction_summary": return qsTr("Compaction summary")
        case "messages": return qsTr("Conversation")
        default: return category
        }
    }

    // The bar widths share one denominator, so rows stay comparable with each other.
    readonly property real total: {
        const t = Mgr.contextReportTotal || {}
        return t.total > 0 ? t.total : 1
    }

    Controls.Label {
        Layout.fillWidth: true
        visible: Mgr.contextReport.length === 0
        wrapMode: Text.WordWrap
        opacity: 0.7
        font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
        text: qsTr("Nothing measured yet — asking the server. (A server older than goose 1.58 cannot answer this.)")
    }

    RowLayout {
        Layout.fillWidth: true
        visible: Mgr.contextReport.length > 0
        spacing: Kirigami.Units.smallSpacing
        Controls.Label {
            Layout.fillWidth: true
            font.bold: true
            elide: Text.ElideRight
            text: qsTr("Estimated for the next request")
        }
        Controls.Label {
            opacity: 0.7
            text: {
                const t = Mgr.contextReportTotal || {}
                const total = t.total ? breakdown.fmtK(t.total) : ""
                const limit = t.limit ? breakdown.fmtK(t.limit) : ""
                return limit.length > 0 ? total + " / " + limit : total
            }
        }
    }

    Controls.Label {
        Layout.fillWidth: true
        visible: {
            const t = Mgr.contextReportTotal || {}
            return Mgr.contextReport.length > 0 && t.pending === true
        }
        wrapMode: Text.WordWrap
        color: Kirigami.Theme.negativeTextColor
        font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
        text: qsTr("Over the compaction threshold — the next turn compacts first")
    }

    // One row per category, its segments nested and collapsed until tapped.
    Repeater {
        model: breakdown.groups()
        delegate: ColumnLayout {
            id: groupRow
            required property var modelData
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing / 2
            readonly property bool isOpen: !!breakdown.open[groupRow.modelData.category]

            RowLayout {
                Layout.fillWidth: true
                spacing: Kirigami.Units.smallSpacing
                Controls.Label {
                    // The chevron is only a hint; the whole row is the hit target.
                    text: groupRow.modelData.children.length > 1
                          ? (groupRow.isOpen ? "\u25be" : "\u25b8") : " "
                    opacity: 0.6
                }
                Controls.Label {
                    Layout.fillWidth: true
                    elide: Text.ElideRight
                    text: breakdown.categoryLabel(groupRow.modelData.category)
                }
                Controls.Label {
                    opacity: 0.7
                    text: breakdown.fmtK(groupRow.modelData.tokens || 0)
                }
            }
            Rectangle {
                Layout.fillWidth: true
                Layout.preferredHeight: 3
                radius: 2
                color: Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g,
                               Kirigami.Theme.textColor.b, 0.12)
                Rectangle {
                    width: parent.width * Math.min(1, (groupRow.modelData.tokens || 0) / breakdown.total)
                    height: parent.height
                    radius: 2
                    color: Kirigami.Theme.highlightColor
                }
            }
            Repeater {
                model: groupRow.isOpen ? groupRow.modelData.children : []
                delegate: RowLayout {
                    required property var modelData
                    Layout.fillWidth: true
                    Layout.leftMargin: Kirigami.Units.gridUnit
                    spacing: Kirigami.Units.smallSpacing
                    Controls.Label {
                        Layout.fillWidth: true
                        elide: Text.ElideRight
                        opacity: 0.8
                        font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.9
                        text: modelData.label && modelData.label.length > 0
                              ? modelData.label : breakdown.categoryLabel(modelData.category)
                    }
                    Controls.Label {
                        opacity: 0.6
                        font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.9
                        text: breakdown.fmtK(modelData.tokens || 0)
                    }
                }
            }
            TapHandler {
                onTapped: breakdown.toggle(groupRow.modelData.category)
            }
        }
    }

    Controls.Label {
        Layout.fillWidth: true
        visible: Mgr.contextReport.length > 0
        wrapMode: Text.WordWrap
        opacity: 0.6
        font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.8
        text: qsTr("Counted with goose's local tokenizer, so it will not match the donut's billed total.")
    }
}
