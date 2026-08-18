import QtQuick
import QtQuick.Controls
import QtTest

// Regression test for the provider/model ComboBoxes: without a valueRole,
// QQC2's currentValue does NOT return the value string — Mgr.setConfigOption
// silently received "[object V4ReferenceObject]" and provider switches broke.
// The strip/landing combos must carry valueRole: "value".
TestCase {
    id: tc
    name: "ComboBoxValueRole"
    width: 400
    height: 200

    function makeCombo(withValueRole) {
        const role = withValueRole ? 'valueRole: "value"; ' : ""
        return Qt.createQmlObject(
            'import QtQuick.Controls; ComboBox { ' + role +
            'model: [{ value: "openai", name: "OpenAI" }, { value: "anthropic", name: "Anthropic" }]; ' +
            'textRole: "name" }', tc, "combo")
    }

    function test_valueRoleDeliversValueString() {
        const combo = makeCombo(true)
        combo.currentIndex = 0
        compare(combo.currentValue, "openai")
        combo.currentIndex = 1
        compare(combo.currentValue, "anthropic")
        combo.destroy()
    }

    function test_withoutValueRoleDoesNotDeliverValue() {
        // Documents why valueRole is mandatory: the value string is NOT what
        // currentValue yields without it (it degrades to text/item).
        const combo = makeCombo(false)
        combo.currentIndex = 0
        verify(combo.currentValue !== "openai")
        combo.destroy()
    }
}
