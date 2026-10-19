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
            textFormat: Text.PlainText
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
                // NO textFormat here: `Controls.Button` has no such property, and
                // assigning one does not merely warn — the whole type fails to
                // construct, which takes `PermissionDialog` (and therefore
                // main.qml, since every dialog is a declared instance) down with
                // it. Button text is already plain: rich text is a Label/Text
                // feature, so the "render server text as plain text" rule needs
                // nothing here.
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
