import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// KDE-native settings dialog: fields in a Kirigami.FormLayout, persisted via
// QSettings through Mgr (which owns the QSettings store). Global tools live
// one level deeper — reached from here, not from the sidebar.
Controls.Dialog {
    id: dialog
    title: qsTr("Settings")
    modal: true
    standardButtons: Controls.Dialog.Close
    closePolicy: Controls.Popup.CloseOnEscape

    signal openGlobalTools()

    Component.onCompleted: reload()
    function reload() {
        hostField.text = Mgr.host
        portField.text = Mgr.port
        tlsBox.checked = Mgr.useTls
        keyField.text = Mgr.secretKey
        cwdField.text = Mgr.workingDir
        autoBox.checked = Mgr.autoConnectEnabled
        notifyBox.checked = Mgr.notificationsEnabled
        configuredOnlyBox.checked = Mgr.configuredProvidersOnly
        pushBox.checked = Push.enabled
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
        Mgr.notificationsEnabled = notifyBox.checked
        Mgr.configuredProvidersOnly = configuredOnlyBox.checked
    }
    onClosed: commit()

    property bool testOk: false
    property string testMessage: ""
    property bool testing: false

    Connections {
        target: Mgr
        function onConnectionTested(ok, message) {
            dialog.testing = false
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
                onClicked: {
                    dialog.commit()
                    dialog.testing = true
                    dialog.testMessage = qsTr("Testing…")
                    Mgr.testConnection()
                }
            }
            Controls.Label {
                visible: dialog.testMessage.length > 0
                text: dialog.testMessage
                color: dialog.testing ? Kirigami.Theme.textColor
                                      : (dialog.testOk ? Kirigami.Theme.positiveTextColor
                                                       : Kirigami.Theme.negativeTextColor)
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
            // Desktop notifications come from this client's own connection (turn
            // finished, approval needed, a session changed elsewhere) — no server
            // support involved. Shown only when the window isn't the active one.
            Controls.CheckBox {
                id: notifyBox
                text: qsTr("Notify when the window isn't focused")
                Kirigami.FormData.label: qsTr("Notifications:")
            }
            // Picker noise control: goose's inventory marks which providers are actually
            // configured; this hides the rest. The current pick is always kept.
            Controls.CheckBox {
                id: configuredOnlyBox
                text: qsTr("Show configured providers only")
                Kirigami.FormData.label: qsTr("Providers:")
            }
            // UnifiedPush is receive-only here: the app registers with the desktop's
            // distributor and shows whatever your own senders POST to that endpoint.
            // A stock goose server never pushes — nothing depends on this.
            Controls.CheckBox {
                id: pushBox
                text: qsTr("Register with the desktop's UnifiedPush distributor")
                Kirigami.FormData.label: qsTr("Push:")
                onToggled: Push.enabled = checked
            }
            Controls.TextField {
                readOnly: true
                visible: Push.endpoint.length > 0
                text: Push.endpoint
                Kirigami.FormData.label: qsTr("Push endpoint:")
                Controls.ToolTip.visible: hovered
                Controls.ToolTip.text: qsTr("What your own sender POSTs to (receive-only)")
            }
            Controls.Label {
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                visible: Push.status.length > 0
                text: qsTr("UnifiedPush: %1 — Grouse ships no sender; your goose-side scripts are the sender.").arg(Push.status)
                color: Kirigami.Theme.disabledTextColor
                font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
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
            // Global tools (config.yaml extensions) are reached from Settings,
            // not the sidebar.
            Controls.Button {
                text: qsTr("Global tools…")
                icon.name: "configure"
                Kirigami.FormData.label: ""
                onClicked: { dialog.close(); dialog.openGlobalTools() }
            }
        }
    }
}
