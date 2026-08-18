import QtQuick
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

// Renders an autovisualiser chart spec (Chart.js-style JSON) with a native Canvas —
// the desktop stays lightweight by drawing the common types (bar/line/pie/doughnut/
// scatter) directly instead of embedding a browser engine for the Android client's
// WebView bubble. Unsupported or malformed specs fall back to a plain data card.
Item {
    id: root

    property string chartSpec: ""
    property string title: ""

    readonly property real pad: Kirigami.Units.smallSpacing
    readonly property real legendH: 24

    // The host delegate computes the row height (chartBubbleH()) from the spec; this
    // component just paints whatever space it is given.
    function parseSpec() {
        if (root.chartSpec.length === 0) return null
        try {
            const s = JSON.parse(root.chartSpec)
            if (s && s.type && s.data) return s
        } catch (e) { }
        return null
    }

    function paletteColor(i) {
        // A fixed 8-color palette (theme-agnostic, readable on both schemes).
        const pal = ["#e08a00", "#3d9ae8", "#4e9a06", "#c01c28",
                     "#75507b", "#f2a900", "#0b8e6d", "#7b8ea8"]
        return pal[i % pal.length]
    }

    // Sort-of "nice" axis maximum so bars/lines don't get clipped.
    function niceMax(v) {
        if (v <= 0) return 1
        const mag = Math.pow(10, Math.floor(Math.log(v) / Math.LN10))
        const norm = v / mag
        const nice = norm <= 1 ? 1 : norm <= 2 ? 2 : norm <= 5 ? 5 : 10
        return nice * mag
    }

    Canvas {
        id: canvas
        anchors.fill: parent
        onPaint: root.draw(canvas.getContext("2d"), canvas.width, canvas.height)
        onWidthChanged: requestPaint()
        onHeightChanged: requestPaint()
        Connections {
            target: root
            function onChartSpecChanged() { canvas.requestPaint() }
        }
    }

    function draw(ctx, w, h) {
        const s = root.parseSpec()
        if (!s) {
            ctx.fillStyle = Kirigami.Theme.textColor
            ctx.font = "12px sans-serif"
            ctx.fillText("Chart data unavailable", root.pad, 20)
            return
        }
        const t = s.type
        if (t === "pie" || t === "doughnut") root.drawPie(ctx, w, h, s)
        else if (t === "scatter") root.drawScatter(ctx, w, h, s)
        else root.drawCartesian(ctx, w, h, s)   // bar / line / radar-ish fallback
    }

    function chartTitle(s) {
        if (s.options && s.options.title && s.options.title.text)
            return String(s.options.title.text)
        return root.title
    }

    function datasets(s) {
        return s.data.datasets || []
    }

    function labels(s) {
        return s.data.labels || []
    }

    function num(v) { return Number(v) }

    function drawTitle(ctx, w) {
        const t = root.chartTitle(root.parseSpec())
        if (!t) return
        ctx.fillStyle = Kirigami.Theme.textColor
        ctx.font = "bold 13px sans-serif"
        ctx.textAlign = "center"
        ctx.fillText(t, w / 2, 18)
        ctx.textAlign = "start"
    }

    function legend(ctx, w, h, items) {
        if (items.length === 0) return
        let x = root.pad, y = h - root.legendH
        ctx.font = "11px sans-serif"
        for (let i = 0; i < items.length; i++) {
            const label = items[i].label || ("Series " + (i + 1))
            const txt = String(label)
            ctx.fillStyle = items[i].color
            ctx.fillRect(x, y + 5, 10, 10)
            ctx.fillStyle = Kirigami.Theme.textColor
            const tw = ctx.measureText(txt).width
            ctx.fillText(txt, x + 14, y + 14)
            x += 14 + tw + root.pad * 3
            if (x > w - 60) break
        }
    }

    function drawCartesian(ctx, w, h, s) {
        root.drawTitle(ctx, w)
        const ds = root.datasets(s)
        const ls = root.labels(s)
        if (ds.length === 0) { ctx.fillText("no data", root.pad, 30); return }

        // Collect all numeric values to size the Y axis.
        let maxVal = 0
        for (let d = 0; d < ds.length; d++) {
            const vals = ds[d].data || []
            for (let i = 0; i < vals.length; i++) {
                const v = root.num(vals[i])
                if (v > maxVal) maxVal = v
            }
        }
        const top = 34
        const left = root.pad * 2 + 30
        const bottom = h - root.legendH - 24
        const right = w - root.pad * 2
        const yMax = root.niceMax(maxVal)

        // Gridlines + Y labels
        ctx.strokeStyle = Kirigami.Theme.disabledTextColor
        ctx.lineWidth = 1
        ctx.fillStyle = Kirigami.Theme.textColor
        ctx.font = "10px sans-serif"
        ctx.textAlign = "right"
        for (let g = 0; g <= 4; g++) {
            const val = yMax * g / 4
            const y = bottom - (bottom - top) * g / 4
            ctx.globalAlpha = 0.25
            ctx.beginPath(); ctx.moveTo(left, y); ctx.lineTo(right, y); ctx.stroke()
            ctx.globalAlpha = 1
            ctx.fillText(String(Math.round(val)), left - 4, y + 4)
        }
        ctx.textAlign = "start"

        const n = ls.length > 0 ? ls.length : (ds[0].data ? ds[0].data.length : 0)
        const isLine = s.type === "line"
        const slotW = n > 0 ? (right - left) / n : 1

        if (s.type === "bar") {
            const groupW = slotW * 0.6
            const barW = groupW / ds.length
            for (let d = 0; d < ds.length; d++) {
                const vals = ds[d].data || []
                ctx.fillStyle = root.paletteColor(d)
                for (let i = 0; i < vals.length; i++) {
                    const v = root.num(vals[i])
                    const x = left + i * slotW + (slotW - groupW) / 2 + d * barW
                    const y = bottom - (v / yMax) * (bottom - top)
                    ctx.fillRect(x, y, Math.max(1, barW - 2), bottom - y)
                }
            }
        } else {
            // line (or unknown -> draw as line)
            for (let d = 0; d < ds.length; d++) {
                const vals = ds[d].data || []
                ctx.strokeStyle = root.paletteColor(d)
                ctx.fillStyle = root.paletteColor(d)
                ctx.lineWidth = 2
                ctx.beginPath()
                let started = false
                for (let i = 0; i < vals.length; i++) {
                    const v = root.num(vals[i])
                    const x = n > 1 ? left + i * slotW + slotW / 2 : left + (right - left) / 2
                    const y = bottom - (v / yMax) * (bottom - top)
                    if (!started) { ctx.moveTo(x, y); started = true }
                    else ctx.lineTo(x, y)
                }
                ctx.stroke()
                // point markers
                for (let i = 0; i < vals.length; i++) {
                    const v = root.num(vals[i])
                    const x = n > 1 ? left + i * slotW + slotW / 2 : left + (right - left) / 2
                    const y = bottom - (v / yMax) * (bottom - top)
                    ctx.beginPath(); ctx.arc(x, y, 3, 0, 2 * Math.PI); ctx.fill()
                }
            }
        }

        // X labels (a few, evenly spaced)
        ctx.fillStyle = Kirigami.Theme.textColor
        ctx.font = "10px sans-serif"
        ctx.textAlign = "center"
        const labelStep = Math.max(1, Math.ceil(n / 6))
        for (let i = 0; i < n; i += labelStep) {
            const x = left + i * slotW + slotW / 2
            const txt = String(ls.length > 0 ? ls[i] : i)
            ctx.fillText(txt.length > 14 ? txt.substring(0, 13) + "…" : txt, x, bottom + 14)
        }
        ctx.textAlign = "start"

        root.legend(ctx, w, h, ds.map((d, i) => ({ label: d.label, color: root.paletteColor(i) })))
    }

    function drawPie(ctx, w, h, s) {
        root.drawTitle(ctx, w)
        const ds = root.datasets(s)
        if (ds.length === 0) return
        const vals = ds[0].data || []
        const ls = root.labels(s)
        const total = vals.reduce((a, v) => a + root.num(v), 0)
        if (total <= 0) return
        const cx = w / 2 - 40
        const cy = (h + 30) / 2
        const radius = Math.min(w, h) / 2 - 50
        const inner = s.type === "doughnut" ? radius * 0.55 : 0
        let angle = -Math.PI / 2
        ctx.lineWidth = 1
        ctx.strokeStyle = Kirigami.Theme.alternateBackgroundColor
        const items = []
        for (let i = 0; i < vals.length; i++) {
            const frac = root.num(vals[i]) / total
            const end = angle + frac * 2 * Math.PI
            ctx.beginPath()
            ctx.moveTo(cx, cy)
            ctx.arc(cx, cy, radius, angle, end)
            ctx.closePath()
            ctx.fillStyle = root.paletteColor(i)
            ctx.fill()
            ctx.stroke()
            if (inner > 0) {
                // Punch the doughnut hole with the bubble background so it reads as a ring.
                ctx.fillStyle = Kirigami.Theme.alternateBackgroundColor
                ctx.beginPath()
                ctx.arc(cx, cy, inner, 0, 2 * Math.PI)
                ctx.fill()
            }
            items.push({ label: ls.length > i ? ls[i] : ("Item " + (i + 1)), color: root.paletteColor(i) })
            angle = end
        }
        // Percentage in the middle for doughnut.
        if (inner > 0 && items.length > 0) {
            ctx.fillStyle = Kirigami.Theme.textColor
            ctx.font = "bold 12px sans-serif"
            ctx.textAlign = "center"
            ctx.fillText("100%", cx, cy + 4)
            ctx.textAlign = "start"
        }
        root.legend(ctx, w, h, items)
    }

    function drawScatter(ctx, w, h, s) {
        root.drawTitle(ctx, w)
        const ds = root.datasets(s)
        if (ds.length === 0) return
        let minX = 0, maxX = 1, minY = 0, maxY = 1
        const points = []
        for (let d = 0; d < ds.length; d++) {
            const vals = ds[d].data || []
            for (let i = 0; i < vals.length; i++) {
                const p = vals[i]
                const x = root.num(p.x)
                const y = root.num(p.y)
                points.push({ x, y, d })
                if (x < minX) minX = x
                if (x > maxX) maxX = x
                if (y < minY) minY = y
                if (y > maxY) maxY = y
            }
        }
        const top = 34, left = root.pad * 2 + 30
        const bottom = h - root.legendH - 24, right = w - root.pad * 2
        const spanX = (maxX - minX) || 1
        const spanY = (maxY - minY) || 1
        ctx.strokeStyle = Kirigami.Theme.disabledTextColor
        ctx.lineWidth = 1
        ctx.beginPath(); ctx.moveTo(left, bottom); ctx.lineTo(right, bottom); ctx.stroke()
        ctx.beginPath(); ctx.moveTo(left, top); ctx.lineTo(left, bottom); ctx.stroke()
        for (let i = 0; i < points.length; i++) {
            const px = left + (points[i].x - minX) / spanX * (right - left)
            const py = bottom - (points[i].y - minY) / spanY * (bottom - top)
            ctx.fillStyle = root.paletteColor(points[i].d)
            ctx.beginPath(); ctx.arc(px, py, 4, 0, 2 * Math.PI); ctx.fill()
        }
        root.legend(ctx, w, h, ds.map((d, i) => ({ label: d.label, color: root.paletteColor(i) })))
    }
}
