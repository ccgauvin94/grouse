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
    function categoryLabel(seg) {
        switch (seg.category) {
        case "system_prompt": return qsTr("System prompt")
        case "extension_instructions": return qsTr("Extension instructions")
        case "additional_instructions": return qsTr("Hints and project instructions")
        case "turn_context": return qsTr("Turn context")
        case "tool_definitions": return qsTr("Tool definitions")
        case "compaction_summary": return qsTr("Compaction summary")
        case "messages": return qsTr("Conversation")
        default: return seg.label && seg.label.length > 0 ? seg.label : seg.category
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
                    const total = t.total ? page.fmtK(t.total) : ""
                    const limit = t.limit ? page.fmtK(t.limit) : ""
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

        // Request order, not size order: it is the order the model reads them in, and a bar
        // sorted by size would hide that the system prompt is the thing at the front.
        Repeater {
            model: Mgr.contextReport
            delegate: ColumnLayout {
                id: segRow
                required property var modelData
                Layout.fillWidth: true
                spacing: 2
                readonly property real total: {
                    const t = Mgr.contextReportTotal || {}
                    return t.total > 0 ? t.total : 1
                }

                RowLayout {
                    Layout.fillWidth: true
                    Controls.Label {
                        Layout.fillWidth: true
                        elide: Text.ElideRight
                        text: dialog.categoryLabel(segRow.modelData)
                    }
                    Controls.Label {
                        opacity: 0.7
                        text: page.fmtK(segRow.modelData.tokens || 0)
                    }
                }
                Rectangle {
                    Layout.fillWidth: true
                    Layout.preferredHeight: 3
                    radius: 2
                    color: Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g,
                                   Kirigami.Theme.textColor.b, 0.12)
                    Rectangle {
                        width: parent.width * Math.min(1, (segRow.modelData.tokens || 0) / segRow.total)
                        height: parent.height
                        radius: 2
                        color: Kirigami.Theme.highlightColor
                    }
                }
                // Per-extension / per-kind slicing, indented so the grouping reads without a
                // second list.
                Repeater {
                    model: segRow.modelData.parts ? segRow.modelData.parts : []
                    delegate: RowLayout {
                        required property var modelData
                        Layout.fillWidth: true
                        Layout.leftMargin: Kirigami.Units.gridUnit
                        Controls.Label {
                            Layout.fillWidth: true
                            elide: Text.ElideRight
                            opacity: 0.8
                            text: modelData.label
                        }
                        Controls.Label {
                            opacity: 0.6
                            text: page.fmtK(modelData.tokens || 0)
                        }
                    }
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
