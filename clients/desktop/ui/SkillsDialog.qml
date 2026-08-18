import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// Browse and edit the server's skills (each carries its whole SKILL.md in
// `content`). Skills goose bundles are read-only (`writable` false); Save and
// Delete are disabled for them and the editor is locked.
Controls.Dialog {
    id: dialog
    title: qsTr("Skills")
    modal: true
    standardButtons: Controls.Dialog.Close
    closePolicy: Controls.Popup.CloseOnEscape
    // Explicit size (like Recipes/Scheduler): sizing from contentItem implicit
    // sizes alone collapses this dialog to near-nothing on QQC2.
    width: Math.min(720, parent ? parent.width - 32 : 720)
    height: Math.min(600, parent ? parent.height - 32 : 600)

    property var selectedSkill: null

    onOpened: Mgr.refreshSkills()
    // Re-assign on every selection: once the user edits the TextArea the `text:`
    // binding is gone (a QQC2 control breaks it on input), so this must push the
    // newly selected skill's content explicitly.
    onSelectedSkillChanged: {
        skillTextArea.text = dialog.selectedSkill ? dialog.selectedSkill.content : ""
    }

    contentItem: RowLayout {
        spacing: Kirigami.Units.smallSpacing
        implicitWidth: 700
        implicitHeight: 600

        ListView {
            id: skillsList
            Layout.preferredWidth: 220
            Layout.fillHeight: true
            clip: true
            spacing: 2
            model: Mgr.skills
            Controls.ScrollBar.vertical: Controls.ScrollBar { policy: Controls.ScrollBar.AsNeeded }

            delegate: Controls.ItemDelegate {
                id: sdel
                width: ListView.view.width
                height: Math.max(44, sdelCol.implicitHeight + Kirigami.Units.smallSpacing * 2)
                onClicked: {
                    skillsList.currentIndex = index
                    dialog.selectedSkill = modelData
                }
                contentItem: Column {
                    id: sdelCol
                    spacing: 1
                    Controls.Label {
                        text: modelData.name
                        width: parent.width
                        elide: Text.ElideRight
                        font.bold: true
                    }
                    Controls.Label {
                        text: modelData.description
                        width: parent.width
                        elide: Text.ElideRight
                        opacity: 0.7
                        font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
                        visible: text.length > 0
                    }
                }
            }
        }

        ColumnLayout {
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: Kirigami.Units.smallSpacing

            ColumnLayout {
                visible: !!dialog.selectedSkill
                Layout.fillWidth: true
                Layout.fillHeight: true
                spacing: Kirigami.Units.smallSpacing

                RowLayout {
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.smallSpacing
                    Controls.Label {
                        text: dialog.selectedSkill ? dialog.selectedSkill.name : ""
                        font.bold: true
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }
                    Controls.Label {
                        text: dialog.selectedSkill
                              ? (dialog.selectedSkill.writable
                                 ? qsTr("Writable")
                                 : qsTr("Read-only (bundled)"))
                              : ""
                        opacity: 0.7
                    }
                }

                Controls.TextArea {
                    id: skillTextArea
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    wrapMode: Text.WrapAnywhere
                    readOnly: dialog.selectedSkill ? !dialog.selectedSkill.writable : true
                }

                RowLayout {
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.smallSpacing
                    Controls.Button {
                        text: qsTr("Save")
                        icon.name: "document-save"
                        enabled: dialog.selectedSkill && dialog.selectedSkill.writable
                        onClicked: {
                            Mgr.saveSkill(dialog.selectedSkill.path,
                                          dialog.selectedSkill.name,
                                          dialog.selectedSkill.description,
                                          skillTextArea.text)
                        }
                    }
                    Controls.Button {
                        text: qsTr("Delete")
                        icon.name: "edit-delete"
                        enabled: dialog.selectedSkill && dialog.selectedSkill.writable
                        onClicked: {
                            Mgr.deleteSkill(dialog.selectedSkill.path)
                            dialog.selectedSkill = null
                            skillsList.currentIndex = -1
                        }
                    }
                    Controls.Label {
                        text: dialog.selectedSkill
                              ? (dialog.selectedSkill.global ? qsTr("Global") : qsTr("Project"))
                              : ""
                        opacity: 0.6
                        Layout.fillWidth: true
                        horizontalAlignment: Text.AlignRight
                    }
                }
            }

            Controls.Label {
                visible: !dialog.selectedSkill
                text: qsTr("Select a skill to view it.")
                opacity: 0.7
                horizontalAlignment: Text.AlignHCenter
                verticalAlignment: Text.AlignVCenter
                Layout.fillWidth: true
                Layout.fillHeight: true
            }
        }
    }
}
