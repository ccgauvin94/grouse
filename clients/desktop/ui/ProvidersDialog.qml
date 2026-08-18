import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// Configure the server's global model choices (config.yaml keys; they apply to
// NEW sessions, mirroring the Android client). Values stream in asynchronously
// via Mgr.readServerConfig into the shared Mgr.serverConfig map, so each field
// binds to its own key and only ever WRITES on explicit user edits
// (onTextEdited) — never in response to the map updating.
Controls.Dialog {
    id: dialog
    title: qsTr("Providers")
    modal: true
    standardButtons: Controls.Dialog.Close
    closePolicy: Controls.Popup.CloseOnEscape

    onOpened: {
        Mgr.readServerConfig("GOOSE_PROVIDER")
        Mgr.readServerConfig("GOOSE_MODEL")
        Mgr.readServerConfig("GOOSE_FAST_MODEL")
        Mgr.readServerConfig("VISION_PROVIDER")
        Mgr.readServerConfig("VISION_MODEL")
    }

    contentItem: ColumnLayout {
        spacing: Kirigami.Units.smallSpacing
        implicitWidth: 560

        Controls.Label { text: qsTr("Chat"); font.bold: true }
        RowLayout {
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing
            Controls.Label { text: qsTr("Provider"); Layout.preferredWidth: 70 }
            Controls.TextField {
                id: chatProviderField
                Layout.fillWidth: true
                text: Mgr.serverConfig["GOOSE_PROVIDER"] || ""
                onTextEdited: Mgr.setServerConfig("GOOSE_PROVIDER", text)
            }
            Controls.Button {
                text: qsTr("Refresh models")
                icon.name: "view-refresh"
                onClicked: Mgr.refreshSupportedModels(chatProviderField.text)
            }
        }
        RowLayout {
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing
            Controls.Label { text: qsTr("Model"); Layout.preferredWidth: 70 }
            Controls.TextField {
                id: chatModelField
                Layout.fillWidth: true
                text: Mgr.serverConfig["GOOSE_MODEL"] || ""
                onTextEdited: Mgr.setServerConfig("GOOSE_MODEL", text)
            }
            Controls.ComboBox {
                editable: true
                implicitWidth: 240
                model: Mgr.supportedModels
                onAccepted: Mgr.setServerConfig("GOOSE_MODEL", editText)
                onActivated: Mgr.setServerConfig("GOOSE_MODEL", currentText)
            }
        }

        Controls.Label {
            text: qsTr("Fast")
            font.bold: true
            Layout.topMargin: Kirigami.Units.gridUnit
        }
        RowLayout {
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing
            Controls.CheckBox {
                id: fastEnabledBox
                text: qsTr("Enabled")
                Layout.preferredWidth: 70
                checked: (Mgr.serverConfig["GOOSE_FAST_MODEL"] || "") !== ""
                onToggled: {
                    if (!checked)
                        Mgr.setServerConfig("GOOSE_FAST_MODEL", "")
                }
            }
            Controls.TextField {
                id: fastProviderField
                Layout.fillWidth: true
                text: Mgr.serverConfig["GOOSE_PROVIDER"] || ""
                onTextEdited: Mgr.setServerConfig("GOOSE_PROVIDER", text)
            }
            Controls.Button {
                text: qsTr("Refresh models")
                icon.name: "view-refresh"
                onClicked: Mgr.refreshSupportedModels(fastProviderField.text)
            }
        }
        RowLayout {
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing
            Controls.Label { text: qsTr("Model"); Layout.preferredWidth: 70 }
            Controls.TextField {
                id: fastModelField
                Layout.fillWidth: true
                text: Mgr.serverConfig["GOOSE_FAST_MODEL"] || ""
                onTextEdited: Mgr.setServerConfig("GOOSE_FAST_MODEL", text)
            }
            Controls.ComboBox {
                editable: true
                implicitWidth: 240
                model: Mgr.supportedModels
                onAccepted: Mgr.setServerConfig("GOOSE_FAST_MODEL", editText)
                onActivated: Mgr.setServerConfig("GOOSE_FAST_MODEL", currentText)
            }
        }

        Controls.Label {
            text: qsTr("Vision")
            font.bold: true
            Layout.topMargin: Kirigami.Units.gridUnit
        }
        RowLayout {
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing
            Controls.CheckBox {
                id: visionEnabledBox
                text: qsTr("Enabled")
                Layout.preferredWidth: 70
                checked: (Mgr.serverConfig["VISION_MODEL"] || "") !== ""
                onToggled: {
                    if (!checked)
                        Mgr.setServerConfig("VISION_MODEL", "")
                }
            }
            Controls.TextField {
                id: visionProviderField
                Layout.fillWidth: true
                text: Mgr.serverConfig["VISION_PROVIDER"] || ""
                onTextEdited: Mgr.setServerConfig("VISION_PROVIDER", text)
            }
            Controls.Button {
                text: qsTr("Refresh models")
                icon.name: "view-refresh"
                onClicked: Mgr.refreshSupportedModels(visionProviderField.text)
            }
        }
        RowLayout {
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing
            Controls.Label { text: qsTr("Model"); Layout.preferredWidth: 70 }
            Controls.TextField {
                id: visionModelField
                Layout.fillWidth: true
                text: Mgr.serverConfig["VISION_MODEL"] || ""
                onTextEdited: Mgr.setServerConfig("VISION_MODEL", text)
            }
            Controls.ComboBox {
                editable: true
                implicitWidth: 240
                model: Mgr.supportedModels
                onAccepted: Mgr.setServerConfig("VISION_MODEL", editText)
                onActivated: Mgr.setServerConfig("VISION_MODEL", currentText)
            }
        }

        Controls.Label {
            text: qsTr("These apply to new chats (config.yaml).")
            opacity: 0.7
            Layout.topMargin: Kirigami.Units.gridUnit
        }
    }
}
