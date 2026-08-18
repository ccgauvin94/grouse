import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

Controls.Dialog {
    id: dialog
    title: qsTr("Scheduler")
    modal: true
    standardButtons: Controls.Dialog.Close
    closePolicy: Controls.Popup.CloseOnEscape
    width: Math.min(700, parent ? parent.width - 32 : 700)
    height: Math.min(560, parent ? parent.height - 32 : 560)

    onOpened: Mgr.refreshRecipes()

    contentItem: ColumnLayout {
        spacing: Kirigami.Units.smallSpacing

        RowLayout {
            Layout.fillWidth: true
            Controls.Label {
                text: qsTr("Scheduled jobs")
                font.weight: Font.DemiBold
                Layout.fillWidth: true
            }
            Controls.ToolButton {
                icon.name: "view-refresh"
                display: Controls.AbstractButton.IconOnly
                Controls.ToolTip.visible: hovered
                Controls.ToolTip.text: qsTr("Refresh scheduler")
                onClicked: Mgr.refreshRecipes()
            }
        }

        ListView {
            id: scheduleList
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            spacing: Kirigami.Units.smallSpacing
            model: Mgr.schedules
            Controls.ScrollBar.vertical: Controls.ScrollBar { policy: Controls.ScrollBar.AsNeeded }

            delegate: Rectangle {
                width: ListView.view.width
                height: scheduleRow.implicitHeight + Kirigami.Units.largeSpacing * 2
                radius: Kirigami.Units.smallSpacing
                color: Kirigami.Theme.alternateBackgroundColor
                border.color: Kirigami.Theme.separatorColor
                border.width: 1

                RowLayout {
                    id: scheduleRow
                    anchors.fill: parent
                    anchors.margins: Kirigami.Units.largeSpacing
                    spacing: Kirigami.Units.smallSpacing
                    ColumnLayout {
                        Layout.fillWidth: true
                        Controls.Label {
                            text: modelData.source || modelData.id
                            font.weight: Font.DemiBold
                            elide: Text.ElideRight
                            Layout.fillWidth: true
                        }
                        Controls.Label {
                            text: (modelData.cron || qsTr("No cron expression"))
                                  + (modelData.running ? " · " + qsTr("running") : "")
                                  + (modelData.lastRun ? " · " + qsTr("last run") : "")
                            color: Kirigami.Theme.disabledTextColor
                            elide: Text.ElideRight
                            Layout.fillWidth: true
                        }
                    }
                    Controls.ToolButton {
                        icon.name: "media-playback-start"
                        display: Controls.AbstractButton.IconOnly
                        enabled: !modelData.running
                        Controls.ToolTip.visible: hovered
                        Controls.ToolTip.text: qsTr("Run now")
                        onClicked: Mgr.runScheduleNow(modelData.id)
                    }
                    Controls.Switch {
                        checked: !modelData.paused
                        onToggled: Mgr.setSchedulePaused(modelData.id, !checked)
                        Controls.ToolTip.visible: hovered
                        Controls.ToolTip.text: checked ? qsTr("Pause schedule") : qsTr("Enable schedule")
                    }
                }
            }

            footer: Controls.Label {
                visible: Mgr.schedules.length === 0
                text: qsTr("No scheduled jobs.")
                color: Kirigami.Theme.disabledTextColor
                horizontalAlignment: Text.AlignHCenter
                width: scheduleList.width
                padding: Kirigami.Units.largeSpacing
            }
        }
    }
}
