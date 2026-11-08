// SPDX-License-Identifier: AGPL-3.0-or-later

import QtQuick
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

/**
 * An image the SERVER shared, painted INLINE in the transcript.
 *
 * The same reference-not-bytes rule applies as for any shared file (see
 * FileCard.qml): the row carries name, type, size, digest and the server path,
 * and the bytes are pulled on demand. What differs is what "on demand" means —
 * an image in a reply is PART of the reply, so it is fetched as soon as its row
 * appears rather than waiting for a tap, and then it is the picture, not a chip
 * describing one.
 *
 * Which files qualify is decided by the VIEW (`ChatPage.inlineImage`), not here:
 * a card that had to guess would disagree with the row that instantiated it. The
 * rule is duplicated in the Android client (`isInlineImage` in Files.kt) because
 * the two clients share no code — change one, change the other.
 *
 * Two layout constraints, inherited from FileCard's hard-won lessons:
 *  - the preview is a FIXED height box, so `implicitHeight` never depends on the
 *    decoded picture's size (an `Image` that sizes itself from the source makes
 *    the delegate's height chase the file, and QuickTest hangs on the churn), and
 *  - no `Controls.ProgressBar` (QQC2 animates its position on every change and a
 *    running animation never settles under QuickTest) — a plain track and fill.
 */
Rectangle {
    id: card

    property string fileName: ""
    /** "PNG · 42 KB"; the size is left out until the server reports it. */
    property string subtitle: ""
    /** Local copy of the bytes, once `phase` is "saved". Empty until then. */
    property string imagePath: ""
    /**
     * idle | downloading | saved | failed.
     *
     * Named `phase`, NOT `state` — `Item.state` is QML's own state machine, and
     * setting it to "downloading" would ask QML for a State of that name.
     */
    property string phase: "idle"
    property real received: 0
    property real total: 0
    property string failure: ""

    signal downloadRequested()
    signal openRequested()

    readonly property bool busy: phase === "downloading"
    readonly property bool ready: phase === "saved"
    readonly property bool showsProgress: card.busy
    /** A saved card WITH a local copy is a picture; anything else is a placeholder. */
    readonly property bool showsImage: card.ready && card.imagePath.length > 0
    /// The inverse, named so a test can pin it: a child's `visible` reads FALSE for
    /// any item under an unshown window (which is every QuickTest case), so it
    /// cannot answer "is the placeholder what is painted".
    readonly property bool showsPlaceholder: !card.showsImage
    readonly property real progress: total > 0 ? Math.min(1, received / total) : 0
    /// The box the picture fits inside. Fixed on purpose (see the header).
    readonly property real previewHeight: 200

    /// The source's own shape. `Image.implicitWidth/Height` are the file's pixel
    /// dimensions once it loads (0 before), and they are what lets the frame hug the
    /// picture instead of letterboxing it inside a fixed box.
    /// The decoded picture's own pixel size, 0 until it loads. Assigned BY the Image
    /// below (`onStatusChanged`), not bound to it: a property binding to an id that the
    /// component has not instantiated yet evaluates once to `undefined` and is never
    /// re-run, so `preview.implicitWidth` here silently stayed 0 and every picture fell
    /// back to a full-width frame. It is also why a test may assign them directly.
    property real naturalW: 0
    property real naturalH: 0
    /// Width the frame takes: the picture's own, capped — NOT the column's. A frame
    /// that filled the width put a narrow picture in the middle of a wide box, which
    /// is the opposite of a reply carrying a picture.
    readonly property real frameW: (card.showsImage && naturalW > 0 && naturalH > 0)
        ? Math.min(card.width - 12, card.previewHeight * naturalW / naturalH)
        : card.width - 12
    readonly property real frameH: (card.showsImage && naturalW > 0 && naturalH > 0)
        ? Math.min(card.previewHeight, card.frameW * naturalH / naturalW)
        : card.previewHeight

    implicitWidth: 340
    implicitHeight: card.frameH + caption.implicitHeight + 14
    radius: 8
    color: Qt.rgba(0, 0, 0, 0.06)

    /** What a tap does: fetch the bytes, or enlarge the picture we already have. */
    function activate() {
        // A second tap mid-fetch would start a second fetch for the same bytes and
        // the two `saved` reports would fight over the preview.
        if (card.busy)
            return
        if (card.ready) {
            card.openRequested()
            card.showLarge()
        } else {
            card.downloadRequested()
        }
    }

    /** The picture at (nearly) full window, over a dim backdrop. */
    function showLarge() {
        if (card.showsImage)
            viewer.open()
    }

    /// Named so a test can pin "a tap enlarges" without needing a shown window.
    readonly property bool viewerOpen: viewer.opened

    Controls.Popup {
        id: viewer
        objectName: "imageViewer"
        modal: true
        dim: true
        padding: 0
        closePolicy: Controls.Popup.CloseOnEscape | Controls.Popup.CloseOnPressOutside
        // Centred on the WINDOW by coordinates, not by reparenting to Overlay.overlay:
        // `parent: Overlay.overlay` plus `anchors.centerIn: parent` is a binding loop Qt
        // warns about (and warns about on every instantiation, once per card).
        width: Math.min(card.Window.width > 0 ? card.Window.width * 0.92 : 900, 1400)
        height: Math.min(card.Window.height > 0 ? card.Window.height * 0.92 : 700, 1000)
        x: (card.Window.width - width) / 2
        y: (card.Window.height - height) / 2
        background: Rectangle { color: "#101010"; radius: 8 }
        contentItem: Image {
            objectName: "viewerImage"
            source: viewer.opened && card.showsImage ? "file://" + card.imagePath : ""
            fillMode: Image.PreserveAspectFit
            asynchronous: true
            // The file is already on disk; QML's URL cache would only pin the decoded
            // bitmap for the session.
            cache: false
        }
        // A press on the picture closes it again: the popup's own close policy covers
        // presses OUTSIDE, and the picture is the whole popup.
        MouseArea {
            anchors.fill: parent
            onClicked: viewer.close()
        }
    }

    Rectangle {
        id: frame
        objectName: "previewFrame"
        anchors.top: parent.top
        anchors.left: parent.left
        anchors.topMargin: 6
        anchors.leftMargin: 6
        width: card.frameW
        height: card.frameH
        radius: 6
        color: Qt.rgba(0, 0, 0, 0.08)
        clip: true

        Image {
            objectName: "cardImage"
            visible: card.showsImage
            anchors.fill: parent
            anchors.margins: 4
            // The frame's size comes from here, so the size has to arrive as a signal:
            // see the note on `naturalW`.
            onStatusChanged: {
                if (status === Image.Ready) {
                    card.naturalW = implicitWidth
                    card.naturalH = implicitHeight
                } else if (status === Image.Error) {
                    card.naturalW = 0
                    card.naturalH = 0
                }
            }
            onSourceChanged: {
                if (source.toString().length === 0) {
                    card.naturalW = 0
                    card.naturalH = 0
                }
            }
            // Fit, never stretch: a squashed chart is worse than a small one.
            fillMode: Image.PreserveAspectFit
            asynchronous: true
            // The file is already on disk under a digest-derived name, so QML's URL
            // cache buys nothing and would pin the decoded bitmap for the session.
            cache: false
            source: card.showsImage ? "file://" + card.imagePath : ""
        }

        Kirigami.Icon {
            objectName: "previewIcon"
            // The placeholder while the bytes are in flight (or unreadable): an
            // empty frame reads as a broken picture, not as a download in progress.
            visible: card.showsPlaceholder
            anchors.centerIn: parent
            source: "image-x-generic"
            isMask: true
            color: Kirigami.Theme.textColor
            implicitWidth: 44
            implicitHeight: 44
            opacity: 0.5
        }

        // A plain track + fill along the bottom of the frame.
        Rectangle {
            objectName: "cardProgress"
            visible: card.showsProgress
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom
            height: 3
            color: Qt.rgba(0, 0, 0, 0.12)
            Rectangle {
                objectName: "cardProgressFill"
                height: parent.height
                width: parent.width * card.progress
                color: Kirigami.Theme.highlightColor
            }
        }

        MouseArea {
            objectName: "cardTap"
            anchors.fill: parent
            cursorShape: Qt.PointingHandCursor
            onClicked: card.activate()
        }
    }

    Column {
        id: caption
        anchors.top: frame.bottom
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.leftMargin: 8
        anchors.rightMargin: 8
        anchors.topMargin: 2
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
            // The subtitle IS the status line once something is happening.
            text: card.phase === "downloading"
                  ? (card.total > 0
                     ? qsTr("downloading %1%").arg(Math.round(card.progress * 100))
                     : qsTr("downloading…"))
                  : card.phase === "saved" ? qsTr("tap to open")
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
    }
}
