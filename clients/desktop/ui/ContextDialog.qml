import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// What fills the context window if the chat were prompted right now — the SERVER's own
// attribution (`_goose/unstable/context/report`), which is a different measurement from the
// footer's donut: goose tokenizes the request it WOULD send next, the footer shows the
// provider's billed count for the request it already sent. They are ~7k apart on a 165k
// conversation, so the header says "estimated" rather than letting the two look like a bug.
//
// Modeless on purpose: it is a reading, not a decision, so it can stay open while you keep
// typing. Opening it asks the core; a server older than goose 1.58 never answers and the
// dialog just says so.
Controls.Dialog {
    id: dialog
    title: qsTr("What fills the context window")
    modal: false
    standardButtons: Controls.Dialog.Close
    closePolicy: Controls.Popup.CloseOnEscape
    width: Math.min(Kirigami.Units.gridUnit * 34, parent ? parent.width * 0.92 : 900)

    onOpened: Mgr.requestContextReport()

    // goose's categories are wire values; a person reads English. An unknown category falls
    // back to its own label so a future goose category renders instead of vanishing.
    // goose's `category` is the CLASS and `label` is the THING (a tool, a hint file, an
    // extension). A flat list of segments therefore prints the class over and over; group by
    // category, keep request order of first appearance (NOT size order: the system prompt is
    // first because it IS first), and nest the labels under it.
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
    // Which categories are open; empty is the compact view, which is the point.
    property var open: ({})
    function toggle(cat) {
        var next = Object.assign({}, open)
        if (next[cat]) delete next[cat]
        else next[cat] = true
        open = next
    }

    // Same convention as the page's footer numbers (40.3k, 1.0M) so the dialog and the donut
    // read alike. Local to the dialog: `page` is ChatPage's id and is NOT in scope here.
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

    function categoryLabel(category) {
        switch (category) {
        case "system_prompt": return qsTr("System prompt")
        case "extension_instructions": return qsTr("Extension instructions")
        case "additional_instructions": return qsTr("Hints and project instructions")
        case "turn_context": return qsTr("Turn context")
        case "tool_definitions": return qsTr("Tool definitions")
        case "compaction_summary": return qsTr("Compaction summary")
        case "messages": return qsTr("Conversation")
        default: return seg
        }
    }

    contentItem: ColumnLayout {
        spacing: Kirigami.Units.smallSpacing

        Controls.Label {
            Layout.fillWidth: true
            visible: Mgr.contextReport.length === 0
            wrapMode: Text.WordWrap
            opacity: 0.7
            text: qsTr("Asking the server… (a server older than goose 1.58 cannot answer this)")
        }

        RowLayout {
            Layout.fillWidth: true
            visible: Mgr.contextReport.length > 0
            Controls.Label {
                Layout.fillWidth: true
                font.bold: true
                text: qsTr("Estimated for the next request")
            }
            Controls.Label {
                opacity: 0.7
                text: {
                    const t = Mgr.contextReportTotal || {}
                    const total = t.total ? dialog.fmtK(t.total) : ""
                    const limit = t.limit ? dialog.fmtK(t.limit) : ""
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
            text: qsTr("Over the compaction threshold — the next turn compacts first")
        }

        // One row per category, its segments nested and collapsed until tapped.
        Repeater {
            model: dialog.groups()
            delegate: ColumnLayout {
                id: groupRow
                required property var modelData
                Layout.fillWidth: true
                spacing: 2
                readonly property real total: {
                    const t = Mgr.contextReportTotal || {}
                    return t.total > 0 ? t.total : 1
                }
                readonly property bool isOpen: !!dialog.open[groupRow.modelData.category]

                RowLayout {
                    Layout.fillWidth: true
                    Controls.Label {
                        text: groupRow.modelData.children.length > 1
                              ? (groupRow.isOpen ? "\u25be" : "\u25b8") : " "
                        opacity: 0.6
                    }
                    Controls.Label {
                        Layout.fillWidth: true
                        elide: Text.ElideRight
                        text: dialog.categoryLabel(groupRow.modelData.category)
                    }
                    Controls.Label {
                        opacity: 0.7
                        text: dialog.fmtK(groupRow.modelData.tokens || 0)
                    }
                }
                Rectangle {
                    Layout.fillWidth: true
                    Layout.preferredHeight: 3
                    radius: 2
                    color: Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g,
                                   Kirigami.Theme.textColor.b, 0.12)
                    Rectangle {
                        width: parent.width * Math.min(1, (groupRow.modelData.tokens || 0) / groupRow.total)
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
                        Controls.Label {
                            Layout.fillWidth: true
                            elide: Text.ElideRight
                            opacity: 0.8
                            text: modelData.label && modelData.label.length > 0
                                  ? modelData.label : modelData.category
                        }
                        Controls.Label {
                            opacity: 0.6
                            text: dialog.fmtK(modelData.tokens || 0)
                        }
                    }
                }
                TapHandler {
                    onTapped: dialog.toggle(groupRow.modelData.category)
                }
            }
        }

        Controls.Label {
            Layout.fillWidth: true
            visible: Mgr.contextReport.length > 0
            wrapMode: Text.WordWrap
            opacity: 0.6
            font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
            text: qsTr("Counted with goose's local tokenizer, so it will not match the footer's billed total.")
        }
    }
}
