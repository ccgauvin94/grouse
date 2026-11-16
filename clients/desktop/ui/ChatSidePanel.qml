import QtQuick
import QtQuick.Controls
import QtQuick.Controls as Controls
import QtQuick.Layouts
import Grouse 1.0
import org.kde.kirigami as Kirigami

// The per-chat panel, DOCKED inside the chat frame rather than overlaid: it takes a column
// of that frame (a SplitView pane, so the divider is draggable) instead of floating over the
// conversation. That is what lets it stay open while you read and scroll.
//
// ONE panel, three stacked sections — what fills the window is full, what tools this chat
// may use, and what work it has handed OFF to a background run (delegate/sub-agent lanes,
// CONTRACT §3.7). No tabs: the three questions are asked together ("why is this chat using
// that much context, and what is it doing?"), and a tab bar made two of them cost a click.
// Each section collapses independently, and a section with nothing to show contributes only
// its header (or disappears), so the space goes to the sections that have content.
Rectangle {
    id: panel

    color: Kirigami.Theme.alternateBackgroundColor

    // Section expansion. Mirrored on the window only through focus(), so a caller can ask
    // for a section without owning the panel's whole state.
    property bool contextExpanded: true
    property bool toolsExpanded: true
    property bool activityExpanded: true

    /// A session with no context window yet (a fresh chat, or a server that never pushed
    /// usage) has nothing to say about context, so the section stays out of the way.
    readonly property bool hasContext: Mgr.contextSize > 0 || Mgr.compacting
    /// Lanes the panel can LIST (running or finished), and how many are live right now —
    /// the second is the badge, because a count that never falls back to zero was a claim
    /// about the present that stopped being true.
    readonly property int laneCount: Mgr.activity.length
    readonly property int runningCount: {
        var lanes = Mgr.activity
        var n = 0
        for (var i = 0; i < lanes.length; i++) if (lanes[i].running === true) n++
        return n
    }

    signal closeRequested()
    /// Ask the window to confirm and then send /clear (goose's own "clear the conversation
    /// history"); the panel does not own a destructive action's dialog.
    signal clearRequested()

    /// Expand one section. A function, not a property: asking for a section that is
    /// already open must still work, and a property write of the same value fires nothing.
    /// NOT named `focus`: `Item.focus` is a FINAL property, so a function of that name is
    /// discarded with "final member focus is overridden" and every caller hits a bool.
    function focusSection(section) {
        if (section === 0) panel.contextExpanded = true
        else if (section === 1) panel.toolsExpanded = true
        else if (section === 2) panel.activityExpanded = true
    }

    function percent() {
        if (Mgr.contextSize <= 0) return 0
        const p = Mgr.contextUsed * 100 / Mgr.contextSize
        // One decimal below 10% so a nearly-empty window still reads as progress.
        return p < 10 ? Math.round(p * 10) / 10 : Math.round(p)
    }

    // Shown is the moment to ask: the tool list is per SESSION (it changes when you switch
    // chats), the lanes change with every tool row, and the breakdown is measured server
    // side — none of them are worth asking for while the panel is hidden.
    /// Re-read the three sections. Called on show and on a chat switch: `visible` does not
    /// change when the session does, so onVisibleChanged alone left a panel that stayed open
    /// across chats describing the PREVIOUS one.
    function refreshAll() {
        Mgr.refreshToolGroups()
        Mgr.refreshActivity()
        if (panel.hasContext)
            Mgr.requestContextReport()
    }

    onVisibleChanged: {
        if (visible)
            panel.refreshAll()
    }

    Connections {
        target: Mgr
        function onCurrentSessionChanged() {
            if (panel.visible)
                panel.refreshAll()
        }
    }

    // A collapsed section is cheap to re-read and must never show another chat's rows.
    onToolsExpandedChanged: if (panel.toolsExpanded && panel.visible) Mgr.refreshToolGroups()
    onActivityExpandedChanged: if (panel.activityExpanded && panel.visible) Mgr.refreshActivity()
    onContextExpandedChanged: if (panel.contextExpanded && panel.visible) Mgr.requestContextReport()

    // A section's title row: chevron, name, and whatever number the caller wants beside it.
    component SectionBar: RowLayout {
        id: bar
        property string title: ""
        property string badge: ""
        property bool expanded: true
        signal toggleRequested()
        Layout.fillWidth: true
        spacing: Kirigami.Units.smallSpacing

        Controls.Button {
            flat: true
            display: Controls.AbstractButton.IconOnly
            icon.name: bar.expanded ? "arrow-down" : "arrow-right"
            implicitWidth: Kirigami.Units.gridUnit * 1.25
            implicitHeight: Kirigami.Units.gridUnit * 1.25
            onClicked: bar.toggleRequested()
            ToolTip.visible: hovered
            ToolTip.text: bar.expanded ? qsTr("Collapse") : qsTr("Expand")
        }
        Controls.Label {
            Layout.fillWidth: true
            text: bar.title
            font.bold: true
            elide: Text.ElideRight
        }
        Controls.Label {
            text: bar.badge
            visible: text.length > 0
            opacity: 0.6
            elide: Text.ElideRight
            Layout.maximumWidth: bar.width * 0.55
            font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
        }
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: Kirigami.Units.smallSpacing
        spacing: Kirigami.Units.smallSpacing

        RowLayout {
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing
            Controls.Label {
                Layout.fillWidth: true
                text: qsTr("This chat")
                opacity: 0.7
                elide: Text.ElideRight
                font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
            }
            Controls.Button {
                flat: true
                display: Controls.AbstractButton.IconOnly
                icon.name: "dialog-close"
                onClicked: panel.closeRequested()
                ToolTip.visible: hovered
                ToolTip.text: qsTr("Hide this panel")
            }
        }

        // ---- Context: what fills the window, and the two ways to make room ----
        SectionBar {
            visible: panel.hasContext
            title: qsTr("Context")
            badge: {
                if (Mgr.compacting) return qsTr("compacting…")
                if (Mgr.contextSize <= 0) return ""
                return breakdown.fmtK(Mgr.contextUsed) + " / " + breakdown.fmtK(Mgr.contextSize)
                        + " · " + panel.percent() + "%"
            }
            expanded: panel.contextExpanded
            onToggleRequested: panel.contextExpanded = !panel.contextExpanded
        }

        ColumnLayout {
            visible: panel.hasContext && panel.contextExpanded
            Layout.fillWidth: true
            // A nominal preferred height, and NOT its implicit one: a scrollable breakdown
            // reports the height of everything inside it, and this column handed out space in
            // proportion to preferred heights — so the Context section ate the whole panel and
            // left the Tools list at zero (measured on the first build, 2026-11-15). Equal
            // nominal heights give the expanded sections equal shares, and a collapsed one
            // still hands its share to the others because it is not in the layout at all.
            Layout.preferredHeight: Kirigami.Units.gridUnit * 12
            Layout.fillHeight: true
            spacing: Kirigami.Units.smallSpacing

            RowLayout {
                Layout.fillWidth: true
                spacing: Kirigami.Units.smallSpacing
                ContextRing {
                    Layout.preferredWidth: Kirigami.Units.iconSizes.small
                    Layout.preferredHeight: Kirigami.Units.iconSizes.small
                    percent: panel.percent()
                    thickness: 4
                }
                Controls.Label {
                    Layout.fillWidth: true
                    elide: Text.ElideRight
                    text: Mgr.compacting
                          ? (Mgr.compactionStatus ? Mgr.compactionStatus : qsTr("Compacting…"))
                          : breakdown.fmtK(Mgr.contextUsed) + " / " + breakdown.fmtK(Mgr.contextSize)
                            + " · " + panel.percent() + "%"
                    color: panel.percent() >= 90 ? Kirigami.Theme.negativeTextColor
                                                 : Kirigami.Theme.textColor
                }
                Controls.Button {
                    flat: true
                    display: Controls.AbstractButton.IconOnly
                    icon.name: "view-refresh"
                    onClicked: Mgr.requestContextReport()
                    ToolTip.visible: hovered
                    ToolTip.text: qsTr("Measure this chat again (the server tokenizes the next request)")
                }
            }

            RowLayout {
                Layout.fillWidth: true
                spacing: Kirigami.Units.smallSpacing
                Controls.Button {
                    text: qsTr("Compact")
                    icon.name: "archive-insert-directory"
                    enabled: Mgr.online && !Mgr.compacting && !Mgr.prompting && Mgr.contextSize > 0
                    onClicked: Mgr.compactConversation()
                    ToolTip.visible: hovered
                    ToolTip.text: qsTr("Summarise the conversation to free most of the window")
                }
                // A turn on the wire owns the conversation: clearing or compacting
                // underneath it is how two writers race for one history.
                Controls.Button {
                    text: qsTr("Clear")
                    icon.name: "edit-clear"
                    enabled: Mgr.online && !Mgr.compacting && !Mgr.prompting
                    onClicked: panel.clearRequested()
                    ToolTip.visible: hovered
                    ToolTip.text: qsTr("Start this chat over with no history")
                }
                Item { Layout.fillWidth: true }
            }

            Controls.ScrollView {
                id: contextScroll
                Layout.fillWidth: true
                Layout.fillHeight: true
                clip: true
                contentWidth: availableWidth
                ScrollBar.horizontal.policy: ScrollBar.AlwaysOff

                ContextBreakdown {
                    id: breakdown
                    width: contextScroll.availableWidth
                }
            }
        }

        Kirigami.Separator { Layout.fillWidth: true }

        // ---- Tools: what this chat may use --------------------------------
        SectionBar {
            title: Mgr.tools.length > 0 ? qsTr("Tools (%1)").arg(Mgr.tools.length) : qsTr("Tools")
            expanded: panel.toolsExpanded
            onToggleRequested: panel.toolsExpanded = !panel.toolsExpanded
        }

        ListView {
            id: toolsList
            visible: panel.toolsExpanded
            Layout.fillWidth: true
            Layout.preferredHeight: Kirigami.Units.gridUnit * 12
            Layout.fillHeight: true
            clip: true
            spacing: 2
            model: Mgr.toolGroups
            ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }

            // Column root (not Item): the header row and the expanded subtool column then
            // stack naturally instead of overlapping.
            delegate: Column {
                id: tdel
                width: ListView.view.width - Kirigami.Units.gridUnit
                spacing: 2
                property bool expanded: false
                readonly property string gName: modelData.name
                // Session toggles/catalog discovery are keyed by extensionKey, not the
                // display name (see Manager::ExtDef.key).
                readonly property string gKey: modelData.key
                readonly property bool gAttrib: modelData.attrib
                readonly property bool gEnabled: modelData.enabled
                readonly property bool gKnown: modelData.known
                // The expander is about SUB-TOOLS, not attachment (Manager computes it):
                // unknown catalogues get a peek arrow (the peek attaches the extension for
                // one round-trip and detaches it — seeing the list costs no context), known
                // ones only if they really have >=2 tools.
                readonly property bool gExpandable: modelData.expandable

                RowLayout {
                    width: parent.width
                    spacing: Kirigami.Units.smallSpacing
                    // Fixed-width slot for the expander: hiding the BUTTON alone lets the
                    // Switch drift left on arrowless rows (an invisible item still claims
                    // its column). The Item keeps every switch at the same x.
                    Item {
                        Layout.preferredWidth: Kirigami.Units.gridUnit
                        implicitWidth: Kirigami.Units.gridUnit
                        Layout.preferredHeight: Kirigami.Units.gridUnit * 1.25
                        Controls.Button {
                            anchors.centerIn: parent
                            flat: true
                            display: Controls.AbstractButton.IconOnly
                            icon.name: tdel.expanded ? "arrow-down" : "arrow-right"
                            visible: tdel.gExpandable
                            onClicked: {
                                tdel.expanded = !tdel.expanded
                                if (tdel.expanded && !tdel.gKnown)
                                    Mgr.discoverToolGroup(tdel.gKey)
                            }
                        }
                    }
                    Controls.Switch {
                        Layout.preferredWidth: Kirigami.Units.gridUnit * 2.5
                        checked: tdel.gEnabled
                        onToggled: Mgr.setSessionExtensionEnabled(tdel.gKey, checked)
                    }
                    Controls.Label {
                        text: tdel.gName
                        textFormat: Text.PlainText
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                        font.bold: true
                    }
                    Controls.Label {
                        // The count shows on every KNOWN row — expandable or not (the
                        // background sweep makes rows known without a click); "…" only
                        // while a row is still unknown.
                        text: tdel.gKnown
                              ? (tdel.expanded ? "" : modelData.tools.length + qsTr(" tools"))
                              : (tdel.gExpandable ? qsTr("…") : "")
                        opacity: 0.6
                        horizontalAlignment: Text.AlignRight
                    }
                }

                Column {
                    id: toolsCol
                    width: parent.width
                    visible: tdel.expanded
                    Repeater {
                        model: tdel.gExpandable ? modelData.tools : []
                        delegate: RowLayout {
                            width: parent.width
                            spacing: Kirigami.Units.smallSpacing
                            Controls.Switch {
                                Layout.preferredWidth: Kirigami.Units.gridUnit * 2.5
                                checked: modelData.on
                                // Ticking a tool on a detached row ATTACHES the extension
                                // restricted to that tool (see Manager::setSessionToolEnabled)
                                // — browsing the list and enabling one must not require
                                // enabling all.
                                onToggled: Mgr.setSessionToolEnabled(tdel.gKey, modelData.name, checked)
                            }
                            Controls.Label {
                                text: modelData.name
                                textFormat: Text.PlainText
                                elide: Text.ElideRight
                                Layout.fillWidth: true
                            }
                        }
                    }
                }
            }
        }

        Kirigami.Separator { Layout.fillWidth: true }

        // ---- Activity: the chat's work lanes ------------------------------
        SectionBar {
            title: qsTr("Activity")
            badge: panel.runningCount > 0 ? String(panel.runningCount) + qsTr(" running") : ""
            expanded: panel.activityExpanded
            onToggleRequested: panel.activityExpanded = !panel.activityExpanded
        }

        Controls.Label {
            Layout.fillWidth: true
            visible: panel.activityExpanded && panel.laneCount === 0
            wrapMode: Text.Wrap
            text: qsTr("No background work. A delegate (a sub-agent run) shows up here while it works, and stays once it has finished.")
            color: Kirigami.Theme.disabledTextColor
            font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
        }

        ListView {
            id: workList
            objectName: "workList"
            // Only a chat that HAS lanes gets the space: an empty list claiming a third of
            // the panel is the "dynamic" part failing.
            visible: panel.activityExpanded && panel.laneCount > 0
            Layout.fillWidth: true
            Layout.preferredHeight: Kirigami.Units.gridUnit * 12
            Layout.fillHeight: true
            clip: true
            spacing: Kirigami.Units.smallSpacing
            model: Mgr.activity
            ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }

            delegate: Column {
                id: wdel
                width: ListView.view.width - Kirigami.Units.gridUnit
                spacing: 2
                property bool expanded: true

                // The lane's identity is the tool call it belongs to (CONTRACT §3.7).
                readonly property string wId: modelData.id
                readonly property string wTitle: modelData.title !== "" ? modelData.title
                                                                        : qsTr("tool call")
                // `outcome` is the CORE's answer, not the tool call's status: a delegate's
                // call returns its ack a second after it is issued, so `status` reads
                // "completed" while the subagent still works. Reading `status` here is what
                // made this panel say "done" the instant an agent was fired (2026-11-13).
                readonly property bool wRunning: modelData.running === true
                readonly property bool wCancelled: modelData.outcome === "cancelled"
                readonly property bool wUnfinished: modelData.outcome === "unfinished"
                readonly property bool wFailed: modelData.outcome === "failed"
                readonly property var wSteps: modelData.steps === undefined ? [] : modelData.steps
                readonly property bool wAgent: modelData.subagent === true

                Controls.Button {
                    width: parent.width
                    flat: true
                    padding: Kirigami.Units.smallSpacing
                    onClicked: wdel.expanded = !wdel.expanded
                    contentItem: RowLayout {
                        spacing: Kirigami.Units.smallSpacing
                        Kirigami.Icon {
                            // A sub-agent is its own lane; a delegate with no child step
                            // yet is the same background work, before it reports.
                            source: wdel.wAgent ? "system-users" : "view-refresh"
                            implicitWidth: Kirigami.Units.iconSizes.small
                            implicitHeight: Kirigami.Units.iconSizes.small
                            color: wdel.wFailed ? Kirigami.Theme.negativeTextColor
                                  : wdel.wRunning ? Kirigami.Theme.neutralTextColor
                                  : wdel.wCancelled ? Kirigami.Theme.disabledTextColor
                                  : Kirigami.Theme.positiveTextColor
                        }
                        Controls.Label {
                            Layout.fillWidth: true
                            text: wdel.wTitle
                            textFormat: Text.PlainText
                            elide: Text.ElideRight
                            font.bold: wdel.wAgent
                        }
                        Controls.Label {
                            text: wdel.wRunning ? qsTr("running")
                                  : wdel.wCancelled ? qsTr("cancelled")
                                  : wdel.wFailed ? qsTr("failed")
                                  : wdel.wUnfinished ? qsTr("unfinished")
                                  : qsTr("done")
                            color: Kirigami.Theme.disabledTextColor
                            font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.8
                        }
                    }
                }

                Controls.Label {
                    width: parent.width
                    visible: wdel.expanded && modelData.detail !== ""
                    text: modelData.detail
                    textFormat: Text.PlainText
                    wrapMode: Text.Wrap
                    elide: Text.ElideRight
                    maximumLineCount: 3
                    color: Kirigami.Theme.disabledTextColor
                    font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.82
                }

                // What it is doing RIGHT NOW: the child's own tool calls arrive on this row
                // while it works, so the last step is its current one — real progress
                // rather than a spinner.
                Controls.Label {
                    width: parent.width
                    visible: wdel.expanded && wdel.wRunning
                    text: {
                        var steps = wdel.wSteps
                        if (steps.length === 0) return qsTr("working…")
                        var now = steps[steps.length - 1]
                        return qsTr("now · %1 · %2").arg(now.name).arg(now.detail)
                    }
                    textFormat: Text.PlainText
                    elide: Text.ElideRight
                    color: Kirigami.Theme.neutralTextColor
                    font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.8
                    leftPadding: Kirigami.Units.gridUnit * 0.6
                }

                // The sub-agent's own tool calls: what the delegate ACTUALLY did, which
                // neither the parent's row nor its summary says.
                Repeater {
                    model: wdel.expanded ? wdel.wSteps : []
                    delegate: RowLayout {
                        width: parent.width
                        spacing: Kirigami.Units.smallSpacing
                        Item { implicitWidth: Kirigami.Units.gridUnit * 0.6 }
                        Controls.Label {
                            text: modelData.name
                            textFormat: Text.PlainText
                            font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.8
                            font.family: "monospace"
                            color: Kirigami.Theme.disabledTextColor
                        }
                        Controls.Label {
                            Layout.fillWidth: true
                            text: modelData.detail
                            textFormat: Text.PlainText
                            elide: Text.ElideRight
                            font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.8
                            color: Kirigami.Theme.disabledTextColor
                        }
                    }
                }
            }
        }

    }
}
