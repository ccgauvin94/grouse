// SPDX-License-Identifier: AGPL-3.0-or-later

import QtQuick
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

/**
 * A file the SERVER shared, as a card in the transcript.
 *
 * The transcript carries a REFERENCE — name, type, size, digest and the
 * server-side path — and never the bytes (measured 2026-11-03: a blob in a tool
 * result is stored in the session, re-sent on every replay and shown to the
 * model, so a 1 MiB PDF would cost ~350k tokens of context per turn). So the card
 * is also the download: tapping it asks the core for the file, the core pulls it
 * in chunks and reports on `Mgr.fileEvent`, and a finished file opens in whatever
 * this desktop uses for its type.
 *
 * A component (not delegate markup) because the states are worth testing on their
 * own: idle → downloading (with progress) → open, or a failure that keeps the
 * retry one tap away.
 *
 * Two things this layout deliberately does NOT do, each of which hung the
 * headless test runner before it was fixed:
 *  - no `Controls.ProgressBar` (the QQC2 control animates its position on every
 *    change, and a running animation never settles under QuickTest), and
 *  - no implicit size derived from a child whose width derives from this card
 *    (`implicitWidth: row.implicitWidth` where a child is `width: card.width - 78`
 *    is a binding cycle; QML breaks it by guessing, and the guess is what made
 *    one test case hang while its neighbours passed).
 */
Rectangle {
    id: card

    property string fileName: ""
    /** Freedesktop icon name, e.g. "application-pdf". */
    property string icon: "application-octet-stream"
    /** "PDF · 106 KB"; the size is left out until the server reports it. */
    property string subtitle: ""
    /**
     * idle | downloading | saved | failed.
     *
     * Named `phase`, NOT `state`: `Item.state` is QML's own state-machine
     * property, and assigning "downloading" to it would ask QML for a State of
     * that name instead of driving this card.
     */
    property string phase: "idle"
    property real received: 0
    property real total: 0
    property string failure: ""

    signal downloadRequested()
    signal openRequested()

    readonly property bool busy: phase === "downloading"
    readonly property bool ready: phase === "saved"
    readonly property real progress: total > 0 ? Math.min(1, received / total) : 0
    /// The bar is shown exactly while busy — a named property so a test can pin
    /// the contract without reaching into the layout.
    readonly property bool showsProgress: card.busy

    /// A constant: whatever the transcript gives it wins, and nothing here
    /// depends on the children measuring themselves.
    implicitWidth: 340
    implicitHeight: labels.implicitHeight + 20
    radius: 8
    color: Qt.rgba(0, 0, 0, 0.06)

    /** What a tap does: fetch the file, or open the copy we already have. */
    function activate() {
        // A second tap mid-fetch would start a second fetch for the same bytes
        // (the core would answer `saved` twice and the card would flap).
        if (card.busy)
            return
        if (card.ready)
            card.openRequested()
        else
            card.downloadRequested()
    }

    MouseArea {
        objectName: "cardTap"
        anchors.fill: parent
        cursorShape: Qt.PointingHandCursor
        onClicked: card.activate()
    }

    Kirigami.Icon {
        objectName: "iconItem"
        source: card.icon
        isMask: true
        color: Kirigami.Theme.textColor
        implicitWidth: 20
        implicitHeight: 20
        anchors.left: parent.left
        anchors.leftMargin: 10
        anchors.top: parent.top
        anchors.topMargin: 10
    }

    Controls.ToolButton {
        objectName: "cardAction"
        anchors.right: parent.right
        anchors.rightMargin: 6
        anchors.verticalCenter: parent.verticalCenter
        implicitWidth: 28
        implicitHeight: 28
        // Idle and failed both download; a finished file opens.
        icon.name: card.ready ? "document-open" : "download-symbolic"
        enabled: !card.busy
        onClicked: card.activate()
    }

    Column {
        id: labels
        anchors.left: parent.left
        anchors.leftMargin: 38
        anchors.right: parent.right
        anchors.rightMargin: 40
        anchors.verticalCenter: parent.verticalCenter
        spacing: 1

        Controls.Label {
            objectName: "cardName"
            text: card.fileName
            width: parent.width
            // The suffix is the informative end, so cut the middle.
            elide: Text.ElideMiddle
            maximumLineCount: 1
        }
        Controls.Label {
            objectName: "cardSubtitle"
            // The subtitle IS the status line once something is happening: a
            // second line that never changes would waste the space the download
            // needs to say how far along it is.
            text: card.phase === "downloading"
                  ? qsTr("downloading %1%").arg(Math.round(card.progress * 100))
                  : card.phase === "saved" ? qsTr("saved · open")
                  : card.phase === "failed" ? (card.failure.length > 0 ? card.failure : qsTr("could not fetch"))
                  : card.subtitle
            visible: text.length > 0
            width: parent.width
            elide: Text.ElideRight
            maximumLineCount: 1
            opacity: 0.75
            color: card.phase === "failed" ? Kirigami.Theme.negativeTextColor
                                           : Kirigami.Theme.textColor
            font.pixelSize: Math.max(10, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.8))
        }
        // A plain track + fill, not a Control: no animation, no implicit size.
        Rectangle {
            objectName: "cardProgress"
            visible: card.showsProgress
            width: parent.width
            height: 3
            radius: 1.5
            color: Qt.rgba(0, 0, 0, 0.12)
            Rectangle {
                objectName: "cardProgressFill"
                height: parent.height
                // A zero-width fill means zero bytes, which the subtitle already
                // says in words; do not paint a sliver for it.
                width: parent.width * card.progress
                radius: parent.radius
                color: Kirigami.Theme.highlightColor
            }
        }
    }
}
