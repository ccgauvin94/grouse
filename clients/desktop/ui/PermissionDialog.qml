import QtQuick
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

Controls.Dialog {
    id: dialog
    title: qsTr("Tool approval")
    modal: true
    closePolicy: Controls.Popup.NoAutoClose

    property string pendingToolCallId: ""
    property string appTitle: ""
    // The server's OWN answers for this request (allow once / always allow / deny…),
    // assigned by whoever raises the dialog.
    //
    // It is a property, NOT a `Mgr.permissionOptions()` call in the delegate below.
    // A QML binding that calls an invokable reads no property, so QML evaluates it
    // exactly once — when this dialog instance is created at app start, while nothing
    // is pending — and never again. The list stayed empty for the life of the process
    // and the dialog showed only the static Deny, whatever the server offered.
    property var options: []

    /** A kind as words. The server names its options by id ("allow_once"), which is
     *  not something to put on a button. */
    function optionLabel(kind) {
        switch (kind) {
        case "allow_once": return qsTr("Allow once")
        case "allow_always": return qsTr("Always allow")
        case "reject_once": return qsTr("Deny")
        case "reject_always": return qsTr("Always deny")
        default: return qsTr("Deny")
        }
    }

    contentItem: Column {
        width: 420
        spacing: Kirigami.Units.smallSpacing

        Kirigami.Heading {
            text: dialog.appTitle || qsTr("Tool approval requested")
            textFormat: Text.PlainText
            level: 3
            width: parent.width
            wrapMode: Text.Wrap
        }

        Controls.Label {
            text: qsTr("%1 wants to run a tool on the goose server. What should I do?")
                      .arg(qsTr("Goose"))
            width: parent.width
            wrapMode: Text.Wrap
        }

        Repeater {
            model: dialog.options
            delegate: Controls.Button {
                // The core's flat record is snake_case (`option_id`), like every other
                // record the client reads; QML's `modelData.optionId` was undefined, so
                // an allow button would have answered with an empty id — i.e. a denial.
                text: dialog.optionLabel(modelData.kind)
                // NO textFormat here: `Controls.Button` has no such property, and
                // assigning one does not merely warn — the whole type fails to
                // construct, which takes `PermissionDialog` (and therefore
                // main.qml, since every dialog is a declared instance) down with
                // it. Button text is already plain: rich text is a Label/Text
                // feature, so the "render server text as plain text" rule needs
                // nothing here.
                flat: true
                width: parent.width
                onClicked: {
                    // Clear the id FIRST: onClosed answers an empty dialog with a
                    // denial, so a clicked option would otherwise be answered twice
                    // (allow, then cancel).
                    const id = dialog.pendingToolCallId
                    dialog.pendingToolCallId = ""
                    Mgr.respondPermission(id, modelData.option_id)
                    dialog.close()
                }
            }
        }

        Controls.Button {
            // Every goose request carries its own options, so this is the fallback for
            // a server that sends none. A dismissal is a denial, so one button covers
            // Escape, the close box and this.
            visible: dialog.options.length === 0
            text: qsTr("Deny")
            flat: true
            width: parent.width
            onClicked: {
                const id = dialog.pendingToolCallId
                dialog.pendingToolCallId = ""
                Mgr.respondPermission(id, "")
                dialog.close()
            }
        }
    }

    onClosed: {
        // Any other dismissal (Escape, the window's close box) is also a denial.
        if (pendingToolCallId.length > 0) {
            const id = pendingToolCallId
            pendingToolCallId = ""
            Mgr.respondPermission(id, "")
        }
    }
}
