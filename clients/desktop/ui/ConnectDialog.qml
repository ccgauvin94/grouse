import QtQuick
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

Controls.Dialog {
    id: dialog
    title: qsTr("Connect to goosed")
    modal: true
    standardButtons: Controls.Dialog.Save | Controls.Dialog.Cancel
    closePolicy: Controls.Popup.CloseOnEscape

    // Initial text is loaded once from Mgr on open; edits push to Mgr on change.
    // (Two-way `text:`/`onTextChanged` binding causes a QML binding loop.)
    Component.onCompleted: reload()
    function reload() {
        hostField.text = Mgr.host
        portField.text = Mgr.port
        keyField.text = Mgr.secretKey
        cwdField.text = Mgr.workingDir
        tlsBox.checked = Mgr.useTls
        dialog.testMessage = ""
    }

    property bool testOk: false
    property string testMessage: ""

    Connections {
        target: Mgr
        function onConnectionTested(ok, message) {
            dialog.testOk = ok
            dialog.testMessage = message
        }
    }

    onOpened: reload()

    contentItem: Column {
        spacing: Kirigami.Units.smallSpacing
        width: 420

        Controls.Label { text: qsTr("Host / tailnet IP") }
        Controls.TextField {
            id: hostField
            width: parent.width
            onTextChanged: Mgr.host = text
        }

        Controls.Label { text: qsTr("Port") }
        Controls.TextField {
            id: portField
            width: parent.width
            inputMethodHints: Qt.ImhDigitsOnly
            onTextChanged: Mgr.port = text
        }

        Controls.CheckBox {
            id: tlsBox
            text: qsTr("Use TLS (wss)")
            onToggled: Mgr.useTls = checked
        }

        Controls.Label { text: qsTr("Secret key") }
        Controls.TextField {
            id: keyField
            width: parent.width
            echoMode: TextInput.Password
            onTextChanged: Mgr.secretKey = text
        }

        // Probe the endpoint before committing: reachability, TLS, secret key,
        // and the ACP handshake — reported here instead of a silent landing page.
        // Kept next to the credentials (not at the bottom, where short screens
        // clip the dialog's last row).
        Controls.Button {
            width: parent.width
            implicitHeight: Kirigami.Units.gridUnit * 2
            text: qsTr("Test connection")
            icon.name: "network-connect"
            onClicked: Mgr.testConnection()
        }
        Controls.Label {
            width: parent.width
            visible: dialog.testMessage.length > 0
            text: dialog.testMessage
            color: dialog.testOk ? Kirigami.Theme.positiveTextColor
                                 : Kirigami.Theme.negativeTextColor
            wrapMode: Text.Wrap
            font.pixelSize: Math.max(11, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.9))
        }

        Controls.Label { text: qsTr("Server working directory (for new chats)") }
        Controls.TextField {
            id: cwdField
            width: parent.width
            placeholderText: qsTr("e.g. /home/colin/Projects/Inbox")
            onTextChanged: Mgr.workingDir = text
        }
    }
}
