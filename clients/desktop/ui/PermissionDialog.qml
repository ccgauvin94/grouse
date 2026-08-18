import QtQuick
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

Controls.Dialog {
    id: dialog
    title: qsTr("Tool approval")
    modal: true
    closePolicy: Controls.Popup.NoAutoClose

    property string pendingToolCallId: ""
    property string appTitle: ""

    contentItem: Column {
        width: 420
        spacing: Kirigami.Units.smallSpacing

        Kirigami.Heading {
            text: dialog.appTitle || qsTr("Tool approval requested")
            level: 3
            width: parent.width
            wrapMode: Text.Wrap
        }

        Controls.Label {
            text: qsTr("%1 wants to run a tool on the goose server. What should I do?")
                      .arg(qsTr("Goose"))
            width: parent.width
            wrapMode: Text.Wrap
        }

        Repeater {
            model: Mgr.permissionOptions()
            delegate: Controls.Button {
                text: modelData.name
                flat: true
                width: parent.width
                onClicked: {
                    Mgr.respondPermission(dialog.pendingToolCallId, modelData.optionId)
                    dialog.close()
                }
            }
        }

        Controls.Button {
            text: qsTr("Deny")
            flat: true
            width: parent.width
            onClicked: {
                Mgr.respondPermission(dialog.pendingToolCallId, "")
                dialog.close()
            }
        }
    }

    onClosed: {
        // Any other dismissal (Escape) is also a denial.
        if (pendingToolCallId.length > 0) {
            Mgr.respondPermission(pendingToolCallId, "")
            pendingToolCallId = ""
        }
    }
}
