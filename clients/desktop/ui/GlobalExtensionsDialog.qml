import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// Global (config.yaml) extension controls — the defaults applied to NEW sessions,
// mirroring the Android client's Extensions screen. Per-extension enable switches
// plus per-tool allowlists for mcp-backed extensions. Changes take effect for new
// chats (existing sessions keep their own profiles).
Controls.Dialog {
    id: dialog
    title: qsTr("Tools")
    modal: true
    standardButtons: Controls.Dialog.Close
    closePolicy: Controls.Popup.CloseOnEscape
    width: 520
    height: 560

    onOpened: Mgr.refreshGlobalExtensions()

    contentItem: ColumnLayout {
        spacing: Kirigami.Units.smallSpacing

        Controls.Label {
            text: qsTr("Globally enabled extensions — applied to new chats")
            opacity: 0.7
            Layout.fillWidth: true
        }

        ListView {
            id: extList
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            spacing: 2
            model: Mgr.globalExtensions
            Controls.ScrollBar.vertical: Controls.ScrollBar { policy: Controls.ScrollBar.AsNeeded }

            delegate: Column {
                id: edel
                width: ListView.view.width
                spacing: 2
                property bool expanded: false
                readonly property string eName: modelData.name
                readonly property bool eAttrib: modelData.attrib
                readonly property bool eEnabled: modelData.enabled

                RowLayout {
                    width: parent.width
                    spacing: Kirigami.Units.smallSpacing
                    Controls.Button {
                        flat: true
                        display: Controls.AbstractButton.IconOnly
                        icon.name: edel.expanded ? "arrow-down" : "arrow-right"
                        implicitWidth: Kirigami.Units.gridUnit
                        Layout.preferredWidth: Kirigami.Units.gridUnit
                        Layout.preferredHeight: Kirigami.Units.gridUnit * 1.25
                        visible: edel.eAttrib
                        onClicked: edel.expanded = !edel.expanded
                    }
                    Controls.Switch {
                        Layout.preferredWidth: Kirigami.Units.gridUnit * 2.5
                        checked: edel.eEnabled
                        onToggled: Mgr.setGlobalExtensionEnabled(edel.eName, checked)
                    }
                    Controls.Label {
                        text: edel.eName
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                        font.bold: true
                    }
                    Controls.Label {
                        text: modelData.type
                        opacity: 0.6
                    }
                }

                Column {
                    width: parent.width
                    visible: edel.expanded
                    Repeater {
                        model: edel.eAttrib ? modelData.tools : []
                        delegate: RowLayout {
                            width: parent.width
                            spacing: Kirigami.Units.smallSpacing
                            Controls.Switch {
                                Layout.preferredWidth: Kirigami.Units.gridUnit * 2.5
                                checked: modelData.on
                                enabled: edel.eEnabled
                                onToggled: Mgr.setGlobalToolEnabled(edel.eName, modelData.name, checked)
                            }
                            Controls.Label {
                                text: modelData.name
                                elide: Text.ElideRight
                                Layout.fillWidth: true
                            }
                        }
                    }
                    Controls.Label {
                        visible: edel.eAttrib && modelData.tools.length === 0
                        text: qsTr("Tool list is empty — open the tools panel in a chat to build it")
                        opacity: 0.6
                        font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
                    }
                }
            }
        }
    }
}
