import QtQuick
import QtQuick.Controls
import QtQuick.Controls as Controls
import QtQuick.Layouts
import Grouse 1.0
import org.kde.kirigami as Kirigami

// The per-chat panel, DOCKED rather than overlaid: it takes a column of the chat
// area (a SplitView pane, so the divider is draggable) instead of floating over the
// conversation. That is what lets it stay open while you read and scroll — the
// OverlayDrawer it replaced covered the chat, so it had to be dismissed to read.
//
// Two tabs, because both answer "what is this chat doing": the tools it may use, and
// the work it has handed OFF to a background run (delegate/sub-agent lanes, CONTRACT
// §3.7). An ordinary tool call is a step of the turn and is not a lane, however long it
// runs: its chip in the transcript is where it pulses.
Rectangle {
    id: panel
    /// 0 = tools, 1 = activity. Mirrored on the window so the toggle can flip it.
    property int tab: 0

    color: Kirigami.Theme.alternateBackgroundColor

    // Shown is the moment to ask: the tool list is per SESSION (it changes when you
    // switch chats) and the activity lanes change with every tool row, so both are
    // re-read on show and kept current by the core's item stream after that.
    onVisibleChanged: {
        if (!visible)
            return
        Mgr.refreshToolGroups()
        Mgr.refreshActivity()
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: Kirigami.Units.smallSpacing
        spacing: Kirigami.Units.smallSpacing

        RowLayout {
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing
            Controls.TabBar {
                id: tabs
                Layout.fillWidth: true
                currentIndex: panel.tab
                onCurrentIndexChanged: panel.tab = currentIndex
                Controls.TabButton {
                    text: Mgr.tools.length > 0
                          ? qsTr("Tools (%1)").arg(Mgr.tools.length)
                          : qsTr("Tools")
                }
                // The count is the point of the tab: a glance says whether this chat
                // has sub-agents/live work without opening it.
                Controls.TabButton {
                    text: panel.workCount > 0
                          ? qsTr("Activity (%1)").arg(panel.workCount)
                          : qsTr("Activity")
                }
            }
            Controls.Button {
                flat: true
                display: Controls.AbstractButton.IconOnly
                icon.name: "dialog-close"
                ToolTip.visible: hovered
                ToolTip.text: qsTr("Hide this panel")
                onClicked: panel.closeRequested()
            }
        }

        Controls.Label {
            Layout.fillWidth: true
            visible: panel.tab === 1 && panel.workCount === 0
            wrapMode: Text.Wrap
            text: qsTr("No background work. A delegate (a sub-agent run) shows up here while it works, and stays once it has finished.")
            color: Kirigami.Theme.disabledTextColor
            font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
        }

        // ---- Activity: the chat's work lanes ------------------------------
        ListView {
            id: workList
            objectName: "workList"
            visible: panel.tab === 1
            Layout.fillWidth: true
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

                // What it is doing RIGHT NOW: the child's own tool calls arrive on this
                // row while it works, so the last step is its current one — real progress
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

        // ---- Tools: what this chat may use --------------------------------
              ListView {
                  id: toolsList
                  Layout.fillWidth: true
                  Layout.fillHeight: true
                  clip: true
                  spacing: 2
                  model: Mgr.toolGroups
                  ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }

                  // Column root (not Item): the header row and the expanded subtool
                  // column then stack naturally instead of overlapping.
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
                      // The expander is about SUB-TOOLS, not attachment (Manager
                      // computes it): unknown catalogues get a peek arrow (the peek
                      // attaches the extension for one round-trip and detaches it —
                      // seeing the list costs no context), known ones only if they
                      // really have >=2 tools.
                      readonly property bool gExpandable: modelData.expandable

                      RowLayout {
                          width: parent.width
                          spacing: Kirigami.Units.smallSpacing
                          // Fixed-width slot for the expander: hiding the BUTTON
                          // alone lets the Switch drift left on arrowless rows
                          // (an invisible item still claims its column). The Item
                          // keeps every switch at the same x.
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
                          // The count shows on every KNOWN row — expandable or
                          // not (the background sweep makes rows known without a
                          // click); "…" only while a row is still unknown.
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
                                      // Ticking a tool on a detached row ATTACHES the
                                      // extension restricted to that tool (see
                                      // Manager::setSessionToolEnabled) — browsing the
                                      // list and enabling one must not require enabling all.
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
    }

    /// The number of lanes RUNNING NOW, for the tab's badge. Live only: a count that never
    /// falls back to zero (it kept reading "3" after the turn was cancelled) is a claim
    /// about the present that is not true. The tab still lists finished runs.
    readonly property int workCount: {
        var lanes = Mgr.activity
        var n = 0
        for (var i = 0; i < lanes.length; i++) if (lanes[i].running === true) n++
        return n
    }
    signal closeRequested()
}
