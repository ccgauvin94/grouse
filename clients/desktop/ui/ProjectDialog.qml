import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// Shows a project's summary — the projects/<name>.md source file content goose
// stores for it (plus its root working dir). Opened by clicking a project
// header in the sidebar. The summary is EDITABLE: sources/update is the API for
// this text, and it is a whole-source replace, so saving passes the project's
// name/description back unchanged alongside the new content.
Controls.Dialog {
    id: dialog
    title: qsTr("Project")
    modal: true
    standardButtons: Controls.Dialog.Close
    closePolicy: Controls.Popup.CloseOnEscape
    width: Math.min(560, parent ? parent.width - 32 : 560)
    height: Math.min(520, parent ? parent.height - 32 : 520)

    property string projectId
    // True once the textarea diverges from the model's content; gates Save.
    property bool summaryDirty: false

    function openFor(id, name) {
        dialog.projectId = id
        dialog.title = name
        // Set the text imperatively: a binding would fight the user's edits.
        summaryArea.text = (dialog.proj() && dialog.proj().content) || ""
        dialog.summaryDirty = false
        dialog.open()
    }
    function proj() {
        for (var i = 0; i < Mgr.projects.length; i++)
            if (Mgr.projects[i].id === dialog.projectId)
                return Mgr.projects[i]
        return null
    }

    contentItem: ColumnLayout {
        spacing: Kirigami.Units.smallSpacing

        Controls.Label {
            text: qsTr("Summary (markdown — goose reads this as the project's own instructions; a first \"root: /dir\" line sets the working dir)")
            font.weight: Font.DemiBold
            wrapMode: Text.Wrap
            Layout.fillWidth: true
        }
        Controls.TextArea {
            id: summaryArea
            Layout.fillWidth: true
            Layout.fillHeight: true
            wrapMode: Text.WrapAnywhere
            placeholderText: qsTr("No summary written yet — write one.")
            onTextChanged: {
                const original = (dialog.proj() && dialog.proj().content) || ""
                dialog.summaryDirty = text !== original
            }
        }
        RowLayout {
            Layout.fillWidth: true
            Controls.Button {
                visible: dialog.proj() !== null && dialog.summaryDirty
                icon.name: "document-save"
                text: qsTr("Save summary")
                onClicked: {
                    const p = dialog.proj()
                    Mgr.updateProject(p.id, p.name, p.description || "", summaryArea.text)
                    dialog.summaryDirty = false
                }
            }
            Item { Layout.fillWidth: true }
            Controls.Label {
                text: dialog.proj() && dialog.proj().root
                      ? qsTr("Root: ") + dialog.proj().root : ""
                color: Kirigami.Theme.disabledTextColor
                visible: text.length > 0
            }
        }
        Controls.Label {
            text: dialog.proj() && dialog.proj().description
                  ? qsTr("Description: ") + dialog.proj().description : ""
            color: Kirigami.Theme.disabledTextColor
            visible: text.length > 0
            wrapMode: Text.Wrap
            Layout.fillWidth: true
        }
    }
}
