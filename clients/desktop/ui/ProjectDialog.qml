import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// Shows and edits a project's instructions — the projects/<name>.md source
// content goose feeds to sessions filed under the project (plus its root
// working dir, a `root:` line inside that content). Opened by clicking a
// project header in the sidebar. Saves whole-source via sources/update; the
// core re-lists projects on the reply, so the labels here refresh on their own.
Controls.Dialog {
    id: dialog
    title: qsTr("Project")
    modal: true
    standardButtons: Controls.Dialog.Close
    closePolicy: Controls.Popup.CloseOnEscape
    width: Math.min(560, parent ? parent.width - 32 : 560)
    height: Math.min(520, parent ? parent.height - 32 : 520)

    property string projectId
    property string origContent: ""

    // A sources/list re-list (our own save, or another client's) lands here
    // while the dialog is open. Push it into the editor only while the user
    // hasn't dirtied it; the baseline always follows the server.
    Connections {
        target: Mgr
        function onProjectsChanged() {
            if (!dialog.visible) return
            const p = dialog.proj()
            if (!p) return
            const c = p.content || ""
            if (instructionsArea.text === dialog.origContent)
                instructionsArea.text = c
            dialog.origContent = c
        }
    }

    function openFor(id, name) {
        dialog.projectId = id
        dialog.title = name
        // Push, never bind: the TextArea's text: binding is gone once the
        // user types, so a reopened dialog must re-assign explicitly
        // (the SkillsDialog lesson).
        const p = dialog.proj()
        dialog.origContent = p ? (p.content || "") : ""
        instructionsArea.text = dialog.origContent
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
            text: qsTr("Instructions")
            font.weight: Font.DemiBold
        }
        // A bare TextArea has no flickable — long instructions were
        // unreachable past the box. ScrollView gives it one (the Recipes
        // prompt/instructions fix, same class).
        Controls.ScrollView {
            Layout.fillWidth: true
            Layout.fillHeight: true
            Controls.TextArea {
                id: instructionsArea
                readOnly: dialog.proj() ? !dialog.proj().writable : true
                wrapMode: Text.WrapAnywhere
                placeholderText: qsTr("(no instructions written yet)")
            }
        }
        Controls.Label {
            text: qsTr("These notes are given to every chat filed under this project.")
            color: Kirigami.Theme.disabledTextColor
            font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
            wrapMode: Text.Wrap
            Layout.fillWidth: true
        }
        RowLayout {
            Layout.fillWidth: true
            Controls.Label {
                text: dialog.proj() && dialog.proj().root
                      ? qsTr("Root: ") + dialog.proj().root : ""
                color: Kirigami.Theme.disabledTextColor
                visible: text.length > 0
                elide: Text.ElideMiddle
                Layout.fillWidth: true
            }
            Controls.Label {
                text: dialog.proj() && !dialog.proj().writable ? qsTr("Read-only") : ""
                color: Kirigami.Theme.disabledTextColor
                visible: text.length > 0
            }
            Controls.Button {
                text: qsTr("Save")
                icon.name: "document-save"
                enabled: dialog.proj() && dialog.proj().writable
                         && instructionsArea.text !== dialog.origContent
                onClicked: {
                    const p = dialog.proj()
                    if (!p) return
                    Mgr.saveProject(p.path, p.name, p.description || "", instructionsArea.text)
                    dialog.origContent = instructionsArea.text
                }
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
