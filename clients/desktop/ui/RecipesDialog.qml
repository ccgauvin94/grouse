import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

Controls.Dialog {
    id: dialog
    title: qsTr("Recipes")
    modal: true
    standardButtons: Controls.Dialog.Close
    closePolicy: Controls.Popup.CloseOnEscape
    width: Math.min(700, parent ? parent.width - 32 : 700)
    height: Math.min(620, parent ? parent.height - 32 : 620)

    onOpened: Mgr.refreshRecipes()

    contentItem: ColumnLayout {
        spacing: Kirigami.Units.smallSpacing

        RowLayout {
            Layout.fillWidth: true
            Controls.Label {
                text: qsTr("Saved recipes")
                font.weight: Font.DemiBold
                Layout.fillWidth: true
            }
            Controls.Label {
                text: Mgr.recipes.length
                color: Kirigami.Theme.disabledTextColor
            }
            Controls.ToolButton {
                icon.name: "view-refresh"
                display: Controls.AbstractButton.IconOnly
                Controls.ToolTip.visible: hovered
                Controls.ToolTip.text: qsTr("Refresh recipes")
                onClicked: Mgr.refreshRecipes()
            }
        }

        ListView {
            id: recipeList
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            spacing: Kirigami.Units.smallSpacing
            model: Mgr.recipes
            Controls.ScrollBar.vertical: Controls.ScrollBar { policy: Controls.ScrollBar.AsNeeded }

            delegate: Rectangle {
                width: ListView.view.width
                implicitHeight: recipeColumn.implicitHeight + Kirigami.Units.largeSpacing * 2
                radius: Kirigami.Units.smallSpacing
                color: Kirigami.Theme.alternateBackgroundColor
                border.color: Kirigami.Theme.separatorColor
                border.width: 1

                ColumnLayout {
                    id: recipeColumn
                    anchors.fill: parent
                    anchors.margins: Kirigami.Units.largeSpacing
                    spacing: Kirigami.Units.smallSpacing

                    RowLayout {
                        Layout.fillWidth: true
                        Controls.Label {
                            text: modelData.title || modelData.id
                            font.weight: Font.DemiBold
                            Layout.fillWidth: true
                            elide: Text.ElideRight
                        }
                        Controls.Button {
                            text: qsTr("Start session")
                            icon.name: "media-playback-start"
                            onClicked: Mgr.runRecipe(modelData.id)
                        }
                    }
                    Controls.Label {
                        text: modelData.description || qsTr("No description")
                        color: Kirigami.Theme.disabledTextColor
                        wrapMode: Text.Wrap
                        Layout.fillWidth: true
                        visible: text.length > 0
                    }
                    RowLayout {
                        Layout.fillWidth: true
                        Controls.TextField {
                            id: cronField
                            Layout.fillWidth: true
                            placeholderText: qsTr("Cron: sec min hour day month weekday")
                            text: modelData.cron || ""
                        }
                        Controls.Button {
                            text: qsTr("Schedule")
                            icon.name: "appointment-new"
                            onClicked: Mgr.scheduleRecipe(modelData.id, cronField.text.trim())
                        }
                        Controls.Button {
                            text: qsTr("Unschedule")
                            flat: true
                            enabled: cronField.text.length > 0
                            onClicked: {
                                Mgr.scheduleRecipe(modelData.id, "")
                                cronField.text = ""
                            }
                        }
                    }
                }
            }

            footer: Controls.Label {
                visible: Mgr.recipes.length === 0
                text: qsTr("No recipes found.")
                color: Kirigami.Theme.disabledTextColor
                horizontalAlignment: Text.AlignHCenter
                width: recipeList.width
                padding: Kirigami.Units.largeSpacing
            }
        }
    }
}
