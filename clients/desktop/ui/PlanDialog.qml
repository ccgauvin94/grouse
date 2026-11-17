import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import Grouse 1.0
import org.kde.kirigami as Kirigami

// The plan popup: what to do with the plan a plan-mode turn just produced.
//
// A dialog rather than a card in the transcript because the four answers are a decision,
// not conversation — the same reason the permission prompt is one. The plan text is
// selectable: the most common "edit" is copying a line out of it.
//
// Cancel leaves plan mode ON. Dismissing a plan is not approving it, and the agent is
// still instructed to plan — the one thing that must not happen is a popup that quietly
// turns the mode off because the user closed a window.
Controls.Dialog {
    id: dialog

    property string planText: ""

    title: qsTr("Plan ready")
    modal: true
    closePolicy: Controls.Popup.CloseOnEscape
    width: Math.min(Kirigami.Units.gridUnit * 40, parent ? parent.width * 0.9 : 900)
    height: Math.min(Kirigami.Units.gridUnit * 34, parent ? parent.height * 0.9 : 700)

    function openWith(text) {
        dialog.planText = text
        dialog.open()
    }

    contentItem: ColumnLayout {
        spacing: Kirigami.Units.smallSpacing

        Controls.Label {
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            text: qsTr("The agent has a plan. Approving leaves plan mode and starts the work; editing keeps planning.")
            color: Kirigami.Theme.disabledTextColor
        }

        Controls.ScrollView {
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            contentWidth: availableWidth

            Controls.TextArea {
                text: dialog.planText
                readOnly: true
                selectByMouse: true
                wrapMode: TextEdit.Wrap
                textFormat: TextEdit.MarkdownText
                background: Rectangle {
                    color: Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g,
                                   Kirigami.Theme.textColor.b, 0.06)
                    radius: Theme.radius.sm
                }
            }
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing
            Item { Layout.fillWidth: true }

            Controls.Button {
                text: qsTr("Cancel")
                onClicked: dialog.close()   // plan mode stays on
            }
            Controls.Button {
                text: qsTr("Edit")
                icon.name: "document-edit"
                // Back to the conversation with plan mode still on: the next message is
                // another planning turn, so "edit" needs no other state change.
                onClicked: {
                    dialog.close()
                    dialog.editRequested()
                }
            }
            Controls.Button {
                text: qsTr("Approve & compact")
                icon.name: "archive-insert"
                // Folds the exploration away before executing: a plan turn spends a lot of
                // window on reading, and the implementing turn does not need it.
                onClicked: {
                    dialog.close()
                    Mgr.approvePlan(true)
                }
            }
            Controls.Button {
                text: qsTr("Approve")
                icon.name: "dialog-ok-apply"
                highlighted: true
                onClicked: {
                    dialog.close()
                    Mgr.approvePlan(false)
                }
            }
        }
    }

    signal editRequested()
}
