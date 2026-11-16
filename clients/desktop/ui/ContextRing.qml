import QtQuick
import org.kde.kirigami as Kirigami

// How full the context window is, as a donut — the same shape and the same three
// thresholds the phone draws (comfortable / nearing compaction / at it). A ring rather
// than a bar because it sits in the composer strip at 16px, where a bar reads as progress
// for the SEND rather than for the chat.
//
// Pure paint on purpose: no input handling. The surfaces that show it (the composer chip,
// the panel's Context header) put their own tap target around it, so one gesture cannot
// end up answered twice.
Item {
    id: ring

    /// 0..100. Values past 100 are clamped: a provider can bill over the declared window,
    /// and an arc that wraps past the start would read as an empty window.
    property real percent: 0
    property int thickness: 3

    readonly property real fraction: Math.max(0, Math.min(1, percent / 100))
    readonly property color fillColor: percent >= 90 ? Kirigami.Theme.negativeTextColor
                                    : percent >= 60 ? Kirigami.Theme.neutralTextColor
                                    : Kirigami.Theme.positiveTextColor
    // A tint of the text colour, not `view`: the app's palette is custom, so a themed
    // track colour goes white-on-white in one of the two schemes.
    readonly property color trackColor: Qt.rgba(Kirigami.Theme.textColor.r,
                                                Kirigami.Theme.textColor.g,
                                                Kirigami.Theme.textColor.b, 0.18)

    implicitWidth: Kirigami.Units.iconSizes.small
    implicitHeight: Kirigami.Units.iconSizes.small

    Canvas {
        id: canvas
        anchors.fill: parent

        onPaint: {
            const ctx = getContext("2d")
            // Canvas keeps its previous frame; a shorter arc must not leave the old tail.
            ctx.clearRect(0, 0, width, height)
            const cx = width / 2
            const cy = height / 2
            const radius = Math.max(1, Math.min(width, height) / 2 - ring.thickness / 2)
            ctx.lineWidth = ring.thickness
            ctx.lineCap = "round"
            ctx.beginPath()
            ctx.strokeStyle = ring.trackColor
            ctx.arc(cx, cy, radius, 0, Math.PI * 2)
            ctx.stroke()
            if (ring.fraction > 0) {
                // From 12 o'clock, clockwise: the reading everyone expects from a gauge.
                ctx.beginPath()
                ctx.strokeStyle = ring.fillColor
                ctx.arc(cx, cy, radius, -Math.PI / 2, -Math.PI / 2 + Math.PI * 2 * ring.fraction)
                ctx.stroke()
            }
        }

        onWidthChanged: requestPaint()
        onHeightChanged: requestPaint()
    }

    onFractionChanged: canvas.requestPaint()
    onFillColorChanged: canvas.requestPaint()
    onTrackColorChanged: canvas.requestPaint()
    onVisibleChanged: if (visible) canvas.requestPaint()
    // The first paint can land before the item has been laid out, and a Canvas that was
    // hidden while its value changed has no frame to show when it comes back.
    Component.onCompleted: canvas.requestPaint()

    // No light/dark hook is needed: both colours above are BINDINGS on the palette, so a
    // scheme switch changes them and the on*ColorChanged handlers repaint. (A Connections
    // on `Kirigami.Theme.colorSchemeChanged` would never fire — that signal does not exist
    // on the theme singleton, and this app drives a custom palette anyway.)
}
