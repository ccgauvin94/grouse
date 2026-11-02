// SPDX-License-Identifier: AGPL-3.0-or-later

import QtQuick
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

/**
 * One attached file, as the chat bubble draws it: icon by kind, name, and
 * `kind · size` underneath.
 *
 * A component rather than markup inside the delegate because this shape has
 * real rules worth testing (a long name loses its MIDDLE and keeps its suffix,
 * an unknown size drops the second line instead of printing 0 B) and because
 * tests/qml can instantiate it directly — a binding TypeError inside a delegate
 * is invisible to qmllint and to an offscreen page load.
 *
 * The composer's chip is deliberately NOT this component: it can remove the
 * file and shows a thumbnail for an image. Both take their icon/kind/size from
 * `Mgr.attachmentCard()`, so they cannot disagree about what a file is.
 */
Rectangle {
    id: card

    property string name: ""
    /** Freedesktop icon name, e.g. "application-pdf". */
    property string icon: "application-octet-stream"
    /** "PDF · 106 KB", or just the kind when the size is unknown. */
    property string subtitle: ""
    /** Card fill; the bubble tints it against its own background. */
    property color fillColor: Qt.rgba(0, 0, 0, 0.06)
    property color foreground: Kirigami.Theme.textColor

    implicitWidth: body.implicitWidth + 24
    implicitHeight: body.implicitHeight + 14
    radius: 8
    color: fillColor

    Row {
        id: body
        anchors.fill: parent
        anchors.leftMargin: 10
        anchors.rightMargin: 10
        anchors.topMargin: 7
        anchors.bottomMargin: 7
        spacing: 8

        Kirigami.Icon {
            objectName: "iconItem"
            source: card.icon
            // A mask icon is tinted by `color`; a full-colour one ignores it and
            // would show as a dark glyph on the highlighted bubble.
            isMask: true
            color: card.foreground
            implicitWidth: 20
            implicitHeight: 20
            anchors.verticalCenter: parent.verticalCenter
        }

        Column {
            width: card.width - 28
            anchors.verticalCenter: parent.verticalCenter
            spacing: 0

            Controls.Label {
                objectName: "cardName"
                text: card.name
                width: parent.width
                // The suffix is the informative end, so cut the middle.
                elide: Text.ElideMiddle
                maximumLineCount: 1
                color: card.foreground
            }
            Controls.Label {
                objectName: "cardSubtitle"
                text: card.subtitle
                visible: text.length > 0
                width: parent.width
                elide: Text.ElideRight
                maximumLineCount: 1
                opacity: 0.75
                color: card.foreground
                font.pixelSize: Math.max(10, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.8))
            }
        }
    }
}
