import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// KDE-native settings dialog: fields in a Kirigami.FormLayout, persisted via
// QSettings through Mgr (which owns the QSettings store). Providers and global
// tools live one level deeper — reached from here, not from the sidebar.
Controls.Dialog {
    id: dialog
    title: qsTr("Settings")
    modal: true
    standardButtons: Controls.Dialog.Close
    closePolicy: Controls.Popup.CloseOnEscape

    signal openProviders()
    signal openGlobalTools()

    Component.onCompleted: reload()
    function reload() {
        hostField.text = Mgr.host
        portField.text = Mgr.port
        tlsBox.checked = Mgr.useTls
        keyField.text = Mgr.secretKey
        cwdField.text = Mgr.workingDir
        autoBox.checked = Mgr.autoConnectEnabled
        dialog.testMessage = ""
    }
    onOpened: reload()
    // Persist everything up front so closing is always safe.
    function commit() {
        Mgr.host = hostField.text
        Mgr.port = portField.text
        Mgr.useTls = tlsBox.checked
        Mgr.secretKey = keyField.text
        Mgr.workingDir = cwdField.text
        Mgr.autoConnectEnabled = autoBox.checked
    }
    onClosed: commit()

    property bool testOk: false
    property string testMessage: ""

    Connections {
        target: Mgr
        function onConnectionTested(ok, message) {
            dialog.testOk = ok
            dialog.testMessage = message
        }
    }

    contentItem: ColumnLayout {
        spacing: Kirigami.Units.smallSpacing
        implicitWidth: 520

        Kirigami.FormLayout {
            Layout.fillWidth: true
            Layout.topMargin: Kirigami.Units.smallSpacing

            Controls.Label {
                text: qsTr("Connection")
                font.bold: true
                Kirigami.FormData.label: ""
            }
            Controls.TextField {
                id: hostField
                Kirigami.FormData.label: qsTr("Host / tailnet IP:")
                placeholderText: qsTr("e.g. 192.168.1.5 or host.example.net")
            }
            Controls.TextField {
                id: portField
                Kirigami.FormData.label: qsTr("Port:")
                inputMethodHints: Qt.ImhDigitsOnly
                placeholderText: qsTr("3284")
            }
            Controls.CheckBox {
                id: tlsBox
                text: qsTr("Use TLS (wss)")
                Kirigami.FormData.label: qsTr("Transport:")
            }
            Controls.TextField {
                id: keyField
                Kirigami.FormData.label: qsTr("Secret key:")
                echoMode: TextInput.Password
            }
            // Probe the endpoint with the current fields (committed first —
            // Settings persists on close, so the fields aren't on Mgr yet).
            Controls.Button {
                Layout.fillWidth: true
                implicitHeight: Kirigami.Units.gridUnit * 2
                text: qsTr("Test connection")
                icon.name: "network-connect"
                Kirigami.FormData.label: ""
                onClicked: { dialog.commit(); Mgr.testConnection() }
            }
            Controls.Label {
                visible: dialog.testMessage.length > 0
                text: dialog.testMessage
                color: dialog.testOk ? Kirigami.Theme.positiveTextColor
                                     : Kirigami.Theme.negativeTextColor
                wrapMode: Text.Wrap
                Kirigami.FormData.label: ""
            }

            Controls.Label {
                text: qsTr("Sessions")
                font.bold: true
                Kirigami.FormData.label: ""
                Layout.topMargin: Kirigami.Units.gridUnit
            }
            Controls.TextField {
                id: cwdField
                Kirigami.FormData.label: qsTr("Working directory:")
                placeholderText: qsTr("e.g. /home/colin/Projects/Inbox")
            }
            Controls.CheckBox {
                id: autoBox
                text: qsTr("Connect automatically on launch")
                Kirigami.FormData.label: qsTr("Startup:")
            }

            Kirigami.Separator {
                Layout.fillWidth: true
                Layout.topMargin: Kirigami.Units.smallSpacing
                Layout.bottomMargin: Kirigami.Units.smallSpacing
            }
            // Disconnect lives here (the sidebar only offers Connect).
            Controls.Button {
                visible: Mgr.online
                text: qsTr("Disconnect")
                icon.name: "network-disconnect"
                Kirigami.FormData.label: ""
                onClicked: Mgr.disconnect()
            }
            // Providers (server model config) and global tools (config.yaml
            // extensions) are reached from Settings, not the sidebar.
            Controls.Button {
                text: qsTr("Providers…")
                icon.name: "network-server"
                Kirigami.FormData.label: ""
                onClicked: { dialog.close(); dialog.openProviders() }
            }
            Controls.Button {
                text: qsTr("Global tools…")
                icon.name: "configure"
                Kirigami.FormData.label: ""
                onClicked: { dialog.close(); dialog.openGlobalTools() }
            }
        }
    }
}
