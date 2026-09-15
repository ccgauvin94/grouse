import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// The server's goose memory store: one flat .txt per topic under
// $XDG_CONFIG_HOME/goose/memory (the builtin Memory extension is global and
// project-blind; "per-project memory" is just a topic named after the
// project, which the seeded project instructions teach the model to use).
// Reached from Settings (all topics) and from a project's dialog (prefixed to
// that project's topic). Reads/writes ride the shell tool via Mgr, so a live
// session is required.
Controls.Dialog {
    id: dialog
    title: qsTr("Memory")
    modal: true
    standardButtons: Controls.Dialog.Close
    closePolicy: Controls.Popup.CloseOnEscape
    width: Math.min(760, parent ? parent.width - 32 : 760)
    height: Math.min(600, parent ? parent.height - 32 : 600)

    property var topics: []
    property string selected: ""
    property string origText: ""
    property string note: ""
    // When set (from the project dialog), the detail pane opens on this topic
    // and creation is allowed even if the file does not exist yet.
    property string pendingTopic: ""

    // Test-visible handles (ids are file-private; the QML test reaches the
    // widgets through aliases — the RecipesDialog pattern).
    readonly property alias detailList: topicList
    readonly property alias detailEditor: memoryArea

    function openGlobal() {
        dialog.pendingTopic = ""
        dialog.open()
        dialog.refresh()
    }
    function openForTopic(topic) {
        dialog.pendingTopic = topic
        dialog.title = qsTr("Memory — %1").arg(topic)
        dialog.open()
        dialog.refresh()
    }
    function refresh() {
        if (!Mgr.memoryReady()) {
            dialog.note = qsTr("Connect and open a chat first — memory rides the server's shell tool.")
            return
        }
        dialog.note = ""
        Mgr.memoryList()
    }
    function select(topic) {
        dialog.selected = topic
        Mgr.memoryRead(topic)
    }
    // A topic's memory file name is `<topic>.txt`; the editor shows the body.
    function topicOf(row) { return row.name }

    Connections {
        target: Mgr
        function onMemoryTopics(rows, ok, note) {
            if (!ok) { dialog.note = note; return }
            dialog.topics = rows
            if (dialog.pendingTopic.length > 0) {
                dialog.select(dialog.pendingTopic)
                dialog.pendingTopic = ""
            } else if (dialog.selected.length === 0 && rows.length > 0) {
                dialog.select(rows[0].name)
            }
        }
        function onMemoryContentLoaded(topic, text, ok) {
            if (topic !== dialog.selected) return
            dialog.origText = ok ? text : ""
            memoryArea.text = dialog.origText
            if (!ok) dialog.note = qsTr("Could not read this memory.")
        }
        function onMemorySaved(topic, ok, note) {
            dialog.note = note
            if (ok && topic === dialog.selected) dialog.origText = memoryArea.text
            if (ok) Mgr.memoryList()
        }
    }

    contentItem: RowLayout {
        spacing: Kirigami.Units.smallSpacing
        implicitWidth: 740
        implicitHeight: 580

        // --- master: topic list ----------------------------------------------
        ColumnLayout {
            Layout.preferredWidth: 240
            Layout.fillHeight: true
            spacing: 2

            RowLayout {
                Layout.fillWidth: true
                Controls.Label {
                    text: qsTr("Topics")
                    font.weight: Font.DemiBold
                    Layout.fillWidth: true
                }
                Controls.Label {
                    text: dialog.topics.length
                    color: Kirigami.Theme.disabledTextColor
                }
                Controls.ToolButton {
                    icon.name: "view-refresh"
                    display: Controls.AbstractButton.IconOnly
                    Controls.ToolTip.visible: hovered
                    Controls.ToolTip.text: qsTr("Reload")
                    onClicked: dialog.refresh()
                }
            }

            ListView {
                id: topicList
                Layout.fillWidth: true
                Layout.fillHeight: true
                clip: true
                spacing: 2
                model: dialog.topics
                highlightFollowsCurrentItem: false
                highlight: Rectangle {
                    radius: Kirigami.Units.smallSpacing
                    color: Kirigami.Theme.highlightColor
                    opacity: 0.22
                }
                Controls.ScrollBar.vertical: Controls.ScrollBar { policy: Controls.ScrollBar.AsNeeded }

                delegate: Controls.ItemDelegate {
                    width: ListView.view.width
                    height: Math.max(40, tdelCol.implicitHeight + Kirigami.Units.smallSpacing * 2)
                    onClicked: {
                        topicList.currentIndex = index
                        dialog.select(modelData.name)
                    }
                    contentItem: Column {
                        id: tdelCol
                        spacing: 1
                        Controls.Label {
                            text: modelData.name
                            width: parent.width
                            elide: Text.ElideRight
                            font.bold: true
                        }
                        Controls.Label {
                            text: modelData.summary
                            width: parent.width
                            elide: Text.ElideRight
                            color: Kirigami.Theme.disabledTextColor
                            font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
                            visible: text.length > 0
                        }
                    }
                }

                footer: Controls.Label {
                    visible: dialog.topics.length === 0 && dialog.note.length === 0
                    text: qsTr("No memories yet.")
                    color: Kirigami.Theme.disabledTextColor
                    padding: Kirigami.Units.largeSpacing
                }
            }

            RowLayout {
                Layout.fillWidth: true
                Controls.TextField {
                    id: newTopicField
                    Layout.fillWidth: true
                    placeholderText: qsTr("new topic name")
                }
                Controls.Button {
                    text: qsTr("Add")
                    enabled: newTopicField.text.trim().length > 0 && Mgr.memoryReady()
                    onClicked: {
                        const t = newTopicField.text.trim()
                        newTopicField.text = ""
                        Mgr.memoryWrite(t, "# " + t + "\n")
                        dialog.selected = t
                    }
                }
            }
        }

        Kirigami.Separator {
            Layout.fillHeight: true
            Layout.preferredWidth: 1
            color: Kirigami.Theme.separatorColor ? Kirigami.Theme.separatorColor
                                                 : Kirigami.Theme.disabledTextColor
        }

        // --- detail: one topic's file ----------------------------------------
        ColumnLayout {
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: Kirigami.Units.smallSpacing

            Controls.Label {
                text: dialog.note
                visible: dialog.note.length > 0
                color: Kirigami.Theme.disabledTextColor
                wrapMode: Text.Wrap
                Layout.fillWidth: true
            }
            Controls.Label {
                text: dialog.selected.length > 0
                      ? qsTr("%1.txt — the first line is the keyword list").arg(dialog.selected)
                      : qsTr("Select a topic.")
                color: Kirigami.Theme.disabledTextColor
                font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
                wrapMode: Text.Wrap
                Layout.fillWidth: true
            }
            Controls.ScrollView {
                Layout.fillWidth: true
                Layout.fillHeight: true
                Controls.TextArea {
                    id: memoryArea
                    enabled: dialog.selected.length > 0
                    wrapMode: Text.Wrap
                    placeholderText: qsTr("(empty memory file)")
                }
            }
            RowLayout {
                Layout.fillWidth: true
                Item { Layout.fillWidth: true }
                Controls.Button {
                    text: qsTr("Save")
                    icon.name: "document-save"
                    enabled: dialog.selected.length > 0 && memoryArea.text !== dialog.origText
                    onClicked: {
                        Mgr.memoryWrite(dialog.selected, memoryArea.text)
                        dialog.origText = memoryArea.text
                    }
                }
            }
        }
    }
}
