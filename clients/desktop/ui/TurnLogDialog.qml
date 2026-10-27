import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// Debug surface, reachable only while Settings → Developer diagnostics is on.
//
// The core keeps a turn log (one line per lifecycle event: prompt sent, run id,
// the reply, and every finish decision — including the ones it IGNORED because
// nothing was in flight) and an app-side log of what the UI did with those
// events. A turn that looks stuck on screen is diagnosed by reading the two in
// order, which is why they share one dialog rather than living in a debugger.
Controls.Dialog {
    id: dialog
    title: qsTr("Turn log")
    modal: true
    standardButtons: Controls.Dialog.Close
    closePolicy: Controls.Popup.CloseOnEscape
    width: 720
    height: 560

    // Re-read on open rather than binding: the log grows while the dialog is
    // closed, and a live binding on a 200-line string would re-lay-out the
    // whole view on every turn event.
    property string logText: ""
    property string statusText: ""
    function reload() {
        logText = Mgr.turnLog()
    }
    onOpened: reload()

    contentItem: ColumnLayout {
        spacing: Kirigami.Units.smallSpacing

        // The live half: whose turn is it, and how long the wire has been idle.
        // Older than a few seconds with no active run id means the server closed
        // the turn and the end signal never reached us.
        Controls.Label {
            Layout.fillWidth: true
            text: Mgr.currentSessionId.length > 0
                  ? (Mgr.turnDiag(Mgr.currentSessionId) || qsTr("No turn in flight."))
                  : qsTr("No chat open.")
            textFormat: Text.PlainText
            wrapMode: Text.WrapAnywhere
            font.family: "monospace"
        }

        Controls.ScrollView {
            Layout.fillWidth: true
            Layout.fillHeight: true
            // TextArea read-only rather than a Label: it keeps a selection, so a
            // measured region can be selected, and it scrolls without a Flickable
            // nested in the dialog's own content item.
            Controls.TextArea {
                id: logArea
                readOnly: true
                selectByMouse: true
                wrapMode: TextEdit.WrapAnywhere
                font.family: "monospace"
                font.pixelSize: Math.max(10, Math.round(Kirigami.Theme.defaultFont.pixelSize * 0.9))
                // NOT Text.AcceptRichText: the log carries server- and tool-supplied
                // text verbatim, so it must never be parsed as markup.
                textFormat: TextEdit.PlainText
                text: dialog.logText.length > 0 ? dialog.logText : qsTr("(no turn events yet)")
            }
        }

        RowLayout {
            Layout.fillWidth: true
            Controls.Button {
                text: qsTr("Copy")
                icon.name: "edit-copy"
                onClicked: {
                    logArea.selectAll()
                    logArea.copy()
                    logArea.deselect()
                    dialog.statusText = qsTr("Copied.")
                }
            }
            Controls.Label {
                Layout.fillWidth: true
                text: dialog.statusText
                color: Kirigami.Theme.disabledTextColor
                elide: Text.ElideRight
            }
            Controls.Button {
                text: qsTr("Reload")
                icon.name: "view-refresh"
                onClicked: dialog.reload()
            }
        }
    }
}
