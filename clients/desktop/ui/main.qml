import QtQuick
import QtQuick.Controls
import QtQuick.Controls as Controls
import QtQuick.Dialogs
import QtQuick.Layouts
import Grouse 1.0
import org.kde.kirigami as Kirigami

// Controls.ApplicationWindow (not Kirigami.ApplicationWindow): its contentItem
// is built-in/read-only, so a sidebar+page layout can't be injected there.
// A plain QQC window lets us own the whole content layout.
Controls.ApplicationWindow {
    id: root
    title: qsTr("Grouse") + " — " + (Mgr.currentSessionTitle)
    visible: true

    Kirigami.Theme.colorSet: Kirigami.Theme.Window

    width: 1120
    height: 720
    minimumWidth: 760
    minimumHeight: 480

    // Keep sidebar text on whole device-independent pixels. Fractional point
    // sizes and alpha-blended glyphs are especially soft at 125/150% scaling.
    readonly property int sidebarTextSize: Math.max(12, Math.round(Kirigami.Theme.defaultFont.pixelSize))
    readonly property int sidebarSmallTextSize: Math.max(11, Math.round(sidebarTextSize * 0.86))

    // Sidebar pane: "main" = the home server's sessions, "roam" = roam peers.
    property string sidebarTab: "main"

    // True while startup is actively connecting / loading the session list;
    // drives the sidebar's "Loading sessions…" overlay.
    function startingUp() {
        const s = Mgr.status
        return s.startsWith("connecting") || s.startsWith("connected") || s === "loading…"
    }

    // Landing-page provider/model pickers, fed from the staging session's
    // config options (Mgr.config) — same shape ChatPage's header selectors use.
    function landingFindOption(id) {
        for (var i = 0; i < Mgr.config.length; i++)
            if (Mgr.config[i].id === id) return Mgr.config[i]
        return null
    }
    function landingProviderChoices() {
        const o = root.landingFindOption("provider")
        return o ? o.choices : []
    }
    function landingModelChoices() {
        const o = root.landingFindOption("model")
        return o ? o.choices : []
    }
    function landingOptionIndex(id) {
        const o = root.landingFindOption(id)
        if (!o) return -1
        const choices = o.choices
        for (let i = 0; i < choices.length; i++)
            if (choices[i].value === o.currentValue) return i
        return -1
    }

    Controls.SplitView {
        anchors.fill: parent
        orientation: Qt.Horizontal

        // ----- left sidebar: controls + session list -----
        // SplitView renders a native resize handle on the shared edge and sizes
        // this panel, so no manual border/separator (that drew a hard black
        // outline and sat off-alignment in non-native themes).
        Rectangle {
            SplitView.preferredWidth: 320
            SplitView.minimumWidth: 240
            SplitView.maximumWidth: 520
            SplitView.fillHeight: true
            color: Kirigami.Theme.alternateBackgroundColor

            ColumnLayout {
                anchors.fill: parent
                anchors.topMargin: Kirigami.Units.largeSpacing
                anchors.leftMargin: Kirigami.Units.largeSpacing
                anchors.rightMargin: Kirigami.Units.largeSpacing
                // The chat input reaches the window bottom; keep the sidebar
                // footer on that same baseline instead of lifting it by the
                // usual panel padding.
                anchors.bottomMargin: 0
                spacing: Kirigami.Units.smallSpacing * 1.25

                RowLayout {
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.smallSpacing
                    Controls.Button {
                        text: qsTr("Main")
                        checkable: true
                        checked: root.sidebarTab === "main"
                        Layout.fillWidth: true
                        font.weight: Font.DemiBold
                        font.pixelSize: root.sidebarTextSize
                        onClicked: {
                            root.sidebarTab = "main"
                            Mgr.setActiveTab("main")
                        }
                    }
                    Controls.Button {
                        text: qsTr("Roam")
                        checkable: true
                        checked: root.sidebarTab === "roam"
                        Layout.fillWidth: true
                        font.weight: Font.DemiBold
                        font.pixelSize: root.sidebarTextSize
                        onClicked: root.sidebarTab = "roam"
                    }
                }

                Item {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    visible: root.sidebarTab === "main"

                // Pinned sidebar chrome, not part of the list: because the
                // ListView below overlays the rail (overlay ScrollBar), this
                // row must sit above the list so the New-project button never
                // slides under the scrollbar when the list overflows.
                RowLayout {
                    id: sessionsHeader
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.top: parent.top
                    spacing: Kirigami.Units.smallSpacing
                    Controls.Label {
                        text: qsTr("Sessions")
                        font.weight: Font.DemiBold
                        font.pixelSize: root.sidebarTextSize
                        renderType: Text.NativeRendering
                        Layout.fillWidth: true
                    }
                    Controls.ToolButton {
                        text: qsTr("New project")
                        display: Controls.AbstractButton.IconOnly
                        icon.name: "project-development-new"
                        onClicked: newProjectDialog.open()
                        ToolTip.visible: hovered
                        ToolTip.text: qsTr("New project")
                    }
                }

                ListView {
                    id: sessionList
                    // Start below the pinned Sessions header instead of
                    // overlaying the rail, so the vertical ScrollBar (which
                    // overlays the viewport) spans only the list and cannot
                    // clip the New-project button above it.
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.top: sessionsHeader.bottom
                    anchors.topMargin: Kirigami.Units.smallSpacing * 2
                    anchors.bottom: parent.bottom
                    clip: true
                    model: Mgr.sessionsModel
                    spacing: Kirigami.Units.smallSpacing
                    // The model emits a flat tree: a top-level "Projects" section
                    // wrapping each project group, then "Chats", then remote
                    // peers, each collapsible group followed by its sessions. A
                    // single delegate branches on model.header and nest depth.
                    delegate: Item {
                        id: del
                        width: ListView.view.width
                        readonly property bool isHeader: header
                        readonly property bool isProject: isHeader && String(section).startsWith("proj:")
                        readonly property bool isPeer: isHeader && String(section).startsWith("peer:")
                        readonly property bool isProjects: isHeader && String(section) === "projects"
                        readonly property bool sel: !isHeader && sessionid === Mgr.currentSessionId
                        // The vertical ScrollBar overlays the viewport (same as the
                        // transcript's scrollbarReserve); the highlight must stop
                        // before it, or hover/selection paint underneath the bar.
                        readonly property bool scrollbarShown: sessionList.ScrollBar.vertical.visible
                        // The sidebar is a tree: "Projects" wraps each project, which
                        // wraps its sessions. `nest` is this row's depth under a
                        // collapsible group (0 = top level): project headers and
                        // peer/filed sessions sit one step in, project sessions two.
                        // Driven here (not in the model) so the row keeps its roles.
                        readonly property int nest: isHeader
                            ? (isProject ? 1 : 0)
                            : (String(section).startsWith("proj:") ? 2
                               : String(section).startsWith("peer:") ? 1 : 0)
                        height: isHeader ? headerRect.implicitHeight
                                         : Math.max(50, col.implicitHeight + Kirigami.Units.smallSpacing * 2.5)

                        // ---------- group header row ----------
                        Rectangle {
                            id: headerRect
                            visible: del.isHeader
                            width: parent.width
                            implicitHeight: Kirigami.Units.gridUnit * 2
                            radius: Kirigami.Units.smallSpacing
                            color: "transparent"
                            // The project highlight must never run under the overlay
                            // ScrollBar. Reserve the same gutter the row content
                            // already does (unconditionally — the scrollbar reserves
                            // this inset whether or not it is currently shown), so
                            // the highlight always stops short of the bar.
                            Rectangle {
                                anchors.fill: parent
                                anchors.rightMargin: Kirigami.Units.smallSpacing + Kirigami.Units.gridUnit * 1.75
                                radius: Kirigami.Units.smallSpacing
                                color: del.isProject ? Qt.rgba(Kirigami.Theme.highlightColor.r,
                                                               Kirigami.Theme.highlightColor.g,
                                                               Kirigami.Theme.highlightColor.b, 0.12)
                                                      : "transparent"
                            }
                            RowLayout {
                                anchors.fill: parent
                                anchors.leftMargin: Kirigami.Units.smallSpacing + del.nest * Kirigami.Units.gridUnit * 0.5
                                anchors.rightMargin: Kirigami.Units.smallSpacing + Kirigami.Units.gridUnit * 2.0
                                spacing: Kirigami.Units.smallSpacing

                                // Caret toggles the group open/collapsed.
                                Item {
                                    Layout.preferredWidth: 18
                                    Layout.preferredHeight: 18
                                    Kirigami.Icon {
                                        anchors.centerIn: parent
                                        source: collapsed ? "arrow-right" : "arrow-down"
                                        implicitWidth: 12
                                        implicitHeight: 12
                                    }
                                    MouseArea {
                                        anchors.fill: parent
                                        onClicked: Mgr.sessionsModel.toggleSection(section)
                                    }
                                }
                                Kirigami.Icon {
                                    source: del.isProject ? "folder" : del.isPeer ? "applications-internet" : ""
                                    implicitWidth: 16
                                    implicitHeight: 16
                                    visible: source.length > 0
                                }
                                Controls.Label {
                                    text: title
                                    font.weight: Font.DemiBold
                                    // Project names read bigger than session text.
                                    font.pixelSize: root.sidebarTextSize + 2
                                    color: Kirigami.Theme.textColor
                                    renderType: Text.NativeRendering
                                    elide: Text.ElideRight
                                    Layout.fillWidth: true
                                }
                                Controls.Label {
                                    text: count + " " + qsTr("chats")
                                    color: Kirigami.Theme.disabledTextColor
                                    font.pixelSize: root.sidebarSmallTextSize
                                    renderType: Text.NativeRendering
                                }
                            }
                            // Left-click a project opens its summary; right-click
                            // shows the project menu.
                            MouseArea {
                                id: headerMouse
                                anchors.left: parent.left
                                anchors.right: parent.right
                                anchors.top: parent.top
                                anchors.bottom: parent.bottom
                                anchors.leftMargin: Kirigami.Units.gridUnit * 1.75
                                hoverEnabled: true
                                acceptedButtons: Qt.LeftButton | Qt.RightButton
                                onClicked: (e) => {
                                    if (e.button === Qt.RightButton) {
                                        if (del.isProject) {
                                            projectMenu.targetId = section.substring(5)
                                            projectMenu.targetName = title
                                            var pos = headerMouse.mapToItem(root.contentItem, e.x, e.y)
                                            projectMenu.popup(pos.x, pos.y)
                                        }
                                    } else if (del.isProject) {
                                        projectDialog.openFor(section.substring(5), title)
                                    }
                                }
                            }
                        }

                        // ---------- session row ----------
                        Rectangle {
                            visible: !del.isHeader && del.nest > 0
                            anchors.left: parent.left
                            anchors.right: parent.right
                            anchors.top: parent.top
                            anchors.bottom: parent.bottom
                            anchors.leftMargin: Kirigami.Units.smallSpacing * 1.5
                            anchors.rightMargin: Kirigami.Units.smallSpacing + Kirigami.Units.gridUnit * 1.5
                            radius: Kirigami.Units.smallSpacing
                            color: Kirigami.Theme.alternateBackgroundColor
                            opacity: 0.34
                        }

                        // Continuous guide for sessions nested under a group.
                        Rectangle {
                            visible: !del.isHeader && del.nest > 0
                            width: 2
                            radius: 1
                            anchors.left: parent.left
                            anchors.top: parent.top
                            anchors.bottom: parent.bottom
                            anchors.leftMargin: Kirigami.Units.smallSpacing
                            color: Kirigami.Theme.highlightColor
                            opacity: 0.45
                        }

                        Rectangle {
                            visible: !del.isHeader
                            anchors.fill: parent
                            anchors.rightMargin: del.scrollbarShown
                                ? Kirigami.Units.smallSpacing + Kirigami.Units.gridUnit * 1.5 : 0
                            radius: Kirigami.Units.smallSpacing
                            color: del.sel ? Kirigami.Theme.highlightColor
                                           : mouse.containsMouse ? Kirigami.Theme.backgroundColor : "transparent"
                            opacity: del.sel ? 0.28 : mouse.containsMouse ? 0.55 : 0
                        }

                        Rectangle {
                            visible: !del.isHeader && del.sel
                            width: 3
                            radius: 2
                            color: Kirigami.Theme.highlightColor
                            anchors.left: parent.left
                            anchors.top: parent.top
                            anchors.bottom: parent.bottom
                        }

                        Column {
                            id: col
                            visible: !del.isHeader
                            anchors.left: parent.left
                            anchors.right: parent.right
                            anchors.verticalCenter: parent.verticalCenter
                            anchors.leftMargin: Kirigami.Units.smallSpacing + del.nest * Kirigami.Units.gridUnit * 0.5
                            anchors.rightMargin: Kirigami.Units.smallSpacing + Kirigami.Units.gridUnit * 1.5
                            spacing: 3
                            Controls.Label {
                                text: title
                                width: parent.width
                                elide: Text.ElideRight
                                font.weight: del.sel ? Font.DemiBold : Font.Normal
                                font.pixelSize: root.sidebarTextSize
                                renderType: Text.NativeRendering
                            }
                            Controls.Label {
                                // Header rows lack the snippet/messagecount roles — guard
                                // so the binding doesn't throw while the column is hidden.
                                text: snippet && snippet.length > 0 ? snippet : (messagecount || 0) + " msg"
                                width: parent.width
                                elide: Text.ElideRight
                                color: Kirigami.Theme.disabledTextColor
                                visible: text.length > 0
                                font.pixelSize: root.sidebarSmallTextSize
                                renderType: Text.NativeRendering
                            }
                        }

                        MouseArea {
                            id: mouse
                            visible: !del.isHeader
                            anchors.fill: parent
                            hoverEnabled: true
                            acceptedButtons: Qt.LeftButton | Qt.RightButton
                            onClicked: (e) => {
                                if (e.button === Qt.RightButton) {
                                    sessionMenu.targetSessionId = sessionid
                                    sessionMenu.targetTitle = title
                                    // popup(x,y) is relative to the menu's parent
                                    // (the window), so map from the MouseArea's local coords.
                                    var pos = mouse.mapToItem(root.contentItem, e.x, e.y)
                                    sessionMenu.popup(pos.x, pos.y)
                                } else {
                                    Mgr.openSession(sessionid)
                                }
                            }
                        }
                    }
                    ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }
                }

                // Overlay shown while the first session/list is still in flight.
                Column {
                    anchors.centerIn: parent
                    visible: root.startingUp() && Mgr.sessions.length === 0
                    spacing: Kirigami.Units.smallSpacing
                    BusyIndicator {
                        anchors.horizontalCenter: parent.horizontalCenter
                        running: parent.visible
                    }
                    Controls.Label {
                        text: qsTr("Loading sessions…")
                        anchors.horizontalCenter: parent.horizontalCenter
                        color: Kirigami.Theme.disabledTextColor
                        font.pixelSize: root.sidebarSmallTextSize
                        renderType: Text.NativeRendering
                    }
                }
                } // end session list wrapper

                // ---------- Roam pane: endpoints + their sessions ----------
                Item {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    visible: root.sidebarTab === "roam"

                    ColumnLayout {
                        anchors.fill: parent
                        spacing: Kirigami.Units.smallSpacing

                        // Add-peer form (paste a card; the host must accept this
                        // device's key — shown below — via `roam peers accept`).
                        Controls.TextField {
                            id: roamLabelField
                            Layout.fillWidth: true
                            placeholderText: qsTr("Peer label (e.g. workstation)")
                            onAccepted: roamConnectClicked()
                        }
                        Controls.TextField {
                            id: roamCardField
                            Layout.fillWidth: true
                            placeholderText: qsTr("Paste goose+roam:// card")
                            onAccepted: roamConnectClicked()
                        }
                        function roamConnectClicked() {
                            const label = roamLabelField.text.trim()
                            const card = roamCardField.text.trim()
                            if (!label || !card) return
                            Mgr.connectRoam(card, label)
                            roamCardField.text = ""
                            root.sidebarTab = "roam"
                        }
                        Controls.Button {
                            text: qsTr("Connect roam peer")
                            icon.name: "network-connect"
                            Layout.fillWidth: true
                            onClicked: parent.roamConnectClicked()
                        }
                        // Shareable roam connection card. A host pastes the FULL
                        // card (`goose+roam://…`) into `roam peers accept`, not the
                        // public key — make it one-click copyable.
                        RowLayout {
                            Layout.fillWidth: true
                            visible: cardField.text.length > 0
                            spacing: Kirigami.Units.smallSpacing
                            Controls.TextField {
                                id: cardField
                                Layout.fillWidth: true
                                readonly property string fullCard: { Mgr.roamIdentity(); return Mgr.roamCard() }
                                text: fullCard
                                font.pixelSize: root.sidebarSmallTextSize
                                wrapMode: Text.NoWrap
                                leftPadding: 8
                                rightPadding: 8
                            }
                            Controls.ToolButton {
                                text: qsTr("Copy card")
                                display: Controls.AbstractButton.IconOnly
                                icon.name: "edit-copy"
                                onClicked: Mgr.copyToClipboard(cardField.fullCard)
                                ToolTip.visible: hovered
                                ToolTip.text: qsTr("Copy this device's roam card")
                            }
                        }

                        ListView {
                            id: roamList
                            Layout.fillWidth: true
                            Layout.fillHeight: true
                            clip: true
                            spacing: Kirigami.Units.smallSpacing
                            model: Mgr.roamModel
                            // Endpoint header rows + session rows (drop-down).
                            delegate: Item {
                                id: rdel
                                width: ListView.view.width
                                readonly property bool isHeader: rowType === "header"
                                readonly property int rowHeight: isHeader
                                    ? Math.max(34, roamHeader.implicitHeight + Kirigami.Units.smallSpacing)
                                    : Math.max(44, rcol.implicitHeight + Kirigami.Units.smallSpacing * 2)
                                height: rowHeight

                                Rectangle {
                                    id: roamHeader
                                    visible: rdel.isHeader
                                    anchors.fill: parent
                                    radius: Kirigami.Units.smallSpacing
                                    color: mouse.containsMouse
                                        ? Kirigami.Theme.highlightColor : "transparent"
                                    opacity: mouse.containsMouse ? 0.3 : 1
                                    MouseArea {
                                        id: rhmouse
                                        anchors.fill: parent
                                        hoverEnabled: true
                                        onClicked: Mgr.toggleRoamPeer(label)
                                    }
                                    RowLayout {
                                        anchors.fill: parent
                                        anchors.leftMargin: Kirigami.Units.smallSpacing
                                        anchors.rightMargin: Kirigami.Units.smallSpacing
                                        spacing: Kirigami.Units.smallSpacing
                                        Controls.Label {
                                            text: label
                                            font.weight: Font.DemiBold
                                            font.pixelSize: root.sidebarTextSize
                                            Layout.fillWidth: true
                                            elide: Text.ElideRight
                                            renderType: Text.NativeRendering
                                        }
                                        Rectangle {
                                            width: 8; height: 8; radius: 4
                                            color: connected
                                                ? (Kirigami.Theme.colorScheme === Kirigami.Theme.ColorScheme.Dark
                                                   ? Theme.semantic.dark.status.online
                                                   : Theme.semantic.light.status.online)
                                                : Kirigami.Theme.negativeTextColor
                                            Layout.alignment: Qt.AlignVCenter
                                        }
                                        Controls.Label {
                                            text: status
                                            color: Kirigami.Theme.disabledTextColor
                                            font.pixelSize: root.sidebarSmallTextSize
                                            Layout.maximumWidth: 130
                                            elide: Text.ElideRight
                                            renderType: Text.NativeRendering
                                            // The row is narrow, so the raw dial error is
                                            // elided — hover to read the FULL status (the
                                            // only place the real failure cause is visible).
                                            ToolTip.visible: hovered && status.length > 0
                                            ToolTip.text: status
                                        }
                                        Controls.ToolButton {
                                            text: qsTr("New chat")
                                            display: Controls.AbstractButton.TextBesideIcon
                                            icon.name: "document-new"
                                            font.pixelSize: root.sidebarSmallTextSize
                                            ToolTip.visible: hovered
                                            ToolTip.text: qsTr("New chat on this peer — long-press to choose a working directory")
                                            // Long-press opens the cwd dialog; the
                                            // hold timer guards the trailing click
                                            // so a hold never also starts a chat.
                                            property bool held: false
                                            Timer {
                                                id: holdTimer
                                                interval: 500
                                                onTriggered: {
                                                    parent.held = true
                                                    roamNewChatDialog.targetLabel = label
                                                    roamNewChatDialog.suggestedCwd = Mgr.workingDir
                                                    roamNewChatDialog.open()
                                                }
                                            }
                                            onPressed: { held = false; holdTimer.restart() }
                                            onReleased: holdTimer.stop()
                                            onCanceled: holdTimer.stop()
                                            onClicked: { if (!held) Mgr.newRoamSession(label) }
                                        }
                                        Controls.ToolButton {
                                            text: qsTr("Remove")
                                            display: Controls.AbstractButton.IconOnly
                                            icon.name: "edit-delete"
                                            onClicked: Mgr.disconnectRoam(label)
                                            ToolTip.visible: hovered
                                            ToolTip.text: qsTr("Disconnect this peer")
                                        }
                                    }
                                }

                                // Session row under the peer.
                                Rectangle {
                                    id: rrow
                                    visible: !rdel.isHeader
                                    anchors.fill: parent
                                    radius: Kirigami.Units.smallSpacing
                                    color: rmouse.containsMouse
                                        ? Kirigami.Theme.highlightColor : "transparent"
                                    opacity: rmouse.containsMouse ? 0.2 : 1
                                    MouseArea {
                                        id: rmouse
                                        anchors.fill: parent
                                        hoverEnabled: true
                                        onClicked: Mgr.openRoamSession(label, sessionId, cwd)
                                    }
                                    Column {
                                        anchors.left: parent.left
                                        anchors.right: parent.right
                                        anchors.verticalCenter: parent.verticalCenter
                                        anchors.leftMargin: Kirigami.Units.gridUnit
                                        anchors.rightMargin: Kirigami.Units.smallSpacing
                                        spacing: 2
                                        Controls.Label {
                                            text: title
                                            width: parent.width
                                            elide: Text.ElideRight
                                            font.pixelSize: root.sidebarTextSize
                                            renderType: Text.NativeRendering
                                        }
                                        Controls.Label {
                                            text: snippet && snippet.length > 0 ? snippet
                                                : (messageCount || 0) + " msg"
                                            width: parent.width
                                            elide: Text.ElideRight
                                            color: Kirigami.Theme.disabledTextColor
                                            visible: text.length > 0
                                            font.pixelSize: root.sidebarSmallTextSize
                                            renderType: Text.NativeRendering
                                        }
                                    }
                                }
                            }
                            ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }
                        }
                    }
                }

                Kirigami.Separator { Layout.fillWidth: true }

                RowLayout {
                    Layout.fillWidth: true
                    visible: Mgr.online
                    spacing: Kirigami.Units.smallSpacing
                    // Equal thirds: Skills, Recipes, Scheduler.
                    Controls.Button {
                        text: qsTr("Skills")
                        icon.name: "document-properties"
                        Layout.fillWidth: true
                        onClicked: skillsDialog.open()
                    }
                    Controls.Button {
                        text: qsTr("Recipes")
                        icon.name: "view-list-details"
                        Layout.fillWidth: true
                        onClicked: recipesDialog.open()
                    }
                    Controls.Button {
                        text: qsTr("Scheduler")
                        icon.name: "appointment-new"
                        Layout.fillWidth: true
                        onClicked: schedulerDialog.open()
                    }
                }

                    Controls.Button {
                        text: qsTr("New chat")
                        icon.name: "document-new"
                        Layout.fillWidth: true
                        onClicked: Mgr.newChat()
                    }

                // Connect lives here; Disconnect lives in Settings.
                Controls.Button {
                    visible: !Mgr.online
                    text: qsTr("Connect")
                    icon.name: "network-connect"
                    Layout.fillWidth: true
                    onClicked: connectDialog.open()
                }

                    Controls.Button {
                        text: qsTr("Settings")
                    icon.name: "settings-configure"
                    Layout.fillWidth: true
                    onClicked: settingsDialog.open()
                }

                Controls.Label {
                    text: Mgr.status
                    color: Kirigami.Theme.disabledTextColor
                    font.pixelSize: root.sidebarSmallTextSize
                    renderType: Text.NativeRendering
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                }
            }
        }

        // ----- chat area -----
        Item {
            id: chatArea
            SplitView.fillWidth: true
            SplitView.fillHeight: true
            clip: true
            ChatPage {
                id: chatPage
                anchors.fill: parent
                visible: !Mgr.landingPage
                onToolsOpenRequested: toolsPanel.drawerOpen = !toolsPanel.drawerOpen
            }

            // Landing page: shown until a conversation is committed — cold
            // start, a disconnect, or after deleting the open chat. Cold start
            // connects and forms a fresh (empty) staging session, so the
            // provider/model pickers below have real choices; New chat steps
            // into that session with the chosen model.
            ColumnLayout {
                anchors.centerIn: parent
                spacing: Kirigami.Units.largeSpacing
                visible: Mgr.landingPage

                BusyIndicator {
                    Layout.alignment: Qt.AlignHCenter
                    visible: root.startingUp()
                    running: root.startingUp()
                }
                Kirigami.Heading {
                    text: qsTr("Welcome to Grouse")
                    level: 2
                    horizontalAlignment: Text.AlignHCenter
                    Layout.alignment: Qt.AlignHCenter
                }
                Controls.Label {
                    text: root.startingUp() ? qsTr("Connecting…")
                          : Mgr.online ? qsTr("Start a new conversation, or pick one from the sidebar.")
                          : qsTr("Connect to your goose server to get started.")
                    horizontalAlignment: Text.AlignHCenter
                    Layout.alignment: Qt.AlignHCenter
                    color: Kirigami.Theme.disabledTextColor
                }
                Controls.Button {
                    visible: Mgr.online
                    text: qsTr("New chat")
                    icon.name: "document-new"
                    Layout.alignment: Qt.AlignHCenter
                    onClicked: Mgr.beginChat()
                }
                // Provider + model pickers for the chat about to be started.
                // Choices come from the staging session's config (Mgr.config).
                Controls.Label {
                    visible: Mgr.online
                    text: qsTr("Provider")
                    Layout.alignment: Qt.AlignHCenter
                    color: Kirigami.Theme.disabledTextColor
                    font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
                }
                Controls.ComboBox {
                    visible: Mgr.online
                    Layout.alignment: Qt.AlignHCenter
                    implicitWidth: 280
                    textRole: "name"
                    // Without a valueRole, currentValue is the model item (a JS
                    // object), not the value string — provider switches silently
                    // sent "[object V4ReferenceObject]" to the server.
                    valueRole: "value"
                    model: root.landingProviderChoices()
                    currentIndex: root.landingOptionIndex("provider")
                    onActivated: Mgr.setConfigOption("provider", currentValue)
                }
                Controls.Label {
                    visible: Mgr.online
                    text: qsTr("Model")
                    Layout.alignment: Qt.AlignHCenter
                    color: Kirigami.Theme.disabledTextColor
                    font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
                }
                Controls.ComboBox {
                    visible: Mgr.online
                    Layout.alignment: Qt.AlignHCenter
                    implicitWidth: 280
                    textRole: "name"
                    valueRole: "value"
                    model: root.landingModelChoices()
                    currentIndex: root.landingOptionIndex("model")
                    onActivated: Mgr.setConfigOption("model", currentValue)
                }
                Controls.Button {
                    visible: !Mgr.online
                    text: qsTr("Connect")
                    icon.name: "network-connect"
                    Layout.alignment: Qt.AlignHCenter
                    onClicked: connectDialog.open()
                }
            }
        }
    }

    // Native KDE drawer slides the per-session tools panel in from the right.
    // It must be a DIRECT child of the window (not nested in a layout) so its
    // DrawerHandle can attach to the window overlay.
    Kirigami.OverlayDrawer {
        id: toolsPanel
        edge: Qt.RightEdge
        // Keep this as a non-modal drawer, but explicitly close it at startup.
        // Older Kirigami versions expose drawerOpen more consistently than Popup-only props.
        modal: false
        drawerOpen: false
        width: 340
        padding: Kirigami.Units.smallSpacing
        onDrawerOpenChanged: {
            if (drawerOpen) {
                Mgr.refreshToolGroups()
            }
        }

        ColumnLayout {
            anchors.fill: parent
            spacing: Kirigami.Units.smallSpacing

            RowLayout {
                Layout.fillWidth: true
                spacing: Kirigami.Units.smallSpacing
                Controls.Label {
                    id: toolsHeaderLabel
                    text: Mgr.tools.length + qsTr(" tools")
                    font.bold: true
                    Layout.fillWidth: true
                }
                Controls.Button {
                    flat: true
                    display: Controls.AbstractButton.IconOnly
                    icon.name: "dialog-close"
                    onClicked: toolsPanel.drawerOpen = false
                }
            }
            Kirigami.Separator { Layout.fillWidth: true }

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
                    width: ListView.view.width
                    spacing: 2
                    property bool expanded: false
                    readonly property string gName: modelData.name
                    readonly property bool gAttrib: modelData.attrib
                    readonly property bool gEnabled: modelData.enabled
                    readonly property bool gKnown: modelData.known

                    RowLayout {
                        width: parent.width
                        spacing: Kirigami.Units.smallSpacing
                        Controls.Button {
                            flat: true
                            display: Controls.AbstractButton.IconOnly
                            icon.name: tdel.expanded ? "arrow-down" : "arrow-right"
                            implicitWidth: Kirigami.Units.gridUnit
                            Layout.preferredWidth: Kirigami.Units.gridUnit
                            Layout.preferredHeight: Kirigami.Units.gridUnit * 1.25
                            onClicked: {
                                tdel.expanded = !tdel.expanded
                                if (tdel.expanded && tdel.gAttrib && !tdel.gKnown)
                                    Mgr.discoverToolGroup(tdel.gName)
                            }
                        }
                        Controls.Switch {
                            Layout.preferredWidth: Kirigami.Units.gridUnit * 2.5
                            checked: tdel.gEnabled
                            onToggled: Mgr.setSessionExtensionEnabled(tdel.gName, checked)
                        }
                        Controls.Label {
                            text: tdel.gName
                            elide: Text.ElideRight
                            Layout.fillWidth: true
                            font.bold: true
                        }
                        Controls.Label {
                            text: tdel.gAttrib ? (tdel.gKnown ? tdel.expanded ? "" : modelData.tools.length + qsTr(" tools") : qsTr("…")) : ""
                            opacity: 0.6
                            horizontalAlignment: Text.AlignRight
                        }
                    }

                    Column {
                        id: toolsCol
                        width: parent.width
                        visible: tdel.expanded
                        Repeater {
                            model: tdel.gAttrib ? modelData.tools : []
                            delegate: RowLayout {
                                width: parent.width
                                spacing: Kirigami.Units.smallSpacing
                                Controls.Switch {
                                    Layout.preferredWidth: Kirigami.Units.gridUnit * 2.5
                                    checked: modelData.on
                                    enabled: tdel.gEnabled
                                    onToggled: Mgr.setSessionToolEnabled(tdel.gName, modelData.name, checked)
                                }
                                Controls.Label {
                                    text: modelData.name
                                    elide: Text.ElideRight
                                    Layout.fillWidth: true
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Connections {
        target: Mgr
        function onPermissionRequested() {
            permissionDialog.pendingToolCallId = Mgr.permissionToolCallId()
            permissionDialog.appTitle = Mgr.permissionTitle()
            permissionDialog.open()
        }
    }

    // Clicking a project header shows its summary + root.
    ProjectDialog {
        id: projectDialog
    }

    ConnectDialog {
        id: connectDialog
        onAccepted: Mgr.connectToServer()
    }

    SettingsDialog {
        id: settingsDialog
        onOpenProviders: providersDialog.open()
        onOpenGlobalTools: globalExtensionsDialog.open()
    }

    ProvidersDialog {
        id: providersDialog
    }

    SkillsDialog {
        id: skillsDialog
    }

    RecipesDialog {
        id: recipesDialog
    }

    SchedulerDialog {
        id: schedulerDialog
    }

    GlobalExtensionsDialog {
        id: globalExtensionsDialog
    }

    // Save-dialog for exporting a session's JSON (the URL-to-path strip mirrors
    // the Android client; Mgr.status reports "exported to …" when it lands).
    FileDialog {
        id: exportDialog
        title: qsTr("Export session")
        fileMode: FileDialog.SaveFile
        defaultSuffix: "json"
        nameFilters: [qsTr("JSON files (*.json)")]
        onAccepted: {
            var p = exportDialog.selectedFile.toString()
            if (p.startsWith("file://"))
                p = p.substring(7)
            Mgr.exportSessionTo(sessionMenu.targetSessionId, p)
        }
    }

    // Right-click actions on a session in the sidebar (per-session rename /
    // archive / delete over ACP, mirroring grouse).
    Menu {
        id: sessionMenu
        property string targetSessionId
        property string targetTitle
        MenuItem {
            text: qsTr("Rename…")
            icon.name: "edit-rename"
            onTriggered: {
                renameDialog.sessionId = sessionMenu.targetSessionId
                renameDialog.sessionName = sessionMenu.targetTitle
                renameDialog.open()
            }
        }
        MenuItem {
            text: qsTr("Archive")
            icon.name: "content-loading"
            onTriggered: Mgr.archiveSession(sessionMenu.targetSessionId)
        }
        MenuItem {
            text: qsTr("Move to project…")
            icon.name: "folder-move"
            visible: !sessionMenu.targetSessionId.startsWith("roam:")
            onTriggered: moveToProjectDialog.open()
        }
        MenuItem {
            text: qsTr("Export…")
            icon.name: "document-export"
            onTriggered: {
                var fname = (sessionMenu.targetTitle.replace(/[\\\/:*?"<>|]/g, "_") || "session") + ".json"
                var dir = (Mgr.workingDir || "").replace(/\/$/, "")
                exportDialog.currentFile = Qt.url("file://" + dir + "/" + fname)
                exportDialog.open()
            }
        }
        MenuSeparator {}
        MenuItem {
            text: qsTr("Delete")
            icon.name: "edit-delete"
            onTriggered: deleteConfirm.dialogOpen()
        }
    }

    // Right-click actions on a project header.
    Menu {
        id: projectMenu
        property string targetId
        property string targetName
        MenuItem {
            text: qsTr("New chat in this project")
            icon.name: "document-new"
            onTriggered: Mgr.newChatInProject(projectMenu.targetId)
        }
        MenuSeparator {}
        MenuItem {
            text: qsTr("Delete project…")
            icon.name: "edit-delete"
            onTriggered: deleteProjectConfirm.dialogOpen()
        }
    }

    // Move the selected session into a project (or back to "Chats" / unfiled).
    Controls.Dialog {
        id: moveToProjectDialog
        title: qsTr("Move to project")
        modal: true
        standardButtons: Controls.Dialog.Ok | Controls.Dialog.Cancel
        onAccepted: Mgr.moveSessionToProject(sessionMenu.targetSessionId, moveProjectCombo.currentValue)
        contentItem: ColumnLayout {
            spacing: Kirigami.Units.smallSpacing
            Controls.Label { text: qsTr("Project:") }
            Controls.ComboBox {
                id: moveProjectCombo
                Layout.fillWidth: true
                textRole: "name"
                valueRole: "id"
                model: ListModel {
                    Component.onCompleted: {
                        append({ id: "", name: qsTr("Chats (no project)") })
                        for (var i = 0; i < Mgr.projects.length; i++)
                            append({ id: Mgr.projects[i].id, name: Mgr.projects[i].name })
                    }
                }
            }
        }
    }

    // Create a project (validated: lowercase letters, digits, hyphens).
    Controls.Dialog {
        id: newProjectDialog
        title: qsTr("New project")
        modal: true
        standardButtons: Controls.Dialog.Ok | Controls.Dialog.Cancel
        onAccepted: Mgr.createProject(projectNameField.text)
        contentItem: ColumnLayout {
            spacing: Kirigami.Units.smallSpacing
            Controls.Label { text: qsTr("Name (lowercase letters, digits, hyphens):") }
            Controls.TextField {
                id: projectNameField
                Layout.fillWidth: true
                placeholderText: qsTr("e.g. cooking")
            }
        }
    }

    // New chat on a roam peer in a chosen working dir (long-press on that
    // peer's "+ New chat"). goose natively honors the cwd on session/new; a
    // blank entry falls back to the config working dir.
    Controls.Dialog {
        id: roamNewChatDialog
        title: qsTr("New chat on peer")
        modal: true
        standardButtons: Controls.Dialog.Ok | Controls.Dialog.Cancel
        property string targetLabel: ""
        property string suggestedCwd: ""
        onAccepted: Mgr.newRoamSessionIn(targetLabel, roamCwdField.text)
        onOpened: {
            roamCwdField.text = suggestedCwd
            roamCwdField.selectAll()
            roamCwdField.forceActiveFocus()
        }
        contentItem: ColumnLayout {
            spacing: Kirigami.Units.smallSpacing
            Controls.Label { text: qsTr("Working directory (absolute path):") }
            Controls.TextField {
                id: roamCwdField
                Layout.fillWidth: true
                placeholderText: qsTr("e.g. /home/colin/projects/foo")
                onAccepted: roamNewChatDialog.accept()
            }
        }
    }

    // Confirmation before deleting a project (its chats move to Unfiled).
    Controls.Dialog {
        id: deleteProjectConfirm
        title: qsTr("Delete project")
        modal: true
        standardButtons: Controls.Dialog.Ok | Controls.Dialog.Cancel
        function dialogOpen() {
            deleteProjectLabel.text = qsTr("Delete project \"%1\"? Its chats move to Unfiled.").arg(projectMenu.targetName)
            deleteProjectConfirm.open()
        }
        contentItem: ColumnLayout {
            spacing: Kirigami.Units.smallSpacing
            Controls.Label {
                id: deleteProjectLabel
                wrapMode: Text.Wrap
                Layout.fillWidth: true
            }
        }
        onAccepted: Mgr.deleteProject(projectMenu.targetId)
    }

    // Right-click actions on a recipe.
    Menu {
        id: recipeMenu
        property string targetId
        property string targetTitle
        MenuItem {
            text: qsTr("Start session")
            icon.name: "media-playback-start"
            onTriggered: Mgr.runRecipe(recipeMenu.targetId)
        }
        MenuItem {
            text: qsTr("Schedule…")
            icon.name: "appointment-new"
            onTriggered: {
                scheduleDialog.recipeId = recipeMenu.targetId
                scheduleDialog.recipeTitle = recipeMenu.targetTitle
                scheduleDialog.open()
            }
        }
        MenuItem {
            text: qsTr("Unschedule")
            icon.name: "edit-delete"
            onTriggered: Mgr.scheduleRecipe(recipeMenu.targetId, "")
        }
        MenuSeparator {}
        MenuItem {
            text: qsTr("Delete")
            icon.name: "edit-delete"
            onTriggered: deleteRecipeConfirm.dialogOpen()
        }
    }

    // Schedule a recipe with a cron expression (6-field, second minute hour day
    // month weekday). Blank unschedules.
    Controls.Dialog {
        id: scheduleDialog
        property string recipeId
        property string recipeTitle
        title: qsTr("Schedule \"%1\"").arg(recipeTitle)
        modal: true
        standardButtons: Controls.Dialog.Ok | Controls.Dialog.Cancel
        onAccepted: Mgr.scheduleRecipe(scheduleDialog.recipeId, scheduleCronField.text.trim())
        contentItem: ColumnLayout {
            spacing: Kirigami.Units.smallSpacing
            Controls.Label { text: qsTr("Cron expression (6-field: sec min hour day month weekday):") }
            Controls.TextField {
                id: scheduleCronField
                Layout.fillWidth: true
                placeholderText: qsTr("e.g. 0 9 * * * *")
            }
        }
    }

    // Confirmation before deleting a recipe (its schedule, if any, stops working).
    Controls.Dialog {
        id: deleteRecipeConfirm
        title: qsTr("Delete recipe")
        modal: true
        standardButtons: Controls.Dialog.Ok | Controls.Dialog.Cancel
        function dialogOpen() {
            deleteRecipeLabel.text = qsTr("Delete recipe \"%1\"? Its schedule (if any) stops working.").arg(recipeMenu.targetTitle)
            deleteRecipeConfirm.open()
        }
        contentItem: ColumnLayout {
            spacing: Kirigami.Units.smallSpacing
            Controls.Label {
                id: deleteRecipeLabel
                wrapMode: Text.Wrap
                Layout.fillWidth: true
            }
        }
        onAccepted: Mgr.deleteRecipe(recipeMenu.targetId)
    }

    // Reuse the connect/rename-style dialog: a small modal prompt for the new title.
    Controls.Dialog {
        id: renameDialog
        property string sessionId
        property string sessionName
        title: qsTr("Rename session")
        modal: true
        standardButtons: Controls.Dialog.Ok | Controls.Dialog.Cancel
        onOpened: renameField.text = renameDialog.sessionName
        onAccepted: Mgr.renameSession(renameDialog.sessionId, renameField.text)
        contentItem: ColumnLayout {
            spacing: Kirigami.Units.smallSpacing
            Controls.Label { text: qsTr("New name:") }
            Controls.TextField {
                id: renameField
                Layout.fillWidth: true
            }
        }
    }

    // Confirmation before an irreversible delete.
    Controls.Dialog {
        id: deleteConfirm
        title: qsTr("Delete session")
        modal: true
        standardButtons: Controls.Dialog.Ok | Controls.Dialog.Cancel
        function dialogOpen() {
            deleteLabel.text = qsTr("Delete \"%1\" permanently? This cannot be undone.").arg(sessionMenu.targetTitle)
            deleteConfirm.open()
        }
        contentItem: ColumnLayout {
            spacing: Kirigami.Units.smallSpacing
            Controls.Label {
                id: deleteLabel
                wrapMode: Text.Wrap
                Layout.fillWidth: true
            }
        }
        onAccepted: Mgr.deleteSession(sessionMenu.targetSessionId)
    }

    PermissionDialog {
        id: permissionDialog
    }

    onClosing: {
        if (Mgr.online)
            Mgr.disconnect()
    }
}
