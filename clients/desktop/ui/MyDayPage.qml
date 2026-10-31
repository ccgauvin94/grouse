// SPDX-License-Identifier: AGPL-3.0-or-later
import QtQuick
import QtQuick.Controls
import QtQuick.Controls as Controls
import QtQuick.Layouts
import Grouse 1.0
import org.kde.kirigami as Kirigami

/*
 * My Day: the assistant's own working state, as the phone's tab shows it.
 *
 * The lists are assistant-state rows fetched over a deterministic `tools/call`
 * (CONTRACT §3.6) — no model turn, so the page opens in milliseconds. The core
 * owns the call and its hidden auto-mode session; Manager decodes the envelope
 * (myday.cpp) and sends the four intents the server acts on (close / snooze /
 * react / refresh), and this file only decides how a row is painted.
 *
 * Identity of the rows is the server's: an item is a fact that needs attention,
 * a task is an act the assistant handed over. They share one thread LABEL, which
 * is what groups them — and that is why the "Open" list interleaves handed-over
 * work with open loops instead of keeping two separate sections: a label with
 * work in flight and an obligation under it is one area of your life, not two.
 */
Kirigami.Page {
    id: page

    /// The user asked to close `id`; `state` is "done" or "dropped". main.qml owns
    /// the confirmation dialog (dialogs are declared window children there, so the
    /// outcome text and the exact wording live in one place with the others).
    signal closeItemRequested(string id, string title, string state)

    // Which thread groups are open. A JS map rather than a Set: this is QML, and a
    // label is a stable string (normalised `[a-z0-9-]`), so it is a safe key.
    property var expandedLabels: ({})
    function toggleLabel(label) {
        const next = {}
        for (const k in page.expandedLabels) next[k] = page.expandedLabels[k]
        next[label] = page.expandedLabels[label] !== true
        page.expandedLabels = next
    }

    /// The in-play list: handed-over work first, then open loops, both carrying the
    /// thread label that groups them. Snoozed rows are excluded here — they get
    /// their own section, because the tap that quieted one has to be visible.
    function inPlay() {
        const rows = []
        let i
        const handed = Mgr.myDayHanded
        for (i = 0; i < handed.length; i++)
            rows.push({ kind: "task", row: handed[i], label: handed[i].thread || "" })
        const open = Mgr.myDayOpen
        for (i = 0; i < open.length; i++) {
            if (Mgr.rowSnoozed(open[i]))
                continue
            rows.push({ kind: "item", row: open[i], label: open[i].thread || "" })
        }
        return rows
    }

    /// The painted sequence: a header for a thread that holds more than one entry,
    /// then its rows when that thread is open. A single-entry label gets NO header —
    /// nesting one row under a chevron costs a tap to reveal the identical thing,
    /// and at the last count My Day held 21 entries over 19 labels, so a header per
    /// label would be all headers and no rows.
    function entries() {
        const rows = page.inPlay()
        const groups = []
        const index = ({})
        for (let i = 0; i < rows.length; i++) {
            const label = rows[i].label
            if (index[label] === undefined) {
                index[label] = groups.length
                groups.push({ label: label, entries: [] })
            }
            groups[index[label]].entries.push(rows[i])
        }
        const out = []
        for (let g = 0; g < groups.length; g++) {
            const grp = groups[g]
            const collapsible = grp.label.length > 0 && grp.entries.length > 1
            const open = page.expandedLabels[grp.label] === true
            if (collapsible)
                out.push({ type: "header", label: grp.label, count: grp.entries.length, open: open })
            if (!collapsible || open) {
                for (let k = 0; k < grp.entries.length; k++) {
                    out.push({ type: grp.entries[k].kind, row: grp.entries[k].row,
                               label: grp.label })
                }
            }
        }
        return out
    }

    // Bound, not called from the delegate: QML captures the properties a binding
    // reads (here Mgr.myDayOpen / myDayHanded / expandedLabels), so a refresh or a
    // toggle rebuilds the sequence exactly once.
    property var entryModel: page.entries()
    property var inPlayRows: page.inPlay()

    function snoozedRows() {
        const out = []
        const open = Mgr.myDayOpen
        for (let i = 0; i < open.length; i++)
            if (Mgr.rowSnoozed(open[i]))
                out.push(open[i])
        return out
    }
    property var snoozedModel: page.snoozedRows()

    /// Send My Day's one-liner into the assistant thread and clear the box.
    function sendAssistant(field) {
        if (field.text.trim().length === 0)
            return
        Mgr.askAssistant(field.text)
        field.text = ""
    }

    function summaryLine() {
        if (Mgr.tasksLoading)
            return qsTr("reading the assistant's state…")
        const parts = []
        if (page.inPlayRows.length > 0)
            parts.push(qsTr("%1 in play").arg(page.inPlayRows.length))
        if (page.snoozedModel.length > 0)
            parts.push(qsTr("%1 snoozed").arg(page.snoozedModel.length))
        if (Mgr.myDayDone.length > 0)
            parts.push(qsTr("%1 closed lately").arg(Mgr.myDayDone.length))
        return parts.length > 0 ? parts.join(" · ") : qsTr("nothing on the list")
    }

    // ---- assistant card ---------------------------------------------------
    /// The always-on thread's entry point. My Day replaced the drawer's Assistant
    /// button, so the way into the conversation has to be the first thing on it —
    /// with a one-line composer, so writing something down costs no navigation.
    component AssistantCard: Rectangle {
        id: card
        Layout.fillWidth: true
        implicitHeight: cardCol.implicitHeight + Theme.spacing.s4
        radius: Theme.radius.xl
        color: Kirigami.Theme.alternateBackgroundColor
        border.width: 1
        border.color: Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g,
                              Kirigami.Theme.textColor.b, 0.12)

        ColumnLayout {
            id: cardCol
            anchors.fill: parent
            anchors.margins: Theme.spacing.s2
            spacing: Theme.spacing.s2

            RowLayout {
                Layout.fillWidth: true
                spacing: Theme.spacing.s2
                Kirigami.Icon {
                    source: "dialog-messages"
                    implicitWidth: 18
                    implicitHeight: 18
                    color: Kirigami.Theme.highlightColor
                }
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 0
                    Controls.Label {
                        text: qsTr("Assistant")
                        font.weight: Font.DemiBold
                        renderType: Text.NativeRendering
                    }
                    Controls.Label {
                        text: qsTr("always on · the thread the briefings write to")
                        color: Kirigami.Theme.disabledTextColor
                        font.pixelSize: Math.max(11, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.85))
                        renderType: Text.NativeRendering
                    }
                }
                Controls.Button {
                    text: qsTr("Open")
                    onClicked: Mgr.openAssistant()
                }
            }

            RowLayout {
                Layout.fillWidth: true
                spacing: Theme.spacing.s2
                // No `text:` binding on purpose: a QQC2 TextField breaks one the
                // moment it is edited, and the composer needs nothing but its own
                // text (the draft is read here, never pushed in).
                Controls.TextField {
                    id: draftField
                    Layout.fillWidth: true
                    placeholderText: qsTr("Message the assistant")
                    onAccepted: page.sendAssistant(draftField)
                }
                Controls.Button {
                    text: qsTr("Send")
                    enabled: draftField.text.trim().length > 0
                    onClicked: page.sendAssistant(draftField)
                }
            }
        }
    }

    // ---- row pieces -------------------------------------------------------
    component SectionHeader: RowLayout {
        id: section
        property string title: ""
        property int count: 0
        Layout.fillWidth: true
        Layout.topMargin: Theme.spacing.s2
        spacing: Theme.spacing.s2
        Controls.Label {
            text: section.title
            font.weight: Font.DemiBold
            renderType: Text.NativeRendering
        }
        Controls.Label {
            text: section.count
            visible: section.count > 0
            color: Kirigami.Theme.disabledTextColor
            font.pixelSize: Math.max(11, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.85))
            renderType: Text.NativeRendering
        }
        Item { Layout.fillWidth: true }
    }

    /// A thread with more than one entry: its name, how many sit under it, and the
    /// way into its own conversation.
    component ThreadHeader: Rectangle {
        id: header
        property string label: ""
        property int count: 0
        property bool expanded: false
        Layout.fillWidth: true
        implicitHeight: headRow.implicitHeight + Theme.spacing.s1
        radius: Theme.radius.md
        color: Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g,
                       Kirigami.Theme.textColor.b, 0.06)

        RowLayout {
            id: headRow
            anchors.fill: parent
            anchors.leftMargin: Theme.spacing.s2
            anchors.rightMargin: Theme.spacing.half
            spacing: Theme.spacing.s1
            Kirigami.Icon {
                source: header.expanded ? "arrow-down" : "arrow-right"
                implicitWidth: 14
                implicitHeight: 14
            }
            Controls.Label {
                text: Mgr.threadName(header.label)
                textFormat: Text.PlainText
                elide: Text.ElideRight
                font.weight: Font.DemiBold
                Layout.fillWidth: true
                renderType: Text.NativeRendering
            }
            Controls.Label {
                text: header.count
                color: Kirigami.Theme.disabledTextColor
                font.pixelSize: Math.max(11, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.85))
                renderType: Text.NativeRendering
            }
            Controls.Button {
                text: qsTr("Discuss")
                flat: true
                onClicked: Mgr.openThreadChat(header.label)
            }
        }
        MouseArea {
            anchors.fill: parent
            anchors.rightMargin: Kirigami.Units.gridUnit * 3.5   // leave Discuss clickable
            onClicked: page.toggleLabel(header.label)
        }
    }

    /// One open item: the whole card taps through to its conversation, and the three
    /// buttons are the entire vocabulary the server acts on.
    component ItemCard: Rectangle {
        id: card
        property var row: ({})
        property bool snoozed: false
        Layout.fillWidth: true
        implicitHeight: cardCol.implicitHeight + Theme.spacing.s2
        radius: Theme.radius.md
        // Asleep reads as asleep: muted, so the tap that snoozed it is visible
        // without reading the button label back.
        color: card.snoozed
               ? Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g,
                         Kirigami.Theme.textColor.b, 0.04)
               : Kirigami.Theme.alternateBackgroundColor
        border.width: 1
        border.color: Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g,
                              Kirigami.Theme.textColor.b, 0.10)

        ColumnLayout {
            id: cardCol
            anchors.fill: parent
            anchors.margins: Theme.spacing.s2
            spacing: Theme.spacing.half

            Controls.Label {
                text: card.row.title || ""
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                maximumLineCount: 3
                elide: Text.ElideRight
                Layout.fillWidth: true
                renderType: Text.NativeRendering
            }
            Controls.Label {
                visible: (card.row.detail || "").length > 0
                text: card.row.detail || ""
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                maximumLineCount: 2
                elide: Text.ElideRight
                color: Kirigami.Theme.disabledTextColor
                font.pixelSize: Math.max(11, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.9))
                Layout.fillWidth: true
                renderType: Text.NativeRendering
            }
            Controls.Label {
                // "wakes in 6h · Chami billing · overdue 2d" — built in C++ so the
                // date maths (and its sign) has a unit test.
                text: Mgr.rowMeta(card.row, card.snoozed)
                visible: text.length > 0
                color: Kirigami.Theme.disabledTextColor
                font.pixelSize: Math.max(10, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.8))
                renderType: Text.NativeRendering
            }
            RowLayout {
                Layout.fillWidth: true
                Layout.alignment: Qt.AlignRight
                spacing: Theme.spacing.half
                Controls.Button {
                    text: qsTr("Done")
                    flat: true
                    onClicked: page.closeItemRequested(card.row.id, card.row.title || "", "done")
                }
                Controls.Button {
                    text: card.snoozed ? qsTr("Wake") : qsTr("Snooze")
                    flat: true
                    onClicked: card.snoozed ? Mgr.wakeTask(card.row.id)
                                            : Mgr.snoozeTaskToMorning(card.row.id)
                }
                Controls.Button {
                    text: qsTr("Not real")
                    flat: true
                    onClicked: page.closeItemRequested(card.row.id, card.row.title || "", "dropped")
                }
            }
        }
        // The card body taps through to the conversation about this item: a labelled
        // loop has a thread chat of its own, an unlabelled one goes to the assistant.
        // The item id rides in the opening line so the model can state_close on THIS
        // one instead of guessing.
        MouseArea {
            // Named for the delegate test: the tap-through is the one behaviour of
            // this card that is wiring, not rendering, and it cannot be reached by
            // clicking through QuickTest without a handle on it.
            objectName: "myDayCardBody"
            anchors.fill: parent
            // Sit under the buttons: a click on Done must not also navigate.
            z: -1
            onClicked: {
                if ((card.row.thread || "").length > 0)
                    Mgr.openThreadChat(card.row.thread)
                else
                    Mgr.openAssistant()
                // Last write wins: openThreadChat/openAssistant already seeded a
                // generic line, and this one names the ITEM so the model can
                // state_close on it instead of guessing which loop you mean.
                Mgr.setDraft("About item " + card.row.id + " ("
                             + String(card.row.title || "").substring(0, 80) + "): ")
            }
        }
    }

    /// One handed-over task. Read-only on purpose: its state moves from the server,
    /// and what a person does with it is to go and look at what came back.
    component TaskCard: Rectangle {
        id: card
        property var row: ({})
        readonly property bool reviewing: (card.row.state || "") === "review"
        Layout.fillWidth: true
        implicitHeight: taskCol.implicitHeight + Theme.spacing.s2
        radius: Theme.radius.md
        color: card.reviewing
               ? Qt.rgba(Kirigami.Theme.highlightColor.r, Kirigami.Theme.highlightColor.g,
                         Kirigami.Theme.highlightColor.b, 0.16)
               : Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g,
                         Kirigami.Theme.textColor.b, 0.04)
        border.width: 1
        border.color: Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g,
                              Kirigami.Theme.textColor.b, 0.10)

        ColumnLayout {
            id: taskCol
            anchors.fill: parent
            anchors.margins: Theme.spacing.s2
            spacing: Theme.spacing.half
            Controls.Label {
                text: card.row.title || ""
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                maximumLineCount: 2
                elide: Text.ElideRight
                Layout.fillWidth: true
                renderType: Text.NativeRendering
            }
            Controls.Label {
                // The line it came back with is the point of the row once it is done;
                // while it runs, what was asked for is the only honest thing to show.
                text: (card.row.result || "").length > 0 ? card.row.result : (card.row.request || "")
                visible: text.length > 0
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                maximumLineCount: 2
                elide: Text.ElideRight
                color: Kirigami.Theme.disabledTextColor
                font.pixelSize: Math.max(11, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.9))
                Layout.fillWidth: true
                renderType: Text.NativeRendering
            }
            Controls.Label {
                text: meta.artifactsLine(card.row)
                visible: text.length > 0
                color: Kirigami.Theme.disabledTextColor
                font.pixelSize: Math.max(10, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.8))
                renderType: Text.NativeRendering
            }
        }
        QtObject {
            id: meta
            // "ready for review · Chami billing · 2 files · deck.pdf"
            function artifactsLine(row) {
                const parts = []
                parts.push((row.state || "") === "review" ? qsTr("ready for review") : qsTr("working"))
                const thread = Mgr.threadName(row.thread || "")
                if (thread.length > 0)
                    parts.push(thread)
                const files = Mgr.artifactsLabel(row.artifacts || [])
                if (files.length > 0)
                    parts.push(files)
                return parts.join(" · ")
            }
        }
        MouseArea {
            objectName: "myDayTaskBody"
            anchors.fill: parent
            onClicked: Mgr.openTaskSession(card.row.sessionId || "", card.row.thread || "")
        }
    }

    /// A closed item: struck through, with the line that closed it. Read-only — the
    /// history answers "did I actually deal with that".
    component DoneRow: ColumnLayout {
        id: doneRow
        property var row: ({})
        Layout.fillWidth: true
        spacing: 0
        RowLayout {
            Layout.fillWidth: true
            spacing: Theme.spacing.s1
            Kirigami.Icon {
                source: "checkmark"
                implicitWidth: 12
                implicitHeight: 12
                color: Kirigami.Theme.disabledTextColor
            }
            Controls.Label {
                text: doneRow.row.title || ""
                textFormat: Text.PlainText
                font.strikeout: true
                elide: Text.ElideRight
                color: Kirigami.Theme.disabledTextColor
                Layout.fillWidth: true
                renderType: Text.NativeRendering
            }
        }
        Controls.Label {
            visible: (doneRow.row.resolution || "").length > 0
            text: doneRow.row.resolution || ""
            textFormat: Text.PlainText
            elide: Text.ElideRight
            color: Kirigami.Theme.disabledTextColor
            font.pixelSize: Math.max(10, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.8))
            Layout.leftMargin: Theme.spacing.s4
            Layout.fillWidth: true
            renderType: Text.NativeRendering
        }
    }

    // ---- page -------------------------------------------------------------
    contentItem: ColumnLayout {
        anchors.fill: parent
        spacing: 0

        Rectangle {
            Layout.fillWidth: true
            implicitHeight: headerRow.implicitHeight + Theme.spacing.s4
            color: Kirigami.Theme.alternateBackgroundColor
            Rectangle {
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                height: 1
                color: Kirigami.Theme.separatorColor ? Kirigami.Theme.separatorColor
                                                     : Kirigami.Theme.disabledTextColor
            }
            RowLayout {
                id: headerRow
                anchors.fill: parent
                anchors.margins: Theme.spacing.s2
                spacing: Theme.spacing.s3
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 0
                    Kirigami.Heading {
                        text: qsTr("My Day")
                        level: 3
                        font.weight: Font.DemiBold
                    }
                    Controls.Label {
                        text: page.summaryLine()
                        color: Kirigami.Theme.disabledTextColor
                        font.pixelSize: Math.max(11, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.86))
                        renderType: Text.NativeRendering
                    }
                }
                Controls.BusyIndicator {
                    visible: Mgr.tasksLoading
                    running: Mgr.tasksLoading
                    implicitWidth: 20
                    implicitHeight: 20
                }
                Controls.Button {
                    icon.name: "view-refresh"
                    text: qsTr("Refresh")
                    enabled: !Mgr.tasksLoading
                    onClicked: Mgr.refreshTasks()
                }
                Controls.Button {
                    text: qsTr("Back to chat")
                    onClicked: Mgr.closeMyDay()
                }
            }
        }

        Controls.ScrollView {
            id: scroller
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            Controls.ScrollBar.horizontal.policy: Controls.ScrollBar.AlwaysOff

            ColumnLayout {
                // `availableWidth` already excludes the vertical scrollbar, so rows
                // never run under the overlay bar.
                width: scroller.availableWidth - Theme.spacing.s4
                x: Theme.spacing.s2
                spacing: Theme.spacing.s2

                AssistantCard { Layout.topMargin: Theme.spacing.s2 }

                // "nothing to do" and "could not ask" must not look alike, which is
                // why the envelope carries per-section errors at all.
                Rectangle {
                    Layout.fillWidth: true
                    visible: Mgr.tasksError.length > 0
                    implicitHeight: errRow.implicitHeight + Theme.spacing.s2
                    radius: Theme.radius.md
                    color: Qt.rgba(Kirigami.Theme.negativeTextColor.r,
                                   Kirigami.Theme.negativeTextColor.g,
                                   Kirigami.Theme.negativeTextColor.b, 0.12)
                    RowLayout {
                        id: errRow
                        anchors.fill: parent
                        anchors.margins: Theme.spacing.s2
                        spacing: Theme.spacing.s2
                        Controls.Label {
                            text: Mgr.tasksError
                            textFormat: Text.PlainText
                            wrapMode: Text.Wrap
                            Layout.fillWidth: true
                            color: Kirigami.Theme.negativeTextColor
                            renderType: Text.NativeRendering
                        }
                        Controls.Button {
                            text: qsTr("Retry")
                            onClicked: Mgr.refreshTasks()
                        }
                    }
                }

                SectionHeader { title: qsTr("Open"); count: page.inPlayRows.length }

                Controls.Label {
                    Layout.fillWidth: true
                    visible: page.entryModel.length === 0 && Mgr.tasksError.length === 0
                    text: Mgr.online
                          ? qsTr("Nothing open. The assistant adds here when a briefing finds something.")
                          : qsTr("Not connected — the lists load once the server is reachable.")
                    wrapMode: Text.Wrap
                    color: Kirigami.Theme.disabledTextColor
                    font.pixelSize: Math.max(11, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.9))
                    renderType: Text.NativeRendering
                }

                Repeater {
                    model: page.entryModel
                    delegate: Loader {
                        // `modelData` is one entry of the flattened sequence: a thread
                        // header, a handed-over task, or an open item. Capture it in a
                        // property — a nested component would shadow `modelData`.
                        property var entry: modelData
                        Layout.fillWidth: true
                        sourceComponent: entry.type === "header" ? threadHeaderComp
                                       : entry.type === "task" ? taskCardComp
                                       : itemCardComp
                        // Only what this entry's component HAS: assigning `label` to
                        // an item card is a TypeError at load time (the row simply
                        // never appears, and only a delegate-instantiation test
                        // catches it).
                        onLoaded: {
                            if (entry.type === "header") {
                                item.label = entry.label || ""
                                item.count = entry.count || 0
                                item.expanded = entry.open === true
                            } else {
                                item.row = entry.row
                            }
                        }
                    }
                }

                SectionHeader {
                    title: qsTr("Snoozed")
                    count: page.snoozedModel.length
                    visible: page.snoozedModel.length > 0
                }
                Repeater {
                    model: page.snoozedModel
                    delegate: Loader {
                        property var entry: modelData
                        Layout.fillWidth: true
                        sourceComponent: itemCardComp
                        onLoaded: {
                            item.row = entry
                            item.snoozed = true
                        }
                    }
                }

                SectionHeader {
                    title: qsTr("Recently done")
                    count: Mgr.myDayDone.length
                    visible: Mgr.myDayDone.length > 0
                }
                Repeater {
                    model: Mgr.myDayDone
                    delegate: Loader {
                        property var entry: modelData
                        Layout.fillWidth: true
                        sourceComponent: doneRowComp
                        onLoaded: item.row = entry
                    }
                }

                Item { Layout.preferredHeight: Theme.spacing.s6 }
            }
        }
    }

    Component { id: threadHeaderComp; ThreadHeader { } }
    Component { id: itemCardComp; ItemCard { } }
    Component { id: taskCardComp; TaskCard { } }
    Component { id: doneRowComp; DoneRow { } }
}
