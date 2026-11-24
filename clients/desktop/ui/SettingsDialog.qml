import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// KDE-native settings dialog: fields in a Kirigami.FormLayout, persisted via
// QSettings through Mgr (which owns the QSettings store). Global tools live
// one level deeper — reached from here, not from the sidebar.
Controls.Dialog {
    id: dialog
    title: qsTr("Settings")
    modal: true
    standardButtons: Controls.Dialog.Close
    closePolicy: Controls.Popup.CloseOnEscape
    // QQC2 Dialog does not infer a usable width from the embedded FormLayout/
    // ScrollView; the form's own implicit size is exposed here and then bounded
    // by the window. This keeps long labels readable on a 4K screen without
    // letting the dialog outgrow a laptop display.
    implicitWidth: Math.min(Kirigami.Units.gridUnit * 42,
                            Math.max(Kirigami.Units.gridUnit * 30,
                                     contentItem.implicitWidth + leftPadding + rightPadding))
    implicitHeight: Math.min(Kirigami.Units.gridUnit * 44,
                             parent && parent.Window.window
                                 ? parent.Window.window.height * 0.85
                                 : Kirigami.Units.gridUnit * 44)
    width: Math.min(implicitWidth,
                    parent && parent.Window.window
                        ? parent.Window.window.width * 0.9
                        : implicitWidth)
    height: implicitHeight

    signal openGlobalTools()
    signal openTurnLog()
    signal openGooseConfig()

    Component.onCompleted: reload()
    function reload() {
        hostField.text = Mgr.host
        portField.text = Mgr.port
        tlsBox.checked = Mgr.useTls
        insecureTlsBox.checked = Mgr.acceptInvalidCerts
        keyField.text = Mgr.secretKey
        cwdField.text = Mgr.workingDir
        autoBox.checked = Mgr.autoConnectEnabled
        notifyBox.checked = Mgr.notificationsEnabled
        configuredOnlyBox.checked = Mgr.configuredProvidersOnly
        pushBox.checked = Push.enabled
        roamBox.checked = Mgr.roamEnabled
        runSummariesBox.checked = Mgr.runSummaries
        rescaleBox.checked = Mgr.rescaleImages
        debugBox.checked = Mgr.debugTools
        advisorBox.checked = Mgr.advisor
        dialog.testMessage = ""
    }
    onOpened: reload()
    // Persist everything up front so closing is always safe.
    function commit() {
        Mgr.host = hostField.text
        Mgr.port = portField.text
        Mgr.useTls = tlsBox.checked
        Mgr.acceptInvalidCerts = insecureTlsBox.checked
        Mgr.secretKey = keyField.text
        Mgr.workingDir = cwdField.text
        Mgr.autoConnectEnabled = autoBox.checked
        Mgr.notificationsEnabled = notifyBox.checked
        Mgr.configuredProvidersOnly = configuredOnlyBox.checked
        Mgr.roamEnabled = roamBox.checked
        Mgr.runSummaries = runSummariesBox.checked
        Mgr.rescaleImages = rescaleBox.checked
        Mgr.debugTools = debugBox.checked
        Mgr.advisor = advisorBox.checked
    }
    onClosed: commit()

    property bool testOk: false
    property string testMessage: ""
    property bool testing: false

    Connections {
        target: Mgr
        function onConnectionTested(ok, message) {
            dialog.testing = false
            dialog.testOk = ok
            dialog.testMessage = message
        }
    }

    // The form outgrew the screen once Dev landed: a Dialog sizes itself to its
    // content, so the last sections (Features, Dev, and the Disconnect/Global tools
    // row) were below the window edge with no way to reach them. Scrolling the form
    // and capping the visible height is the whole fix — the dialog stays centred and
    // the sections below the fold are a scroll away instead of unreachable.
    //
    // 80% of the window, not a fixed pixel height: the same dialog has to fit a 13"
    // laptop and a 4K monitor, and a fixed cap is wrong on one of them.
    // A Dialog is a Popup, not an Item, so `Window.window` is not available here —
    // Qt warns ("Window.window does only support types deriving from Item") and the
    // cap fell through to the fixed fallback on every screen. The attached property
    // belongs on the popup's parent item, which is the window's contentItem.
    readonly property real maxFormHeight: parent && parent.Window.window
        ? parent.Window.window.height * 0.8
        : Kirigami.Units.gridUnit * 40

    contentItem: ColumnLayout {
        spacing: Kirigami.Units.smallSpacing
        // The form reports its own natural width; the dialog adds padding and
        // clamps it. The old literal 520 clipped newly added long labels.
        implicitWidth: form.implicitWidth + Kirigami.Units.gridUnit * 2

        Controls.ScrollView {
            id: scroll
            Layout.fillWidth: true
            // The form's own height until it exceeds the cap; then the cap, and the
            // ScrollView takes over.
            Layout.preferredHeight: Math.min(form.implicitHeight, dialog.maxFormHeight)
            // No horizontal scrolling: the form fills the width it is given.
            contentWidth: availableWidth
            clip: true
            Controls.ScrollBar.vertical.policy: form.implicitHeight > height
                ? Controls.ScrollBar.AsNeeded : Controls.ScrollBar.AlwaysOff

            Kirigami.FormLayout {
                id: form
                width: scroll.availableWidth
                Layout.fillWidth: true
                Layout.topMargin: Kirigami.Units.smallSpacing

                Controls.Label {
                    text: qsTr("Connection")
                    font.bold: true
                    Kirigami.FormData.label: ""
                }
                Controls.TextField {
                    id: hostField
                    Kirigami.FormData.label: qsTr("Host / tailnet IP:")
                    placeholderText: qsTr("e.g. 192.168.1.5 or host.example.net")
                }
                Controls.TextField {
                    id: portField
                    Kirigami.FormData.label: qsTr("Port:")
                    inputMethodHints: Qt.ImhDigitsOnly
                    placeholderText: qsTr("3284")
                }
                Controls.CheckBox {
                    id: tlsBox
                    text: qsTr("Use TLS (wss)")
                    Kirigami.FormData.label: qsTr("Transport:")
                }
                // WebPKI verification is the default (core, Android, desktop).
                // This opt-out exists only for self-signed tailnet hosts: with it
                // on, ANY certificate is accepted, so an attacker on the network
                // path can impersonate the server and read the X-Secret-Key.
                Controls.CheckBox {
                    id: insecureTlsBox
                    text: qsTr("Skip TLS certificate verification (insecure)")
                    enabled: tlsBox.checked
                    Kirigami.FormData.label: qsTr("Certificate:")
                    Controls.ToolTip.visible: hovered
                    Controls.ToolTip.text: qsTr("Only for self-signed servers on a trusted private network.")
                }
                Controls.Label {
                    visible: tlsBox.checked
                    text: insecureTlsBox.checked
                          ? qsTr("WARNING: certificate verification is OFF. An attacker on the network path can impersonate the server and steal your X-Secret-Key.")
                          : qsTr("WebPKI verification is ON. Enable the opt-out only for a self-signed server on a trusted private network.")
                    color: insecureTlsBox.checked ? Kirigami.Theme.negativeTextColor
                                                  : Kirigami.Theme.disabledTextColor
                    wrapMode: Text.Wrap
                    Kirigami.FormData.label: ""
                }
                Controls.TextField {
                    id: keyField
                    Kirigami.FormData.label: qsTr("Secret key:")
                    echoMode: TextInput.Password
                }
                // Probe the endpoint with the current fields (committed first —
                // Settings persists on close, so the fields aren't on Mgr yet).
                Controls.Button {
                    Layout.fillWidth: true
                    implicitHeight: Kirigami.Units.gridUnit * 2
                    text: qsTr("Test connection")
                    icon.name: "network-connect"
                    Kirigami.FormData.label: ""
                    onClicked: {
                        dialog.commit()
                        dialog.testing = true
                        dialog.testMessage = qsTr("Testing…")
                        Mgr.testConnection()
                    }
                }
                Controls.Label {
                    visible: dialog.testMessage.length > 0
                    text: dialog.testMessage
                    color: dialog.testing ? Kirigami.Theme.textColor
                                          : (dialog.testOk ? Kirigami.Theme.positiveTextColor
                                                           : Kirigami.Theme.negativeTextColor)
                    wrapMode: Text.Wrap
                    Kirigami.FormData.label: ""
                }

                Controls.Label {
                    text: qsTr("Sessions")
                    font.bold: true
                    Kirigami.FormData.label: ""
                    Layout.topMargin: Kirigami.Units.gridUnit
                }
                Controls.TextField {
                    id: cwdField
                    Kirigami.FormData.label: qsTr("Working directory:")
                    placeholderText: qsTr("e.g. /home/colin/Projects/Inbox")
                }
                Controls.CheckBox {
                    id: autoBox
                    text: qsTr("Connect automatically on launch")
                    Kirigami.FormData.label: qsTr("Startup:")
                }
                // Desktop notifications come from this client's own connection (turn
                // finished, approval needed, a session changed elsewhere) — no server
                // support involved. Shown only when the window isn't the active one.
                Controls.CheckBox {
                    id: notifyBox
                    text: qsTr("Notify when the window isn't focused")
                    Kirigami.FormData.label: qsTr("Notifications:")
                }
                // Picker noise control: goose's inventory marks which providers are actually
                // configured; this hides the rest. The current pick is always kept.
                Controls.CheckBox {
                    id: configuredOnlyBox
                    text: qsTr("Show configured providers only")
                    Kirigami.FormData.label: qsTr("Providers:")
                }
                // UnifiedPush is receive-only here: the app registers with the desktop's
                // distributor and shows whatever your own senders POST to that endpoint.
                // A stock goose server never pushes — nothing depends on this.
                Controls.CheckBox {
                    id: pushBox
                    text: qsTr("Register with the desktop's UnifiedPush distributor")
                    Kirigami.FormData.label: qsTr("Push:")
                    onToggled: Push.enabled = checked
                }
                Controls.TextField {
                    readOnly: true
                    visible: Push.endpoint.length > 0
                    text: Push.endpoint
                    Kirigami.FormData.label: qsTr("Push endpoint:")
                    Controls.ToolTip.visible: hovered
                    Controls.ToolTip.text: qsTr("What your own sender POSTs to (receive-only)")
                }
                Controls.Label {
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    visible: Push.status.length > 0
                    text: qsTr("UnifiedPush: %1 — Grouse ships no sender; your goose-side scripts are the sender.").arg(Push.status)
                    color: Kirigami.Theme.disabledTextColor
                    font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
                }

                Controls.Label {
                    text: qsTr("Features")
                    font.bold: true
                    Kirigami.FormData.label: ""
                    Layout.topMargin: Kirigami.Units.gridUnit
                }
                // Off by default: roam peers are a separate iroh transport, not the
                // home server, so the tab (and stored peers) stay dormant until asked.
                Controls.CheckBox {
                    id: roamBox
                    text: qsTr("Show the Roam tab")
                    Kirigami.FormData.label: qsTr("Roam:")
                }
                // A run of tool calls is labelled with a short model-written line
                // instead of a bare count. Costs one small inference per turn, on a
                // hidden session that never appears as a chat.
                Controls.CheckBox {
                    id: runSummariesBox
                    text: qsTr("Summarize tool runs with the model")
                    Kirigami.FormData.label: qsTr("Tool runs:")
                }
                // A phone camera photo rides every turn's prompt as megabytes of
                // base64; downscaling to a generous long edge keeps the vision
                // content and drops the bytes ~10x. Off sends the original pixels.
                Controls.CheckBox {
                    id: rescaleBox
                    text: qsTr("Rescale attached images for sending")
                    Kirigami.FormData.label: qsTr("Images:")
                }
                // Debug surface: the core's turn log (and the live turn state under the
                // working indicator). Both show internal state rather than conversation,
                // so they are opt-in and off by default.
                Controls.CheckBox {
                    id: debugBox
                    text: qsTr("Developer diagnostics")
                    Kirigami.FormData.label: qsTr("Debug:")
                }
                Controls.Button {
                    text: qsTr("Turn log…")
                    icon.name: "view-list-text"
                    enabled: debugBox.checked
                    Kirigami.FormData.label: ""
                    onClicked: {
                        // Commit first (the log dialog reads Mgr.debugTools) and close
                        // this one, so two modals are not stacked on each other.
                        dialog.commit()
                        dialog.close()
                        dialog.openTurnLog()
                    }
                }

                Controls.Label {
                    text: qsTr("Dev")
                    font.bold: true
                    Kirigami.FormData.label: ""
                    Layout.topMargin: Kirigami.Units.gridUnit
                }
                // GLOBAL, not per chat: "my dev chats get a second opinion" is a
                // property of how you work, and a per-session flag would be one more
                // thing to remember to set. Each review is a real model call on a
                // hidden session, which is why it is off until asked for.
                Controls.CheckBox {
                    id: advisorBox
                    text: qsTr("Review each dev turn with an advisor")
                    Kirigami.FormData.label: qsTr("Advisor:")
                }
                Controls.Label {
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    color: Kirigami.Theme.disabledTextColor
                    Kirigami.FormData.label: ""
                    text: qsTr("After every turn in a dev chat, a second model call reviews what the turn changed and posts a short note above the composer. It costs one extra inference per turn, and it never joins the conversation — the review is about the turn, not part of it.")
                }

                Kirigami.Separator {
                    Layout.fillWidth: true
                    Layout.topMargin: Kirigami.Units.smallSpacing
                    Layout.bottomMargin: Kirigami.Units.smallSpacing
                }
                // Disconnect lives here (the sidebar only offers Connect).
                Controls.Button {
                    visible: Mgr.online
                    text: qsTr("Disconnect")
                    icon.name: "network-disconnect"
                    Kirigami.FormData.label: ""
                    onClicked: Mgr.disconnect()
                }
                // Global tools (config.yaml extensions) are reached from Settings,
                // not the sidebar.
                Controls.Button {
                    text: qsTr("Global tools…")
                    icon.name: "configure"
                    Kirigami.FormData.label: ""
                    onClicked: { dialog.close(); dialog.openGlobalTools() }
                }
                // The raw config editor sits beside Global tools: both are
                // server-owned settings, and neither belongs in a chat.
                Controls.Button {
                    text: qsTr("Goose config…")
                    icon.name: "document-edit"
                    Kirigami.FormData.label: ""
                    onClicked: { dialog.close(); dialog.openGooseConfig() }
                }
            }
        }
    }
}
