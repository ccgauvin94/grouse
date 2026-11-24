import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// Raw config.yaml editor. The server owns the file: every change here goes
// through the core's config intents, never through a client-side cache.
Controls.Dialog {
    id: dialog
    title: qsTr("Goose config")
    modal: true
    standardButtons: Controls.Dialog.Close
    closePolicy: Controls.Popup.CloseOnEscape
    width: 680
    height: 540

    property string selectedKey: ""
    property bool selectedSecret: false
    property bool awaitingRead: false
    property bool deleteArmed: false
    property string detailHint: ""

    function refreshGooseConfig() { Mgr.refreshGooseConfig() }

    function filteredRows(rows, text) {
        const needle = text.trim().toLowerCase()
        return rows.filter(function (row) {
            return !needle || row.key.toLowerCase().indexOf(needle) >= 0
        })
    }

    function findRow(key) {
        const rows = Mgr.gooseConfig
        for (let i = 0; i < rows.length; i++)
            if (rows[i].key === key) return rows[i]
        return null
    }

    function select(key, secret) {
        selectedKey = key
        selectedSecret = secret
        deleteArmed = false
        awaitingRead = false
        // A secret is never copied into the editor: its row shows a mask and
        // the field is empty, so pressing Save can only REPLACE the value.
        valueField.text = secret ? "" : (findRow(key) ? findRow(key).value : "")
        detailHint = secret
            ? qsTr("Secret value hidden. Type a new value to replace it.")
            : ""
    }

    function addKey() {
        const key = newKeyField.text.trim()
        if (!key.length)
            return
        const known = findRow(key)
        selectedKey = key
        selectedSecret = known ? known.secret : newSecretBox.checked
        deleteArmed = false
        // Re-read by name: read-all cannot list secret keys, so a secret that
        // already exists on the server is only reachable this way.
        awaitingRead = true
        valueField.text = ""
        detailHint = ""
        newKeyField.text = ""
        Mgr.readGooseConfig(key, selectedSecret)
    }

    function save() {
        if (!selectedKey.length || (selectedSecret && !valueField.text.length))
            return
        Mgr.setGooseConfig(selectedKey, valueField.text, selectedSecret)
        detailHint = qsTr("Saved. Changes apply to new sessions.")
        if (selectedSecret)
            valueField.text = ""
    }

    function remove() {
        if (!selectedKey.length)
            return
        Mgr.removeGooseConfig(selectedKey, selectedSecret)
        selectedKey = ""
        valueField.text = ""
        deleteArmed = false
        detailHint = qsTr("Removed.")
    }

    onOpened: dialog.refreshGooseConfig()
    onClosed: {
        selectedKey = ""
        selectedSecret = false
        awaitingRead = false
        deleteArmed = false
        detailHint = ""
        valueField.text = ""
        newKeyField.text = ""
        searchField.text = ""
    }

    Connections {
        target: Mgr
        function onServerConfigChanged() {
            if (!dialog.awaitingRead || !dialog.findRow(dialog.selectedKey))
                return
            dialog.awaitingRead = false
            dialog.select(dialog.selectedKey, dialog.selectedSecret)
        }
    }

    contentItem: ColumnLayout {
        spacing: Kirigami.Units.smallSpacing

        Controls.Label {
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            opacity: 0.8
            font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.9
            text: qsTr("Environment variables override config. Changes apply to new sessions. Read-all lists non-secret config only.")
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing
            Controls.TextField {
                id: searchField
                Layout.fillWidth: true
                placeholderText: qsTr("Filter keys…")
            }
            Controls.Button {
                text: qsTr("Refresh")
                icon.name: "view-refresh"
                onClicked: dialog.refreshGooseConfig()
            }
        }

        ListView {
            id: keyList
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            model: dialog.filteredRows(Mgr.gooseConfig, searchField.text)
            Controls.ScrollBar.vertical: Controls.ScrollBar {
                policy: Controls.ScrollBar.AsNeeded
            }
            delegate: Controls.ItemDelegate {
                width: ListView.view.width - Kirigami.Units.gridUnit * 1.5
                highlighted: modelData.key === dialog.selectedKey
                onClicked: dialog.select(modelData.key, modelData.secret)
                contentItem: RowLayout {
                    spacing: Kirigami.Units.smallSpacing
                    Controls.Label {
                        text: modelData.key
                        font.bold: true
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }
                    Controls.Label {
                        visible: modelData.secret
                        text: qsTr("secret")
                        opacity: 0.7
                        font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
                    }
                    Controls.Label {
                        text: modelData.value
                        opacity: 0.7
                        elide: Text.ElideRight
                        Layout.maximumWidth: keyList.width / 3
                        textFormat: Text.PlainText
                    }
                }
            }
        }

        Kirigami.Separator { Layout.fillWidth: true }

        Controls.Label {
            Layout.fillWidth: true
            text: dialog.selectedKey.length ? qsTr("Edit: %1").arg(dialog.selectedKey)
                                            : qsTr("Select a key, or add one below")
            font.bold: dialog.selectedKey.length > 0
        }
        Controls.TextField {
            id: valueField
            Layout.fillWidth: true
            enabled: dialog.selectedKey.length > 0
            echoMode: dialog.selectedSecret ? TextInput.Password : TextInput.Normal
            placeholderText: dialog.selectedSecret
                ? qsTr("New secret value (replaces the stored one)")
                : qsTr("Value")
        }
        Controls.Label {
            Layout.fillWidth: true
            visible: dialog.detailHint.length > 0
            wrapMode: Text.Wrap
            opacity: 0.7
            text: dialog.detailHint
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing
            Controls.Button {
                text: qsTr("Save")
                icon.name: "document-save"
                enabled: dialog.selectedKey.length > 0
                         && (!dialog.selectedSecret || valueField.text.length > 0)
                onClicked: dialog.save()
            }
            Controls.Button {
                text: dialog.deleteArmed ? qsTr("Cancel delete") : qsTr("Delete…")
                icon.name: dialog.deleteArmed ? "dialog-cancel" : "edit-delete"
                enabled: dialog.selectedKey.length > 0
                onClicked: dialog.deleteArmed = !dialog.deleteArmed
            }
            Controls.Button {
                visible: dialog.deleteArmed
                text: qsTr("Confirm delete")
                onClicked: dialog.remove()
            }
            Controls.Label {
                Layout.fillWidth: true
                visible: dialog.deleteArmed
                wrapMode: Text.Wrap
                color: Kirigami.Theme.negativeTextColor
                text: qsTr("Delete %1 from the server's config?").arg(dialog.selectedKey)
            }
        }

        Kirigami.Separator {
            Layout.fillWidth: true
            Layout.topMargin: Kirigami.Units.smallSpacing
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing
            Controls.TextField {
                id: newKeyField
                Layout.fillWidth: true
                placeholderText: qsTr("New key (e.g. GOOGLE_API_KEY)")
                onAccepted: dialog.addKey()
            }
            Controls.CheckBox {
                id: newSecretBox
                text: qsTr("Secret")
                Controls.ToolTip.visible: hovered
                Controls.ToolTip.text: qsTr("Secret keys are never listed by read-all; you must name them to edit them.")
            }
            Controls.Button {
                text: qsTr("Add / open")
                icon.name: "list-add"
                onClicked: dialog.addKey()
            }
        }
    }
}
