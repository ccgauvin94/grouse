import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

/** Structured input the SERVER is waiting on: an MCP/ACP elicitation, or a
 *  recipe's declared parameters (the core parks the request, so an unanswered
 *  form hangs the turn — this dialog is not optional and has no silent dismiss).
 *
 *  Field types map the way Android's ElicitationSheet maps them: a checkbox for
 *  booleans, a picker for enum/oneOf choices, a typed field otherwise. The form
 *  is rendered from `Mgr.pendingForm`, and answering goes back through
 *  `Mgr.answerForm`, which knows the two wire shapes (elicitations take a typed
 *  content object, recipe parameters a map of STRINGS). */
Controls.Dialog {
    id: dialog
    modal: true
    closePolicy: Controls.Popup.NoAutoClose

    readonly property var form: Mgr.pendingForm
    readonly property var fields: form && form.fields ? form.fields : []
    readonly property bool recipeParams: form && form.recipeParams === true

    // name -> value. Deliberately a plain JS object updated by handlers rather
    // than two-way `text:` bindings: QQC2 TextField drops a `text:` binding the
    // moment the user edits (the footgun documented in AGENTS.md).
    property var values: ({})

    // NOT `reset`: Qt 6's Dialog carries a `reset` signal, so declaring a function
    // with that name is an invalid override and QML drops the whole declaration —
    // which left `values` empty, so booleans never started false and enums never
    // pre-selected their first option.
    function resetValues() {
        values = ({})
        for (var i = 0; i < fields.length; i++) {
            var f = fields[i]
            if (f.type === "boolean") values[f.name] = false
            else if (f.options && f.options.length > 0) values[f.name] = f.options[0].value
            else values[f.name] = ""
        }
        valuesChanged()
    }
    function setValue(name, v) {
        values[name] = v
        valuesChanged()
    }
    function missingRequired() {
        for (var i = 0; i < fields.length; i++) {
            var f = fields[i]
            if (!f.required || f.type === "boolean") continue
            var v = values[f.name]
            if (v === undefined || v === null || String(v).length === 0) return true
        }
        return false
    }
    function submit() {
        if (missingRequired()) return
        // Recipe parameters are all strings on the wire; numbers/booleans have to
        // be stringified here so the server can type them from its own schema.
        var payload = ({})
        for (var i = 0; i < fields.length; i++) {
            var f = fields[i]
            var v = values[f.name]
            if (recipeParams) {
                // recipe/request-params is HashMap<String,String>: stringify
                // everything so the server can type it from its own schema.
                payload[f.name] = v === undefined ? "" : String(v)
            } else if (f.type === "number" || f.type === "integer") {
                var n = parseFloat(v)
                if (!isNaN(n)) payload[f.name] = f.type === "integer" ? Math.trunc(n) : n
            } else if (f.type === "boolean") {
                payload[f.name] = v === true
            } else if (v !== undefined && String(v).length > 0) {
                payload[f.name] = String(v)
            }
        }
        Mgr.answerForm(payload, "accept")
        dialog.close()
    }
    function refuse(cancelled) {
        // A recipe has no "decline": a refusal there is a cancel.
        Mgr.answerForm(({}), recipeParams || cancelled ? "cancel" : "decline")
        dialog.close()
    }

    title: form && form.title ? form.title : qsTr("Input requested")
    width: 520
    height: Math.min(620, 200 + formColumn.implicitHeight)

    onOpened: resetValues()
    // Every path out of the dialog answers the parked request: an abandoned
    // responder would leave the turn blocked on the server.
    onClosed: if (Mgr.pendingForm && Object.keys(Mgr.pendingForm).length > 0
                  && Mgr.pendingForm.title === form.title) refuse(true)

    contentItem: ColumnLayout {
        id: formColumn
        spacing: Kirigami.Units.smallSpacing

        Controls.Label {
            visible: !!(dialog.form && dialog.form.message)
            text: dialog.form && dialog.form.message ? dialog.form.message : ""
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            Layout.fillWidth: true
        }

        Controls.ScrollView {
            Layout.fillWidth: true
            Layout.fillHeight: true
            Layout.preferredHeight: Math.min(400, fieldsColumn.implicitHeight)

            ColumnLayout {
                id: fieldsColumn
                width: parent.width
                spacing: Kirigami.Units.smallSpacing

                Repeater {
                    model: dialog.fields
                    delegate: ColumnLayout {
                        required property var modelData
                        Layout.fillWidth: true
                        spacing: Kirigami.Units.smallSpacing / 2

                        // Boolean: the label IS the control.
                        Controls.CheckBox {
                            visible: modelData.type === "boolean"
                            text: modelData.title
                            checked: dialog.values[modelData.name] === true
                            onToggled: dialog.setValue(modelData.name, checked)
                        }

                        Controls.Label {
                            visible: modelData.type !== "boolean"
                            text: modelData.title + (modelData.required ? " *" : "")
                            textFormat: Text.PlainText
                            font.bold: true
                            Layout.fillWidth: true
                            wrapMode: Text.Wrap
                        }

                        Controls.ComboBox {
                            visible: modelData.options !== undefined
                                     && modelData.options.length > 0
                            Layout.fillWidth: true
                            // valueRole is mandatory for a {value,label} model:
                            // without it currentValue is the ITEM (a JS object)
                            // and the answer reaches the server as nonsense.
                            textRole: "label"
                            valueRole: "value"
                            model: modelData.options
                            onActivated: dialog.setValue(modelData.name, currentValue)
                            Component.onCompleted: if (count > 0)
                                dialog.setValue(modelData.name, currentValue)
                        }

                        Controls.TextField {
                            visible: modelData.type !== "boolean"
                                     && (modelData.options === undefined
                                         || modelData.options.length === 0)
                            Layout.fillWidth: true
                            placeholderText: modelData.description ? modelData.description : ""
                            inputMethodHints: (modelData.type === "number"
                                               || modelData.type === "integer")
                                              ? Qt.ImhFormattedNumbersOnly : Qt.ImhNone
                            onTextChanged: dialog.setValue(modelData.name, text)
                        }

                        Controls.Label {
                            visible: modelData.type !== "boolean"
                                     && (modelData.options === undefined
                                         || modelData.options.length === 0)
                                     && modelData.description
                            text: modelData.description
                            textFormat: Text.PlainText
                            font: Kirigami.Theme.smallFont
                            opacity: 0.7
                            wrapMode: Text.Wrap
                            Layout.fillWidth: true
                        }
                    }
                }
            }
        }

        RowLayout {
            Layout.fillWidth: true
            Item { Layout.fillWidth: true }
            Controls.Button {
                text: dialog.recipeParams ? qsTr("Cancel") : qsTr("Decline")
                onClicked: dialog.refuse(false)
            }
            Controls.Button {
                text: qsTr("Submit")
                enabled: !dialog.missingRequired()
                highlighted: true
                onClicked: dialog.submit()
            }
        }
    }
}
