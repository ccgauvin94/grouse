import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// Shows a project's summary — the projects/<name>.md source file content goose
// stores for it (plus its root working dir). Opened by clicking a project
// header in the sidebar.
Controls.Dialog {
    id: dialog
    title: qsTr("Project")
    modal: true
    standardButtons: Controls.Dialog.Close
    closePolicy: Controls.Popup.CloseOnEscape
    width: Math.min(560, parent ? parent.width - 32 : 560)
    height: Math.min(520, parent ? parent.height - 32 : 520)

    property string projectId

    function openFor(id, name) {
        dialog.projectId = id
        dialog.title = name
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
            text: qsTr("Summary")
            font.weight: Font.DemiBold
        }
        Controls.TextArea {
            Layout.fillWidth: true
            Layout.fillHeight: true
            readOnly: true
            wrapMode: Text.WrapAnywhere
            text: dialog.proj() ? (dialog.proj().content || qsTr("(no summary written yet)"))
                                : qsTr("Project not found.")
            placeholderText: qsTr("No summary.")
        }
        Controls.Label {
            text: dialog.proj() && dialog.proj().root
                  ? qsTr("Root: ") + dialog.proj().root : ""
            color: Kirigami.Theme.disabledTextColor
            visible: text.length > 0
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
