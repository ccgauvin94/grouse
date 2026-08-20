import QtQuick
import QtQuick.Controls
import QtQuick.Controls as Controls
import QtQuick.Layouts
import Grouse 1.0
import org.kde.kirigami as Kirigami

Kirigami.Page {
    id: page

    // Emitted when the user clicks the tools pill; main.qml owns the slide-out panel.
    signal toolsOpenRequested()

    function findOption(id) {
        for (var i = 0; i < Mgr.config.length; i++) {
            if (Mgr.config[i].id === id) return Mgr.config[i]
        }
        return null
    }
    function indexOf(list, value) {
        for (var i = 0; i < list.length; i++)
            if (list[i].value === value) return i
        return -1
    }
    function rebuildSelectors() {
        const p = page.findOption("provider")
        providerChoices = p ? p.choices : []
        providerIndex = page.indexOf(providerChoices, p ? p.currentValue : "")
        const m = page.findOption("model")
        modelChoices = m ? m.choices : []
        modelIndex = page.indexOf(modelChoices, m ? m.currentValue : "")
    }

    property var pendingFiles: []
    property var slashMatches: []

    // Pretty names for the autonomy-mode values the mode pill cycles through.
    function modePrettyName(v) {
        if (v === "auto") return "Auto"
        if (v === "approve") return "Manual"
        if (v === "smart_approve") return "Smart"
        if (v === "chat") return "None"
        const words = String(v).split("_").join(" ").split(" ")
        for (let i = 0; i < words.length; i++)
            words[i] = words[i].charAt(0).toUpperCase() + words[i].substring(1)
        return words.join(" ")
    }
    function currentMode() {
        const o = page.findOption("mode")
        return o ? o.currentValue : ""
    }
    function modeChoices() {
        const o = page.findOption("mode")
        return o ? o.choices : []
    }

    // Compact token-count formatting: 4200 -> "4.2k", 128000 -> "128k".
    function fmtK(n) {
        if (n >= 1000) {
            const v = n / 1000
            const s = v >= 100 ? String(Math.round(v)) : String(Math.round(v * 10) / 10)
            return s + "k"
        }
        return String(n)
    }
    function contextPercent() {
        if (Mgr.contextSize <= 0) return 0
        return Math.round(Mgr.contextUsed * 100 / Mgr.contextSize)
    }
    function formatContext() {
        return page.fmtK(Mgr.contextUsed) + " / " + page.fmtK(Mgr.contextSize) + " · " + page.contextPercent() + "%"
    }

    // Slash-command autocomplete candidates: "/" + prefix, no space yet, and
    // only commands the current session actually accepts.
    function slashCandidates() {
        const t = chatInput ? chatInput.text : ""
        if (!Mgr.online || t.length < 2 || !t.startsWith("/") || t.indexOf(" ") >= 0)
            return []
        const prefix = t.substring(1).toLowerCase()
        const all = Mgr.availableCommands ? Mgr.availableCommands : []
        const out = []
        for (let i = 0; i < all.length && out.length < 6; i++) {
            const name = String(all[i])
            if (name.toLowerCase().startsWith(prefix)) out.push(name)
        }
        return out
    }
    function updateSlashPopup() {
        page.slashMatches = page.slashCandidates()
        if (page.slashMatches.length > 0)
            slashPopup.open()
        else
            slashPopup.close()
    }
    function pickSlashCommand(name) {
        chatInput.text = "/" + name + " "
        chatInput.cursorPosition = chatInput.text.length
        chatInput.focus = true
        page.slashMatches = []
        slashPopup.close()
    }
    function removePendingFile(i) {
        const arr = page.pendingFiles.slice()
        arr.splice(i, 1)
        page.pendingFiles = arr
    }
    function isImageFile(path) {
        const e = String(path).toLowerCase().split(".").pop()
        return ["png", "jpg", "jpeg", "gif", "bmp", "webp", "svg"].indexOf(e) >= 0
    }
    function fileBaseName(path) {
        const parts = String(path).split("/")
        return parts[parts.length - 1]
    }

    property var providerChoices: []
    property int providerIndex: -1
    property var modelChoices: []
    property int modelIndex: -1

    property string hintText: Mgr.online ? "" : qsTr("Choose a session on the left, or start a new chat, then enter a message below.")

    // True while startup is actively connecting / loading a chat. Drives the
    // empty-state spinner; there is nothing to show yet, so say what we're doing.
    function startingUp() {
        const s = Mgr.status
        return s.startsWith("connecting") || s.startsWith("connected") || s === "loading…"
    }
    function busyLabel() {
        const s = Mgr.status
        if (s.startsWith("connecting") || s.startsWith("connected")) return qsTr("Connecting…")
        if (s === "loading…") return qsTr("Loading chat…")
        return ""
    }

    // Per-conversation scroll memory (sessionId -> contentY): lets a user leave
    // a chat scrolled up, switch away, and return to the same spot. First open
    // of a chat in this app session starts at the bottom instead.
    property var scrollMem: ({})
    property string scrollSession: ""
    property bool pendingRestore: false
    property bool pendingFirstScroll: false
    property int scrollApplyAttempts: 0
    // Whether the user is pinned to the end of the transcript. Updated only on
    // real user scrolls (drag/flick), never on the programmatic ListView reset
    // that follows each messagesChanged — so a reset can't silently unpin us.
    property bool pinnedToEnd: true

    Connections {
        target: Mgr
        function onConfigChanged() { page.rebuildSelectors() }
        function onCurrentSessionChanged() { page.handleSessionSwitch(); page.updateSlashPopup() }
        function onMessagesChanged() {
            page.keepScrolled()
            page.applySessionScroll()
        }
        function onOnlineChanged() { page.keepScrolled(); page.updateSlashPopup() }
        function onPromptingChanged() { page.keepScrolled() }
    }

    // Scroll to the end only when the user is already pinned there, and defer
    // the jump so it lands after the model has relaid out. Auto-scrolling on
    // every chunk both fought the user's manual scroll and snapped to a stale
    // contentHeight (which read as a random scroll-up while at the bottom).
    function keepScrolled() {
        if (pinnedToEnd) {
            Qt.callLater(function() { list.positionViewAtEnd() })
        }
    }

    function handleSessionSwitch() {
        // Remember where the outgoing chat was scrolled before the model swaps.
        if (scrollSession.length > 0 && list.contentHeight > 0)
            scrollMem[scrollSession] = list.contentY
        scrollSession = Mgr.currentSessionId
        pendingRestore = false
        pendingFirstScroll = false
        scrollApplyAttempts = 0
        if (scrollSession.length === 0)
            return
        if (scrollSession in scrollMem) {
            pendingRestore = true
            pinnedToEnd = false
        } else {
            pendingFirstScroll = true
            pinnedToEnd = true
        }
    }

    function applySessionScroll() {
        if (!pendingFirstScroll && !pendingRestore)
            return
        // Defer so the jump lands after the new transcript is laid out.
        Qt.callLater(function() {
            // Wait through the model reset and a few layout passes. On a chat
            // switch contentHeight can still describe the outgoing transcript,
            // so checking only for zero is not sufficient.
            if (page.scrollApplyAttempts < 4) {
                ++page.scrollApplyAttempts
                page.applySessionScroll()
                return
            }
            // A large transcript may need additional passes before its first
            // delegate has a useful height.
            if (Mgr.messageModel.count > 0 && list.contentHeight <= 0
                && page.scrollApplyAttempts < 20) {
                ++page.scrollApplyAttempts
                page.applySessionScroll()
                return
            }
            page.scrollApplyAttempts = 0
            if (page.pendingFirstScroll) {
                page.pendingFirstScroll = false
                list.positionViewAtEnd()
            } else if (page.pendingRestore) {
                page.pendingRestore = false
                const y = page.scrollMem[page.scrollSession]
                if (y !== undefined) {
                    const maxY = Math.max(0, list.contentHeight - list.height)
                    list.contentY = Math.min(y, maxY)
                }
            }
        })
    }

    contentItem: ColumnLayout {
        id: pageColumn
        anchors.fill: parent
        spacing: 0

        // header row: title, status, and model selectors
        Rectangle {
            id: headerBar
            Layout.fillWidth: true
            implicitHeight: headerLayout.implicitHeight + Kirigami.Units.largeSpacing * 2
            color: Kirigami.Theme.alternateBackgroundColor

            Rectangle {
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                height: 1
                color: Kirigami.Theme.separatorColor
            }

            RowLayout {
                id: headerLayout
                anchors.fill: parent
                anchors.margins: Kirigami.Units.largeSpacing
                spacing: Kirigami.Units.largeSpacing

                Column {
                    Layout.fillWidth: true
                    spacing: 2
                    Kirigami.Heading {
                        text: Mgr.currentSessionTitle
                        level: 3
                        elide: Text.ElideRight
                        width: parent.width
                        font.weight: Font.DemiBold
                    }
                    Controls.Label {
                        text: Mgr.status
                        color: Kirigami.Theme.disabledTextColor
                        font.pixelSize: Math.max(11, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.86))
                        renderType: Text.NativeRendering
                    }
                }

                // Per-conversation tool indicator: a wrench pill that slides the
                // tools panel out from the right of the chat area.
                Controls.Button {
                    id: toolsPill
                    visible: Mgr.online
                    implicitWidth: Kirigami.Units.gridUnit * 5
                    implicitHeight: Kirigami.Units.gridUnit * 1.5
                    // Breeze's `tools` icon is a toolbox; `configure` is the
                    // actual wrench glyph requested for this control.
                    icon.name: "settings-configure"
                    text: Mgr.tools.length === 0 ? qsTr("Tools") : qsTr("Tools") + " " + Mgr.tools.length
                    display: Controls.AbstractButton.TextBesideIcon
                    leftPadding: Kirigami.Units.smallSpacing * 2
                    rightPadding: Kirigami.Units.smallSpacing * 2
                    background: Rectangle {
                        radius: height / 2
                        color: toolsPill.hovered ? Kirigami.Theme.highlightColor : Kirigami.Theme.backgroundColor
                        border.color: Kirigami.Theme.separatorColor
                        border.width: 1
                        opacity: toolsPill.hovered ? 0.82 : 1
                    }
                    ToolTip.visible: hovered
                    ToolTip.text: qsTr("Configure tools for this chat")
                    onClicked: page.toolsOpenRequested()
                }
            }
        }

        // Context usage + compaction status line. Collapses to zero height when
        // the session has no context window and compaction isn't running.
        Rectangle {
            id: contextRow
            Layout.fillWidth: true
            visible: Mgr.compacting || Mgr.contextSize > 0
            implicitHeight: contextRowLayout.implicitHeight + Kirigami.Units.smallSpacing * 2
            color: "transparent"

            RowLayout {
                id: contextRowLayout
                anchors.fill: parent
                anchors.margins: Kirigami.Units.smallSpacing
                spacing: Kirigami.Units.smallSpacing

                BusyIndicator {
                    visible: Mgr.compacting
                    Layout.preferredWidth: 16
                    Layout.preferredHeight: 16
                    running: Mgr.compacting
                }
                Controls.Label {
                    visible: Mgr.compacting
                    text: qsTr("Compacting…")
                    opacity: 0.8
                }
                Controls.Label {
                    visible: !Mgr.compacting && Mgr.contextSize > 0
                    text: page.formatContext()
                    opacity: page.contextPercent() >= 90 ? 1 : 0.7
                    color: page.contextPercent() >= 90 ? Kirigami.Theme.negativeTextColor : Kirigami.Theme.textColor
                    font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.8
                }
                Item {
                    Layout.fillWidth: true
                }
                Controls.Button {
                    visible: Mgr.online && Mgr.contextSize > 0 && !Mgr.compacting && !Mgr.prompting
                    flat: true
                    icon.name: "edit-clear"
                    text: qsTr("Compact conversation")
                    onClicked: Mgr.compactConversation()
                }
            }
        }

        Column {
            visible: Mgr.messageModel.count === 0
            Layout.fillWidth: true
            Layout.topMargin: Kirigami.Units.largeSpacing
            spacing: Kirigami.Units.smallSpacing

            BusyIndicator {
                anchors.horizontalCenter: parent.horizontalCenter
                visible: page.startingUp()
                running: page.startingUp()
            }
            Controls.Label {
                text: page.startingUp() ? page.busyLabel()
                      : (Mgr.online && Mgr.messageModel.count === 0
                         ? qsTr("Start a new conversation") : page.hintText)
                anchors.horizontalCenter: parent.horizontalCenter
                horizontalAlignment: Text.AlignHCenter
                color: Kirigami.Theme.disabledTextColor
                font.pixelSize: Math.max(12, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.92))
                renderType: Text.NativeRendering
            }
        }

        // Chat list, wrapped so the scroll-to-bottom button can overlay it.
        // The bottom margin keeps the last message (and the waiting indicator)
        // from colliding with the input bar.
        Item {
            Layout.fillWidth: true
            Layout.fillHeight: true
            Layout.bottomMargin: Kirigami.Units.largeSpacing

            ListView {
            id: list
            anchors.fill: parent
            clip: true
            model: Mgr.messageModel
            spacing: Kirigami.Units.largeSpacing
            boundsBehavior: Flickable.StopAtBounds
            // Track pin state only from real user scrolls (wheel/drag/flick all
            // set `moving`); the programmatic reset on messagesChanged and the
            // positionViewAtEnd jump don't, so they can't clear the pin.
            onContentYChanged: {
                if (list.moving)
                    pinnedToEnd = list.atYEnd
            }
            onCountChanged: page.applySessionScroll()
            onContentHeightChanged: page.applySessionScroll()
            onHeightChanged: page.applySessionScroll()
            ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }

            // Inline delegate (a separate-file delegate loses the `model`
            // context for root-object bindings; inline works). Reads role names
            // from the MessageListModel via `model.<role>`.
            delegate: Item {
                id: mdel
                width: ListView.view.width

                readonly property string messageRole: model.role ? model.role : ""
                readonly property string messageText: model.text ? model.text : ""
                readonly property string messageHtml: model.html ? model.html : ""
                readonly property string messageTitle: model.title ? model.title : ""
                readonly property string messageDetail: model.detail ? model.detail : ""
                readonly property string messageOutput: model.output ? model.output : ""
                readonly property string messageStatus: model.status ? model.status : ""
                readonly property var messageImages: model.images ? model.images : []
                readonly property string messageUsage: model.usage ? String(model.usage) : ""
                readonly property string messageChartData: model.chartData ? String(model.chartData) : ""
                readonly property string messageAppHtml: model.appHtml ? String(model.appHtml) : ""
                readonly property var messageCalls: model.calls ? model.calls : []

                readonly property bool isUser: mdel.messageRole === "user"
                readonly property bool isAgent: mdel.messageRole === "agent"
                readonly property bool isTool: mdel.messageRole === "tool"
                readonly property bool isToolGroup: mdel.messageRole === "toolgroup"
                readonly property bool isThought: mdel.messageRole === "thought"
                readonly property bool isError: mdel.messageRole === "error"
                readonly property bool isChart: mdel.messageRole === "chart"
                readonly property bool isMcpApp: mdel.messageRole === "mcpapp"

                // A native Canvas chart reserves ~220-266px depending on type + title.
                readonly property real chartH: (function() {
                    if (mdel.messageChartData.length === 0) return 140
                    var spec = null
                    try { spec = JSON.parse(mdel.messageChartData) } catch (e) { return 140 }
                    var base = (spec.type === "pie" || spec.type === "doughnut") ? 240 : 220
                    if (spec.options && spec.options.title && spec.options.title.text) base += 26
                    return base
                })()

                readonly property real pad: Kirigami.Units.smallSpacing * 1.5
                readonly property real gap: Kirigami.Units.largeSpacing * 1.5
                // QQC2's vertical scrollbar overlays the ListView viewport;
                // reserve its width so bubbles/chips never sit underneath it.
                readonly property real scrollbarReserve: Kirigami.Units.gridUnit * 1.5
                readonly property real maxW: Math.max(0, mdel.width - mdel.gap * 2 - mdel.scrollbarReserve)
                readonly property real bubbleW: Math.min(mdel.maxW, 620)
                readonly property real userBubbleW: Math.min(mdel.maxW * 0.72, 520)
                readonly property real agentBubbleW: Math.min(mdel.maxW, 720)
                readonly property int truncOutput: 4000   // perf/scroll cap for tool output

                property bool toolOpen: false
                // Thoughts start collapsed, even mid-turn; the open state lives
                // in the message model (ExpandedRole) so it survives scrolling.
                readonly property bool thoughtOpen: !!model.expanded

                function shortOutput() {
                    return mdel.messageOutput.length > mdel.truncOutput
                        ? mdel.messageOutput.substring(0, mdel.truncOutput) + "…"
                        : mdel.messageOutput
                }
                function statusText() {
                    return mdel.messageStatus === "completed" ? ("✓ " + qsTr("done"))
                         : mdel.messageStatus === "failed" ? qsTr("failed") : mdel.messageStatus
                }
                function groupTitle() {
                    if (mdel.messageCalls.length === 0) return qsTr("Tool calls")
                    var first = String(mdel.messageCalls[0].title || qsTr("call"))
                    var same = true
                    for (var i = 1; i < mdel.messageCalls.length; i++) {
                        if (String(mdel.messageCalls[i].title || qsTr("call")) !== first) {
                            same = false
                            break
                        }
                    }
                    return same ? mdel.messageCalls.length + " × " + first
                                : mdel.messageCalls.length + " " + qsTr("tool calls")
                }
                function callStatus(call) {
                    return call.status === "completed" ? "✓ " + qsTr("done")
                         : call.status === "failed" ? qsTr("failed") : call.status
                }

                height: mdel.isUser ? userBubble.implicitHeight
                     : mdel.isAgent ? agentBubble.implicitHeight
                     : mdel.isTool ? toolChip.implicitHeight
                     : mdel.isToolGroup ? toolGroupChip.implicitHeight
                     : mdel.isThought ? thoughtBlock.implicitHeight
                     : mdel.isChart ? mdel.chartH + mdel.pad * 2
                     : mdel.isMcpApp ? mcpAppChip.implicitHeight
                     : errorBanner.implicitHeight

                // ---------------- user bubble (right) ----------------
                Rectangle {
                    id: userBubble
                    visible: mdel.isUser
                    anchors.top: parent.top
                    anchors.right: parent.right
                    anchors.rightMargin: mdel.gap + mdel.scrollbarReserve
                    width: mdel.userBubbleW
                    implicitHeight: userCol.implicitHeight + mdel.pad * 2
                    radius: Theme.radius.xl
                    color: Kirigami.Theme.highlightColor
                    clip: true

                    Column {
                        id: userCol
                        anchors.fill: parent
                        anchors.margins: mdel.pad
                        spacing: 2
                        Controls.Label {
                            text: qsTr("You")
                            font.pixelSize: Math.max(11, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.82))
                            renderType: Text.NativeRendering
                            color: Kirigami.Theme.highlightedTextColor
                            opacity: 0.86
                        }
                        Column {
                            visible: mdel.messageImages.length > 0
                            spacing: Kirigami.Units.smallSpacing
                            Repeater {
                                model: mdel.messageImages
                                delegate: Item {
                                    width: parent.width
                                    height: modelData.image ? 140 : fileChip.implicitHeight
                                    Image {
                                        visible: modelData.image
                                        anchors.left: parent.left
                                        source: modelData.url
                                        width: Math.min(140, implicitWidth)
                                        height: 140
                                        fillMode: Image.PreserveAspectFit
                                    }
                                    Rectangle {
                                        id: fileChip
                                        visible: !modelData.image
                                        anchors.left: parent.left
                                        height: 26
                                        width: fileChipRow.implicitWidth + Kirigami.Units.smallSpacing * 2
                                        radius: 6
                                        color: Qt.lighter(Kirigami.Theme.highlightColor, 1.7)
                                        Row {
                                            id: fileChipRow
                                            anchors.centerIn: parent
                                            spacing: Kirigami.Units.smallSpacing
                                            Kirigami.Icon {
                                                source: "text-x-generic"
                                                implicitWidth: 16
                                                implicitHeight: 16
                                            }
                                            Controls.Label {
                                                text: modelData.name
                                                elide: Text.ElideRight
                                                color: Kirigami.Theme.highlightedTextColor
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Controls.Label {
                            text: mdel.messageHtml || mdel.messageText
                            // PlainText until the row is finalized: with RichText, Qt
                            // re-parses + reshapes the WHOLE accumulated message on every
                            // streamed chunk (quadratic). html is empty while a turn
                            // streams and is set once at finalize.
                            textFormat: mdel.messageHtml.length > 0 ? Text.RichText : Text.PlainText
                            width: mdel.userBubbleW - mdel.pad * 2
                            wrapMode: Text.Wrap
                            color: Kirigami.Theme.highlightedTextColor
                        }
                    }
                }

                // ---------------- agent bubble (left) ----------------
                Rectangle {
                    id: agentBubble
                    visible: mdel.isAgent
                    anchors.top: parent.top
                    anchors.left: parent.left
                    anchors.leftMargin: mdel.gap
                    width: mdel.agentBubbleW
                    implicitHeight: agentCol.implicitHeight + mdel.pad * 2
                    radius: Theme.radius.xl
                    color: Kirigami.Theme.alternateBackgroundColor
                    border.color: Kirigami.Theme.separatorColor
                    border.width: 1
                    clip: true

                    Column {
                        id: agentCol
                        anchors.fill: parent
                        anchors.margins: mdel.pad
                        spacing: 2
                        Controls.Label {
                            text: qsTr("Goose")
                            font.pixelSize: Math.max(11, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.82))
                            font.weight: Font.DemiBold
                            renderType: Text.NativeRendering
                            color: Kirigami.Theme.disabledTextColor
                        }
                        Controls.Label {
                            text: mdel.messageHtml || mdel.messageText
                            // PlainText while streaming (see the user bubble note above);
                            // RichText once finalize has rendered the markdown to html.
                            textFormat: mdel.messageHtml.length > 0 ? Text.RichText : Text.PlainText
                            width: mdel.agentBubbleW - mdel.pad * 2
                            wrapMode: Text.Wrap
                            onLinkActivated: Qt.openUrlExternally(link)
                            color: Kirigami.Theme.textColor
                        }
                        Controls.Label {
                            visible: mdel.messageUsage.length > 0
                            text: mdel.messageUsage
                            color: Kirigami.Theme.disabledTextColor
                            font.pixelSize: Math.max(10, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.8))
                            renderType: Text.NativeRendering
                        }
                    }
                }

                // ---------------- tool chip ----------------
                Rectangle {
                    id: toolChip
                    visible: mdel.isTool
                    anchors.top: parent.top
                    anchors.left: parent.left
                    anchors.leftMargin: mdel.gap
                    width: Math.min(mdel.agentBubbleW * 0.78, 560)
                    implicitHeight: chipCol.implicitHeight + mdel.pad * 2
                    radius: Theme.radius.lg
                    color: Qt.lighter(Kirigami.Theme.backgroundColor, 1.18)
                    border.color: Kirigami.Theme.separatorColor
                    border.width: 1

                    Column {
                        id: chipCol
                        anchors.fill: parent
                        anchors.margins: mdel.pad
                        spacing: 0
                        RowLayout {
                            height: Kirigami.Units.gridUnit * 1.25
                            width: parent.width
                            spacing: Kirigami.Units.smallSpacing
                            Kirigami.Icon {
                                source: "configure"
                                implicitWidth: 16
                                implicitHeight: 16
                                Layout.alignment: Qt.AlignVCenter
                            }
                            Controls.Label {
                                text: qsTr("Tool") + ": " + (mdel.messageTitle || qsTr("call"))
                                font.pixelSize: Math.max(11, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.88))
                                renderType: Text.NativeRendering
                                color: Kirigami.Theme.disabledTextColor
                                elide: Text.ElideRight
                                verticalAlignment: Text.AlignVCenter
                                Layout.fillWidth: true
                            }
                            Controls.Label {
                                id: statusLabel
                                text: mdel.statusText()
                                color: mdel.messageStatus === "failed" ? Kirigami.Theme.negativeTextColor : Kirigami.Theme.disabledTextColor
                                font.pixelSize: Math.max(10, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.82))
                                renderType: Text.NativeRendering
                                verticalAlignment: Text.AlignVCenter
                            }
                        }
                        Controls.Label {
                            visible: mdel.toolOpen && mdel.messageOutput.length > 0
                            text: (mdel.messageDetail.length > 0 ? mdel.messageDetail + "\n\n" : "") + mdel.shortOutput()
                            width: parent.width
                            wrapMode: Text.Wrap
                            opacity: 0.8
                            font.family: "monospace"
                        }
                        Controls.Button {
                            visible: mdel.toolOpen
                            flat: true
                            text: qsTr("Hide raw details")
                            height: Kirigami.Units.gridUnit * 1.25
                            onClicked: mdel.toolOpen = false
                        }
                    }
                    TapHandler { onTapped: mdel.toolOpen = !mdel.toolOpen }
                }

                // Consecutive tool calls share one compact activity chip. Expanding
                // it reveals each call's input/output while preserving individual
                // statuses and streaming output.
                Rectangle {
                    id: toolGroupChip
                    visible: mdel.isToolGroup
                    anchors.top: parent.top
                    anchors.left: parent.left
                    anchors.leftMargin: mdel.gap
                    width: Math.min(mdel.agentBubbleW * 0.78, 560)
                    implicitHeight: groupCol.implicitHeight + mdel.pad * 2
                    radius: Theme.radius.lg
                    color: Qt.lighter(Kirigami.Theme.backgroundColor, 1.18)
                    border.color: Kirigami.Theme.separatorColor
                    border.width: 1

                    Column {
                        id: groupCol
                        anchors.fill: parent
                        anchors.margins: mdel.pad
                        spacing: Kirigami.Units.smallSpacing

                        RowLayout {
                            width: parent.width
                            height: Kirigami.Units.gridUnit * 1.25
                            spacing: Kirigami.Units.smallSpacing
                            // Same wrench as the single-call tool chip: the
                            // grouped chip must read as a tool activity too.
                            Kirigami.Icon {
                                source: "configure"
                                implicitWidth: 16
                                implicitHeight: 16
                                Layout.alignment: Qt.AlignVCenter
                            }
                            Controls.Label {
                                text: mdel.groupTitle()
                                Layout.fillWidth: true
                                elide: Text.ElideRight
                                color: Kirigami.Theme.textColor
                                font.weight: Font.DemiBold
                                verticalAlignment: Text.AlignVCenter
                            }
                            Controls.Label {
                                id: groupStatus
                                text: mdel.toolOpen ? qsTr("hide") : qsTr("show")
                                color: Kirigami.Theme.disabledTextColor
                                font.pixelSize: Math.max(10, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.82))
                                verticalAlignment: Text.AlignVCenter
                            }
                        }

                        Column {
                            visible: mdel.toolOpen
                            width: parent.width
                            spacing: Kirigami.Units.smallSpacing
                            Repeater {
                                model: mdel.messageCalls
                                delegate: Column {
                                    width: parent.width
                                    spacing: 2
                                    Row {
                                        width: parent.width
                                        Controls.Label {
                                            text: modelData.title || qsTr("Tool call")
                                            width: parent.width - callStatusLabel.implicitWidth - parent.spacing
                                            elide: Text.ElideRight
                                            font.weight: Font.DemiBold
                                        }
                                        Controls.Label {
                                            id: callStatusLabel
                                            text: mdel.callStatus(modelData)
                                            color: modelData.status === "failed" ? Kirigami.Theme.negativeTextColor
                                                                                  : Kirigami.Theme.disabledTextColor
                                        }
                                    }
                                    Controls.Label {
                                        visible: !!modelData.detail || !!modelData.output
                                        text: (modelData.detail || "")
                                              + (modelData.output ? "\n" + modelData.output : "")
                                        width: parent.width
                                        wrapMode: Text.Wrap
                                        font.family: "monospace"
                                        color: Kirigami.Theme.disabledTextColor
                                    }
                                }
                            }
                        }
                    }
                    TapHandler { onTapped: mdel.toolOpen = !mdel.toolOpen }
                }

                // ---------------- chart bubble ----------------
                Rectangle {
                    id: chartBubble
                    visible: mdel.isChart
                    anchors.top: parent.top
                    anchors.left: parent.left
                    anchors.leftMargin: mdel.gap
                    width: mdel.bubbleW
                    height: mdel.chartH + mdel.pad * 2
                    radius: 16
                    color: Kirigami.Theme.alternateBackgroundColor
                    border.color: Kirigami.Theme.disabledTextColor
                    border.width: 1
                    clip: true

                    ChartBubble {
                        anchors.fill: parent
                        anchors.margins: mdel.pad
                        chartSpec: mdel.messageChartData
                        title: mdel.messageTitle
                    }
                }

                // ---------------- MCP-App bubble ----------------
                // A server-hosted HTML template ("ui://..." resource) would need a browser
                // engine to render; the desktop shows the fetched template status and keeps
                // the tool's input expandable instead (rendering is a follow-up decision).
                Rectangle {
                    id: mcpAppChip
                    visible: mdel.isMcpApp
                    anchors.top: parent.top
                    anchors.left: parent.left
                    anchors.leftMargin: mdel.gap
                    width: Math.min(mdel.agentBubbleW * 0.78, 560)
                    implicitHeight: mcpCol.implicitHeight + mdel.pad * 2
                    radius: Theme.radius.sm
                    color: Qt.lighter(Kirigami.Theme.backgroundColor, 1.18)
                    border.color: Kirigami.Theme.separatorColor
                    border.width: 1

                    Column {
                        id: mcpCol
                        anchors.fill: parent
                        anchors.margins: mdel.pad
                        spacing: 0
                        Row {
                            height: Kirigami.Units.gridUnit * 1.25
                            width: parent.width
                            Controls.Label {
                                text: qsTr("Visualization") + ": " + (mdel.messageTitle || qsTr("call"))
                                font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.9
                                color: Kirigami.Theme.disabledTextColor
                                elide: Text.ElideRight
                                width: parent.width - mcpStatus.implicitWidth - parent.spacing
                                verticalAlignment: Text.AlignVCenter
                            }
                            Controls.Label {
                                id: mcpStatus
                                text: mdel.messageStatus === "failed" ? qsTr("failed")
                                     : mdel.messageAppHtml.length > 0 ? qsTr("template ready")
                                     : mdel.messageStatus
                                color: mdel.messageStatus === "failed" ? Kirigami.Theme.negativeTextColor : Kirigami.Theme.disabledTextColor
                                font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
                                verticalAlignment: Text.AlignVCenter
                            }
                        }
                        Controls.Label {
                            visible: mdel.toolOpen
                            text: mdel.messageDetail
                            width: parent.width
                            wrapMode: Text.Wrap
                            opacity: 0.8
                            font.family: "monospace"
                        }
                        Controls.Label {
                            visible: mdel.messageAppHtml.length > 0 && mdel.toolOpen
                            text: qsTr("Template fetched; interactive rendering needs a web engine (not available on desktop).")
                            width: parent.width
                            wrapMode: Text.Wrap
                            opacity: 0.6
                            font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.8
                        }
                    }
                    TapHandler { onTapped: mdel.toolOpen = !mdel.toolOpen }
                }

                // ---------------- collapsible thinking ----------------
                Item {
                    id: thoughtBlock
                    visible: mdel.isThought
                    anchors.top: parent.top
                    anchors.left: parent.left
                    anchors.leftMargin: mdel.gap
                    anchors.right: parent.right
                    anchors.rightMargin: mdel.gap + mdel.scrollbarReserve
                    implicitHeight: thoughtCol.implicitHeight

                    Column {
                        id: thoughtCol
                        width: parent.width
                        spacing: 0
                        Controls.Button {
                            flat: true
                            text: mdel.thoughtOpen ? qsTr("Thinking") + " -" : qsTr("Thinking") + " >"
                            height: Kirigami.Units.gridUnit * 1.25
                            onClicked: Mgr.messageModel.toggleExpanded(model.id)
                            opacity: 0.7
                        }
                        Controls.Label {
                            visible: mdel.thoughtOpen
                            text: mdel.messageText
                            width: parent.width
                            wrapMode: Text.Wrap
                            opacity: 0.7
                            font.italic: true
                        }
                    }
                }

                // ---------------- error banner ----------------
                Rectangle {
                    id: errorBanner
                    visible: mdel.isError
                    anchors.top: parent.top
                    anchors.left: parent.left
                    anchors.leftMargin: mdel.gap
                    anchors.right: parent.right
                    anchors.rightMargin: mdel.gap + mdel.scrollbarReserve
                    implicitHeight: errCol.implicitHeight + mdel.pad
                    radius: 6
                    color: Kirigami.Theme.negativeBackgroundColor

                    Controls.Label {
                        id: errCol
                        text: mdel.messageText
                        color: Kirigami.Theme.negativeTextColor
                        wrapMode: Text.Wrap
                        anchors.fill: parent
                        anchors.margins: mdel.pad
                    }
                }
            }
        }

            // One-click jump back to the bottom when the user has scrolled up
            // more than a viewport's worth of content.
            Controls.Button {
                id: jumpToBottomButton
                icon.name: "go-down"
                display: Controls.AbstractButton.IconOnly
                visible: list.contentHeight - list.contentY - list.height > list.height
                z: 2
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                anchors.rightMargin: Kirigami.Units.largeSpacing + Kirigami.Units.gridUnit * 1.5
                anchors.bottomMargin: Kirigami.Units.largeSpacing
                onClicked: {
                    pinnedToEnd = true
                    list.positionViewAtEnd()
                    if (page.scrollSession.length > 0)
                        page.scrollMem[page.scrollSession] = list.contentHeight
                }
            }

            // Floating waiting indicator: OVERLAYS the list instead of being a
            // ListView footer, so its height never pollutes contentHeight and
            // positionViewAtEnd always lands exactly on the last message. Left
            // margin matches the delegate's `gap` so it lines up under the
            // agent bubbles instead of hanging out to their right.
            Rectangle {
                id: waitBubble
                visible: Mgr.prompting
                z: 2
                height: Kirigami.Units.gridUnit * 2
                width: busy.implicitWidth + busy.spacing + waitLabel.implicitWidth + Kirigami.Units.smallSpacing * 2
                radius: 16
                color: Kirigami.Theme.alternateBackgroundColor
                border.color: Kirigami.Theme.disabledTextColor
                border.width: 1
                anchors.left: parent.left
                anchors.bottom: parent.bottom
                anchors.leftMargin: Kirigami.Units.largeSpacing * 1.5
                anchors.bottomMargin: Kirigami.Units.smallSpacing
                Row {
                    anchors.centerIn: parent
                    spacing: Kirigami.Units.smallSpacing
                    BusyIndicator {
                        id: busy
                        width: 18
                        height: 18
                        running: Mgr.prompting
                    }
                    Controls.Label {
                        id: waitLabel
                        text: qsTr("Grouse is working…")
                        opacity: 0.8
                        verticalAlignment: Text.AlignVCenter
                    }
                }
            }
        }

        // Pending file attachments: removable chips above the input. Images
        // show a thumbnail; other files show a generic icon + name.
        Item {
            id: attachmentsArea
            Layout.fillWidth: true
            visible: page.pendingFiles.length > 0
            implicitHeight: attachRow.implicitHeight + Kirigami.Units.smallSpacing
            clip: true

            Row {
                id: attachRow
                anchors.fill: parent
                anchors.topMargin: Kirigami.Units.smallSpacing
                anchors.leftMargin: Kirigami.Units.smallSpacing
                anchors.rightMargin: Kirigami.Units.smallSpacing
                spacing: Kirigami.Units.smallSpacing
                Repeater {
                    model: page.pendingFiles
                    Rectangle {
                        height: Kirigami.Units.gridUnit * 2.5
                        width: (page.isImageFile(modelData) ? 60 : 24)
                               + 170 + Kirigami.Units.gridUnit * 1.25
                               + Kirigami.Units.smallSpacing * 5
                        radius: 8
                        color: Kirigami.Theme.alternateBackgroundColor
                        border.color: Kirigami.Theme.disabledTextColor
                        border.width: 1
                        Row {
                            anchors.fill: parent
                            anchors.margins: Kirigami.Units.smallSpacing
                            spacing: Kirigami.Units.smallSpacing
                            Image {
                                id: thumb
                                visible: page.isImageFile(modelData)
                                source: "file://" + modelData
                                width: 60
                                height: parent.height
                                fillMode: Image.PreserveAspectFit
                            }
                            Kirigami.Icon {
                                visible: !page.isImageFile(modelData)
                                source: "text-x-generic"
                                implicitWidth: 24
                                implicitHeight: 24
                                anchors.verticalCenter: parent.verticalCenter
                            }
                            Controls.Label {
                                text: page.fileBaseName(modelData)
                                width: 170
                                elide: Text.ElideRight
                                anchors.verticalCenter: parent.verticalCenter
                            }
                            Controls.Button {
                                id: removeThumb
                                width: Kirigami.Units.gridUnit * 1.25
                                height: parent.height
                                text: "✕"
                                flat: true
                                onClicked: page.removePendingFile(index)
                            }
                        }
                    }
                }
            }
        }

        // Non-blocking queue indicator while the current turn still runs.
        Controls.Label {
            id: queuedChip
            Layout.fillWidth: true
            visible: Mgr.queuedCount > 0
            leftPadding: Kirigami.Units.largeSpacing
            topPadding: Kirigami.Units.smallSpacing
            text: qsTr("%1 queued — will send when this turn finishes").arg(Mgr.queuedCount)
            opacity: 0.8
            font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
        }

        // input bar. Buttons and the text entry share one height so the bar
        // reads as a single control row instead of a tall box flanked by
        // short buttons. The provider/model strip sits under the input line.
        Rectangle {
            id: inputBar
            Layout.fillWidth: true
            implicitHeight: inputCol.implicitHeight + Kirigami.Units.smallSpacing * 2
            color: Kirigami.Theme.alternateBackgroundColor

            ColumnLayout {
                id: inputCol
                anchors.fill: parent
                anchors.margins: Kirigami.Units.smallSpacing
                spacing: Kirigami.Units.smallSpacing

                RowLayout {
                    id: inputRow
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.smallSpacing

                    Controls.Button {
                        id: modePill
                        visible: Mgr.online
                        Layout.alignment: Qt.AlignVCenter
                        icon.name: "tools-wizard"
                        text: page.modePrettyName(page.currentMode())
                        display: Controls.AbstractButton.TextBesideIcon
                        leftPadding: Kirigami.Units.smallSpacing * 2
                        rightPadding: Kirigami.Units.smallSpacing * 2
                        background: Rectangle {
                            radius: height / 2
                            color: modePill.hovered ? Kirigami.Theme.highlightColor : Kirigami.Theme.alternateBackgroundColor
                            border.color: Kirigami.Theme.disabledTextColor
                            border.width: 1
                            opacity: modePill.hovered ? 0.5 : 1
                        }
                        onClicked: modeMenu.popup()

                        Controls.Menu {
                            id: modeMenu
                            Instantiator {
                                model: page.modeChoices()
                                Controls.MenuItem {
                                    text: modelData.name
                                    onTriggered: Mgr.setConfigOption("mode", modelData.value)
                                }
                            }
                        }
                    }

                Controls.Button {
                    id: attachButton
                    visible: Mgr.online
                    width: Kirigami.Units.gridUnit * 3
                    Layout.alignment: Qt.AlignVCenter
                    icon.name: "mail-attachment"
                    display: Controls.AbstractButton.IconOnly
                    ToolTip.visible: hovered
                    ToolTip.text: qsTr("Attach files")
                    // Native KDE file picker (any file type) via the Manager.
                    onClicked: {
                        const files = Mgr.pickAttachmentFiles()
                        if (files.length === 0)
                            return
                        const added = []
                        for (let i = 0; i < files.length; i++)
                            if (page.pendingFiles.indexOf(files[i]) < 0)
                                added.push(files[i])
                        if (added.length > 0)
                            page.pendingFiles = page.pendingFiles.concat(added)
                    }
                }

                Controls.TextArea {
                    id: chatInput
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    Layout.minimumWidth: 120
                    placeholderText: qsTr("Message Goose…")
                    wrapMode: TextEdit.Wrap
                    focus: true
                    onTextChanged: page.updateSlashPopup()
                    Keys.onPressed: (event) => {
                        if (event.key === Qt.Key_Return && !(event.modifiers & Qt.ShiftModifier)) {
                            page.send()
                            event.accepted = true
                        }
                    }
                }

                Controls.Button {
                    id: sendButton
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 3
                    Layout.alignment: Qt.AlignVCenter
                    text: Mgr.prompting ? qsTr("Stop") : qsTr("Send")
                    onClicked: Mgr.prompting ? Mgr.cancelTurn() : page.send()
                }
                }

                // Chat provider + model drop-downs, always visible under the
                // input line (the old header ComboBoxes were gated behind
                // width >= 900 and the pill replacement's menu never popped).
                // Session-scoped: changing applies to this chat and is
                // re-applied on reopen (AcpClient::applyDesired).
                RowLayout {
                    id: providerStrip
                    Layout.fillWidth: true
                    visible: Mgr.online
                    spacing: Kirigami.Units.smallSpacing

                    Controls.Label {
                        text: qsTr("Provider")
                        color: Kirigami.Theme.disabledTextColor
                        font.pixelSize: Math.max(11, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.82))
                        renderType: Text.NativeRendering
                    }
                    Controls.ComboBox {
                        id: providerCombo
                        Layout.preferredWidth: 150
                        Layout.maximumWidth: 170
                        textRole: "name"
                        // currentValue returns the model ITEM unless a valueRole
                        // is set — without it setConfigOption got a JS object
                        // ("[object V4ReferenceObject]"), silently breaking
                        // provider switches.
                        valueRole: "value"
                        model: page.providerChoices
                        currentIndex: page.providerIndex
                        onActivated: Mgr.setConfigOption("provider", currentValue)
                        ToolTip.visible: hovered
                        ToolTip.text: qsTr("Provider for this chat")
                    }
                    Controls.Label {
                        text: qsTr("Model")
                        color: Kirigami.Theme.disabledTextColor
                        font.pixelSize: Math.max(11, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.82))
                        renderType: Text.NativeRendering
                    }
                    Controls.ComboBox {
                        id: modelCombo
                        Layout.preferredWidth: 180
                        Layout.maximumWidth: 210
                        textRole: "name"
                        valueRole: "value"
                        model: page.modelChoices
                        currentIndex: page.modelIndex
                        onActivated: Mgr.setConfigOption("model", currentValue)
                        ToolTip.visible: hovered
                        ToolTip.text: qsTr("Model for this chat")
                    }
                    Item { Layout.fillWidth: true }
                }
            }

            // Slash-command autocomplete popup floating above the input row.
            // QQC2 popups position via x/y relative to the parent item, not anchors.
            Controls.Popup {
                id: slashPopup
                x: inputRow.x
                y: -slashPopup.height - Kirigami.Units.smallSpacing
                width: Math.min(320, page.width * 0.5)
                padding: Kirigami.Units.smallSpacing
                closePolicy: Controls.Popup.CloseOnEscape | Controls.Popup.CloseOnPressOutside

                Column {
                    id: slashCol
                    width: parent.width
                    spacing: 2
                    Repeater {
                        model: page.slashMatches
                        Controls.Button {
                            width: slashCol.width
                            height: 28
                            text: "/" + modelData
                            flat: true
                            onClicked: page.pickSlashCommand(modelData)
                        }
                    }
                }
            }
        }
    }

    function send() {
        const t = chatInput.text
        const files = page.pendingFiles
        if (t.trim().length === 0 && files.length === 0) return
        Mgr.sendPrompt(t, files)
        chatInput.clear()
        page.pendingFiles = []
        chatInput.focus = true
        // Sending means the user wants to be at the bottom to see the reply.
        pinnedToEnd = true
        Qt.callLater(function() { list.positionViewAtEnd() })
    }
    function resetInput() {
        Mgr.newChat()
        chatInput.clear()
        chatInput.focus = true
    }
}
