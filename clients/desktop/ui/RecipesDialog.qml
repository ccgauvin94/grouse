import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// The full recipe surface, mirroring the Android RecipeScreens: a skill-like
// master-detail (recipe list left, editor panel right). A schedule is one of
// a recipe's settings, so the scheduler lives here, not on its own screen.
// Mgr.refreshRecipes() re-lists recipes AND schedules together.
//
// The schedule editor and the provider/model pickers are twins of Android's
// CronEditor (RecipeScreens.kt) and the chat's provider strip. Anything the
// cron picker cannot express opens as Custom rather than being silently
// rewritten into a close-but-wrong shape.
//
// Payload shape (raw from the core, one entry per recipe):
//   { id, file_path, schedule_cron, recipe: { title, description, prompt,
//     instructions, settings: { goose_model, goose_provider },
//     parameters: [{key, requirement, description, default}],
//     sub_recipes: [{name}], extensions: [{name}] } }
// Schedule rows: { id, cron, source, paused, currentlyRunning, lastRun }.
Controls.Dialog {
    id: dialog
    title: qsTr("Recipes")
    modal: true
    standardButtons: Controls.Dialog.Close
    closePolicy: Controls.Popup.CloseOnEscape
    width: Math.min(860, parent ? parent.width - 32 : 860)
    height: Math.min(640, parent ? parent.height - 32 : 640)

    // "Running ..." after a run-now press (the reply is the finish, not the start).
    property string note: ""

    // --- master-detail state -------------------------------------------------
    // Selected by ID so a refreshRecipes re-list keeps the selection and can
    // re-push fresh data into the editor.
    property string selectedId: ""
    property var sel: null
    property var job: null
    property string selOrigCron: ""
    property string selOrigModel: ""
    property string selOrigProvider: ""
    // Drafts while the picker selection is unsaved (Save compares against orig).
    property string selDraftModel: ""
    property string selDraftProvider: ""
    property string selOrigPrompt: ""
    property string selOrigInstructions: ""
    // The schedule the picker edits; replaced whole (never mutated) so the
    // cronText/save bindings recompute.
    property var cronSpec: ({ kind: "custom", minute: 0, hour: 6, fromHour: 0, toHour: 23, dow: "Mon", raw: "" })
    property string cronText: dialog.buildCron(dialog.cronSpec)

    property var cronKinds: ["hourly", "daily", "weekly", "custom"]
    property var cronDays: ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]

    // Test-visible handles: ids are file-private, so the QML test (which owns
    // no scope inside this component) reaches the widgets through aliases.
    readonly property alias detailList: recipeList
    readonly property alias cronKindPicker: cronKindCombo
    readonly property alias minutePicker: minCombo
    readonly property alias hourPicker: hourCombo
    readonly property alias providerPicker: providerCombo
    readonly property alias modelPicker: modelCombo
    readonly property alias promptEditor: promptArea
    readonly property alias instructionsEditor: instrArea

    onOpened: Mgr.refreshRecipes()

    // The model reloads under us (refresh replies, saves, deletes); the detail
    // panel edits PUSHED copies, so re-push whenever the list changes.
    Connections {
        target: Mgr
        function onRecipesChanged() {
            if (!dialog.entryForId(dialog.selectedId)) {
                dialog.selectedId = Mgr.recipes.length > 0 ? Mgr.recipes[0].id : ""
            }
            dialog.pushDetail()
        }
        // Config choices (provider/model lists) arrive with the connection.
        function onConfigChanged() { dialog.pushPins() }
    }

    function entryForId(id) {
        const rs = Mgr.recipes
        for (let i = 0; i < rs.length; ++i)
            if (rs[i].id === id) return rs[i]
        return null
    }
    function selectById(id) {
        dialog.selectedId = id
        dialog.pushDetail()
    }

    // Re-assign on every selection: QQC2 text inputs and combos break their
    // bindings on user edit, so the editor must be PUSHED, never bound
    // (the SkillsDialog lesson).
    function pushDetail() {
        const entry = dialog.entryForId(dialog.selectedId)
        dialog.sel = entry
        dialog.job = entry ? dialog.jobFor(entry) : null
        if (!entry) return
        // schedule_cron is often empty even when the recipe IS scheduled —
        // the cron lives on the matched job. statusLine always fell back; the
        // editor must too, or scheduled recipes open with a blank picker.
        const c = entry.schedule_cron || (dialog.job ? dialog.job.cron : "")
        dialog.cronSpec = dialog.parseCron(c)
        dialog.selOrigCron = c
        dialog.selOrigModel = dialog.settingOf(entry, "goose_model")
        dialog.selOrigProvider = dialog.settingOf(entry, "goose_provider")
        dialog.selDraftModel = dialog.selOrigModel
        dialog.selDraftProvider = dialog.selOrigProvider
        dialog.selOrigPrompt = entry.recipe && entry.recipe.prompt ? entry.recipe.prompt : ""
        dialog.selOrigInstructions = entry.recipe && entry.recipe.instructions ? entry.recipe.instructions : ""
        pushPins()
        promptArea.text = dialog.selOrigPrompt
        instrArea.text = dialog.selOrigInstructions
    }

    // Push picker positions from dialog state (list rebuilt here so a config
    // refresh updates it; an unsaved draft wins over the saved pin).
    function pushPins() {
        const s = dialog.cronSpec
        cronKindCombo.currentIndex = dialog.cronKinds.indexOf(s.kind)
        minCombo.currentIndex = Math.floor(s.minute / 5)
        hourCombo.currentIndex = s.hour
        fromHourCombo.currentIndex = s.fromHour
        toHourCombo.currentIndex = s.toHour
        dowCombo.currentIndex = dialog.cronDays.indexOf(s.dow)
        customField.text = s.raw
        providerCombo.model = dialog.pinChoices("provider", dialog.selOrigProvider)
        providerCombo.currentIndex = valueIndex(providerCombo.model,
                                               dialog.selDraftProvider || dialog.selOrigProvider)
        modelCombo.model = dialog.pinChoices("model", dialog.selOrigModel)
        modelCombo.currentIndex = valueIndex(modelCombo.model,
                                            dialog.selDraftModel || dialog.selOrigModel)
    }

    // --- cron picker (JS twin of Android's parseCron/buildCron) ---------------

    // goose prefixes a seconds field to a 5-field expression; accept both.
    function parseCron(cron) {
        const base = { kind: "custom", minute: 0, hour: 6, fromHour: 0, toHour: 23, dow: "Mon", raw: cron }
        const f = cron.trim() ? cron.trim().split(/\s+/) : []
        if (f.length === 0) return base
        let p = f
        if (f.length === 5) p = ["0"].concat(f)
        else if (f.length !== 6) return base
        const sec = p[0], min = p[1], hour = p[2], dom = p[3], mon = p[4], dow = p[5]
        const m = parseInt(min, 10)
        if (sec !== "0" || isNaN(m) || dom !== "*" || mon !== "*") return base
        if (hour === "*" && dow === "*")
            return { kind: "hourly", minute: m, hour: 6, fromHour: 0, toHour: 23, dow: "Mon", raw: "" }
        if (/^\d+-\d+$/.test(hour) && dow === "*") {
            const ab = hour.split("-")
            return { kind: "hourly", minute: m, hour: 6, fromHour: parseInt(ab[0], 10), toHour: parseInt(ab[1], 10), dow: "Mon", raw: "" }
        }
        if (/^\d+$/.test(hour) && dow === "*")
            return { kind: "daily", minute: m, hour: parseInt(hour, 10), fromHour: 0, toHour: 23, dow: "Mon", raw: "" }
        if (/^\d+$/.test(hour)) {
            for (let i = 0; i < dialog.cronDays.length; ++i)
                if (dialog.cronDays[i].toLowerCase() === dow.toLowerCase())
                    return { kind: "weekly", minute: m, hour: parseInt(hour, 10), fromHour: 0, toHour: 23, dow: dialog.cronDays[i], raw: "" }
        }
        return base
    }

    function buildCron(s) {
        if (s.kind === "hourly")
            return (s.fromHour === 0 && s.toHour === 23)
                   ? "0 " + s.minute + " * * * *"
                   : "0 " + s.minute + " " + s.fromHour + "-" + s.toHour + " * * *"
        if (s.kind === "daily") return "0 " + s.minute + " " + s.hour + " * * *"
        if (s.kind === "weekly") return "0 " + s.minute + " " + s.hour + " * * " + s.dow
        return s.raw
    }

    function setSpec(fields) {
        const s = Object.assign({}, dialog.cronSpec)
        for (const k in fields) s[k] = fields[k]
        dialog.cronSpec = s
    }

    // --- helpers (pure JS, twin of the Android parsers in Wire.kt) -----------

    // A job runs this recipe when its source path and the recipe's file_path
    // are the same file (either may carry the library prefix).
    function jobFor(entry) {
        const fp = entry.file_path || ""
        if (!fp) return null
        const jobs = Mgr.schedules
        for (let i = 0; i < jobs.length; ++i) {
            const src = jobs[i].source || ""
            if (src && (src.endsWith(fp) || fp.endsWith(src))) return jobs[i]
        }
        return null
    }

    function pad2(n) { return (n < 10 ? "0" : "") + n }

    // Plain-English reading of the cron shapes people actually use; anything
    // unrecognized shows the raw expression (a wrong reading is worse than the cron).
    function cronInEnglish(cron) {
        const f = cron.trim().split(/\s+/)
        let p = f
        if (f.length === 5) p = ["0"].concat(f)
        else if (f.length !== 6) return cron
        if (p[0] !== "0" || !/^\d+$/.test(p[1]) || p[3] !== "*" || p[4] !== "*")
            return cron
        const m = parseInt(p[1], 10)
        const hhmm = h => pad2(h) + ":" + pad2(m)
        if (p[2] === "*" && p[5] === "*") return qsTr("hourly at :%1").arg(pad2(m))
        if (/^\d+-\d+$/.test(p[2]) && p[5] === "*") {
            const ab = p[2].split("-")
            // Chained .arg: the two-string form resolves to arg(String, int
            // fieldWidth) in QML and errors with "String.arg(): Invalid arguments".
            return qsTr("hourly, %1-%2").arg(hhmm(parseInt(ab[0], 10))).arg(hhmm(parseInt(ab[1], 10)))
        }
        if (/^\d+$/.test(p[2]) && p[5] === "*") return qsTr("daily at %1").arg(hhmm(parseInt(p[2], 10)))
        if (/^\d+$/.test(p[2])) {
            const days = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
            for (const d of days)
                if (d.toLowerCase() === p[5].toLowerCase())
                    return qsTr("%1 at %2").arg(d).arg(hhmm(parseInt(p[2], 10)))
        }
        return cron
    }

    function statusLine(entry) {
        const job = jobFor(entry)
        const dc = entry.schedule_cron || (job ? job.cron : "")
        let s = dc ? cronInEnglish(dc) : qsTr("not scheduled")
        if (job && job.currentlyRunning) s += " · " + qsTr("running now")
        else if (job && job.paused) s += " · " + qsTr("paused")
        if (job && job.lastRun)
            s += " · " + qsTr("last %1").arg(String(job.lastRun).slice(0, 16).replace("T", " "))
        return s
    }

    // recipes/save replaces the whole DTO: rebuild the raw object, blank drops
    // the key, and an empty settings block is removed (goose treats a present-
    // but-empty block differently from an absent one).
    function recipeDto(entry, edits) {
        const r = JSON.parse(JSON.stringify(entry.recipe || {}))
        for (const e of edits) {
            if (e.setting) {
                r.settings = r.settings || {}
                if (e.value === "") delete r.settings[e.key]
                else r.settings[e.key] = e.value
            } else {
                if (e.value === "") delete r[e.key]
                else r[e.key] = e.value
            }
        }
        if (r.settings && Object.keys(r.settings).length === 0) delete r.settings
        return JSON.stringify(r)
    }

    function settingOf(entry, key) {
        const st = entry.recipe ? entry.recipe.settings : null
        return (st && st[key]) || ""
    }

    // --- provider/model pickers (twin of the chat's provider strip) -----------
    // Choices come from the same server config options (Mgr.config:
    // [{id, currentValue, choices:[{value,name}]}]).
    function findOption(id) {
        for (let i = 0; i < Mgr.config.length; i++)
            if (Mgr.config[i].id === id) return Mgr.config[i]
        return null
    }
    function valueIndex(list, value) {
        for (let i = 0; i < list.length; i++)
            if (list[i].value === value) return i
        return 0
    }
    function configChoices(id) {
        const o = findOption(id)
        const cs = (o && o.choices ? o.choices : []).map(c => ({ value: c.value, name: c.name || c.value }))
        // "Server default" is the empty pin; the recipe's own pin wins over it.
        return [{ value: "", name: qsTr("Server default") }].concat(cs)
    }
    // A recipe pinned to a provider/model not in the (session-config-derived)
    // choice list still shows its pin rather than silently dropping it.
    function pinChoices(id, pin) {
        const list = dialog.configChoices(id)
        if (pin && !list.some(c => c.value === pin))
            return list.concat([{ value: pin, name: pin }])
        return list
    }

    contentItem: RowLayout {
        spacing: Kirigami.Units.smallSpacing
        implicitWidth: 840
        implicitHeight: 620

        // --- master: the recipe list (SkillsDialog pattern) -------------------
        ColumnLayout {
            Layout.preferredWidth: 230
            Layout.fillHeight: true
            spacing: 2

            RowLayout {
                Layout.fillWidth: true
                Controls.Label {
                    text: qsTr("Saved recipes")
                    font.weight: Font.DemiBold
                    Layout.fillWidth: true
                }
                Controls.Label {
                    text: Mgr.recipes.length
                    color: Kirigami.Theme.disabledTextColor
                }
                Controls.ToolButton {
                    icon.name: "view-refresh"
                    display: Controls.AbstractButton.IconOnly
                    Controls.ToolTip.visible: hovered
                    Controls.ToolTip.text: qsTr("Refresh recipes")
                    onClicked: Mgr.refreshRecipes()
                }
            }

            ListView {
                id: recipeList
                Layout.fillWidth: true
                Layout.fillHeight: true
                clip: true
                spacing: 2
                model: Mgr.recipes
                highlightFollowsCurrentItem: false
                highlight: Rectangle {
                    radius: Kirigami.Units.smallSpacing
                    color: Kirigami.Theme.highlightColor
                    opacity: 0.22
                }
                Controls.ScrollBar.vertical: Controls.ScrollBar { policy: Controls.ScrollBar.AsNeeded }

                // Selection lives on the dialog (by id); the view's current
                // index (the highlight) follows re-lists.
                Connections {
                    target: Mgr
                    function onRecipesChanged() {
                        const rs = Mgr.recipes
                        for (let i = 0; i < rs.length; ++i)
                            if (rs[i].id === dialog.selectedId) { recipeList.currentIndex = i; return }
                        recipeList.currentIndex = -1
                    }
                }

                delegate: Controls.ItemDelegate {
                    id: rdel
                    width: ListView.view.width
                    height: Math.max(44, rdelCol.implicitHeight + Kirigami.Units.smallSpacing * 2)
                    onClicked: {
                        recipeList.currentIndex = index
                        dialog.selectById(modelData.id)
                    }
                    contentItem: Column {
                        id: rdelCol
                        spacing: 1
                        Controls.Label {
                            text: modelData.recipe && modelData.recipe.title
                                  ? modelData.recipe.title : modelData.id
                            width: parent.width
                            elide: Text.ElideRight
                            font.bold: true
                        }
                        Controls.Label {
                            text: dialog.statusLine(modelData)
                            width: parent.width
                            elide: Text.ElideRight
                            color: Kirigami.Theme.disabledTextColor
                            font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
                        }
                    }
                }

                footer: Controls.Label {
                    visible: Mgr.recipes.length === 0
                    text: qsTr("No recipes found.")
                    color: Kirigami.Theme.disabledTextColor
                    padding: Kirigami.Units.largeSpacing
                }
            }
        }

        Kirigami.Separator {
            Layout.fillHeight: true
            Layout.preferredWidth: 1
            color: Kirigami.Theme.separatorColor ? Kirigami.Theme.separatorColor
                                                 : Kirigami.Theme.disabledTextColor
        }

        // --- detail: the selected recipe's editor -----------------------------
        Controls.Label {
            visible: !dialog.sel
            text: qsTr("Select a recipe.")
            color: Kirigami.Theme.disabledTextColor
            Layout.fillWidth: true
            Layout.alignment: Qt.AlignTop
        }

        Controls.ScrollView {
            id: detailScroll
            visible: !!dialog.sel
            Layout.fillWidth: true
            Layout.fillHeight: true
            Controls.ScrollBar.vertical: Controls.ScrollBar { policy: Controls.ScrollBar.AsNeeded }

            ColumnLayout {
                width: detailScroll.availableWidth
                spacing: Kirigami.Units.smallSpacing

                Controls.Label {
                    text: dialog.sel && dialog.sel.recipe && dialog.sel.recipe.title
                          ? dialog.sel.recipe.title : (dialog.sel ? dialog.sel.id : "")
                    font.bold: true
                    font.pointSize: Kirigami.Theme.defaultFont.pointSize * 1.2
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                }
                Controls.Label {
                    text: dialog.sel && dialog.sel.recipe && dialog.sel.recipe.description
                          ? dialog.sel.recipe.description : qsTr("No description")
                    color: Kirigami.Theme.disabledTextColor
                    wrapMode: Text.Wrap
                    Layout.fillWidth: true
                }
                Controls.Label {
                    text: dialog.note
                    visible: dialog.note.length > 0
                    color: Kirigami.Theme.disabledTextColor
                    wrapMode: Text.Wrap
                    Layout.fillWidth: true
                }

                // --- actions row -----------------------------------------------
                RowLayout {
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.smallSpacing
                    Controls.Button {
                        text: qsTr("Start session")
                        icon.name: "media-playback-start"
                        onClicked: if (dialog.sel) Mgr.runRecipe(dialog.sel.id)
                    }
                    Controls.Button {
                        visible: !!dialog.job
                        text: qsTr("Run now")
                        flat: true
                        enabled: !!dialog.job && !dialog.job.currentlyRunning
                        onClicked: {
                            Mgr.runScheduleNow(dialog.job.id)
                            dialog.note = qsTr("Running — a briefing takes a few minutes and notifies if it has something.")
                        }
                    }
                    Controls.Switch {
                        visible: !!dialog.job
                        checked: !!dialog.job && !dialog.job.paused
                        onToggled: Mgr.setSchedulePaused(dialog.job.id, !checked)
                        Controls.ToolTip.visible: hovered
                        Controls.ToolTip.text: checked ? qsTr("Pause schedule") : qsTr("Enable schedule")
                    }
                    Item { Layout.fillWidth: true }
                }

                // --- Schedule (cron picker, twin of Android's CronEditor) ------
                Controls.Label { text: qsTr("Schedule"); font.weight: Font.DemiBold }
                RowLayout {
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.smallSpacing
                    Controls.ComboBox {
                        id: cronKindCombo
                        Layout.preferredWidth: 110
                        model: [qsTr("Hourly"), qsTr("Daily"), qsTr("Weekly"), qsTr("Custom")]
                        onActivated: {
                            const k = dialog.cronKinds[currentIndex]
                            // Switching INTO custom carries the built expression;
                            // out of custom keeps the picker fields.
                            dialog.setSpec(k === "custom" && dialog.cronSpec.kind !== "custom"
                                           ? { kind: k, raw: dialog.cronText } : { kind: k })
                        }
                    }
                    Controls.ComboBox {
                        id: minCombo
                        visible: dialog.cronSpec.kind !== "custom"
                        Layout.preferredWidth: 84
                        model: Array.from({ length: 12 }, (_, i) => dialog.pad2(i * 5))
                        Controls.ToolTip.visible: hovered
                        Controls.ToolTip.text: qsTr("Minute")
                        onActivated: dialog.setSpec({ minute: currentIndex * 5 })
                    }
                    Controls.ComboBox {
                        id: hourCombo
                        visible: dialog.cronSpec.kind === "daily" || dialog.cronSpec.kind === "weekly"
                        Layout.preferredWidth: 84
                        model: Array.from({ length: 24 }, (_, i) => dialog.pad2(i))
                        Controls.ToolTip.visible: hovered
                        Controls.ToolTip.text: qsTr("Hour")
                        onActivated: dialog.setSpec({ hour: currentIndex })
                    }
                    Controls.ComboBox {
                        id: fromHourCombo
                        visible: dialog.cronSpec.kind === "hourly"
                        Layout.preferredWidth: 120
                        model: Array.from({ length: 24 }, (_, i) => qsTr("from %1").arg(dialog.pad2(i)))
                        onActivated: dialog.setSpec({ fromHour: currentIndex,
                                                     toHour: Math.max(currentIndex, dialog.cronSpec.toHour) })
                    }
                    Controls.ComboBox {
                        id: toHourCombo
                        visible: dialog.cronSpec.kind === "hourly"
                        Layout.preferredWidth: 120
                        model: Array.from({ length: 24 }, (_, i) => qsTr("to %1").arg(dialog.pad2(i)))
                        onActivated: dialog.setSpec({ toHour: currentIndex,
                                                     fromHour: Math.min(currentIndex, dialog.cronSpec.fromHour) })
                    }
                    Controls.ComboBox {
                        id: dowCombo
                        visible: dialog.cronSpec.kind === "weekly"
                        Layout.preferredWidth: 100
                        model: dialog.cronDays
                        Controls.ToolTip.visible: hovered
                        Controls.ToolTip.text: qsTr("Day")
                        onActivated: dialog.setSpec({ dow: dialog.cronDays[currentIndex] })
                    }
                    Item { Layout.fillWidth: true }
                }
                Controls.TextField {
                    id: customField
                    visible: dialog.cronSpec.kind === "custom"
                    Layout.fillWidth: true
                    placeholderText: qsTr("Cron: sec min hour day month weekday")
                    // Pushed text re-fires this handler; writing the same raw
                    // value back into the spec is a harmless no-op.
                    onTextChanged: dialog.setSpec({ raw: text })
                }
                RowLayout {
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.smallSpacing
                    Controls.Label {
                        text: dialog.cronText.length > 0
                              ? dialog.cronInEnglish(dialog.cronText) + "   [" + dialog.cronText + "]"
                              : qsTr("not scheduled")
                        color: Kirigami.Theme.disabledTextColor
                        font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
                        wrapMode: Text.Wrap
                        Layout.fillWidth: true
                    }
                    Controls.Button {
                        text: qsTr("Save")
                        enabled: dialog.cronText.trim() !== dialog.selOrigCron
                        onClicked: {
                            if (!dialog.sel) return
                            Mgr.scheduleRecipe(dialog.sel.id, dialog.cronText.trim())
                            dialog.selOrigCron = dialog.cronText.trim()
                        }
                    }
                    Controls.Button {
                        text: qsTr("Unschedule")
                        flat: true
                        visible: dialog.selOrigCron.length > 0
                        onClicked: {
                            if (!dialog.sel) return
                            Mgr.scheduleRecipe(dialog.sel.id, "")
                            dialog.selOrigCron = ""
                            dialog.setSpec({ kind: "custom", raw: "" })
                        }
                    }
                }
                Controls.Label {
                    text: qsTr("Server local time.")
                    color: Kirigami.Theme.disabledTextColor
                    font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
                }

                // --- Model / provider pickers -----------------------------------
                Controls.Label { text: qsTr("Model"); font.weight: Font.DemiBold }
                RowLayout {
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.smallSpacing
                    Controls.ComboBox {
                        id: providerCombo
                        Layout.preferredWidth: 190
                        Layout.maximumWidth: 240
                        textRole: "name"
                        // currentValue returns the model ITEM unless a valueRole
                        // is set (the [object V4ReferenceObject] config bug).
                        valueRole: "value"
                        Controls.ToolTip.visible: hovered
                        Controls.ToolTip.text: qsTr("Provider pinned to this recipe")
                        onActivated: dialog.selDraftProvider = currentValue
                    }
                    Controls.ComboBox {
                        id: modelCombo
                        Layout.preferredWidth: 240
                        Layout.maximumWidth: 320
                        textRole: "name"
                        valueRole: "value"
                        Controls.ToolTip.visible: hovered
                        Controls.ToolTip.text: qsTr("Model pinned to this recipe")
                        onActivated: dialog.selDraftModel = currentValue
                    }
                    Item { Layout.fillWidth: true }
                    Controls.Button {
                        text: qsTr("Save")
                        enabled: dialog.selDraftModel !== dialog.selOrigModel
                              || dialog.selDraftProvider !== dialog.selOrigProvider
                        onClicked: {
                            if (!dialog.sel) return
                            Mgr.saveRecipe(dialog.sel.id, dialog.recipeDto(dialog.sel, [
                                { setting: true, key: "goose_model", value: dialog.selDraftModel },
                                { setting: true, key: "goose_provider", value: dialog.selDraftProvider }]))
                            dialog.selOrigModel = dialog.selDraftModel
                            dialog.selOrigProvider = dialog.selDraftProvider
                        }
                    }
                }
                Controls.Label {
                    text: qsTr("A recipe's own pin wins over the server default.")
                    color: Kirigami.Theme.disabledTextColor
                    font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
                    wrapMode: Text.Wrap
                    Layout.fillWidth: true
                }

                // --- Prompt ------------------------------------------------
                Controls.Label { text: qsTr("Prompt"); font.weight: Font.DemiBold }
                Controls.ScrollView {
                    Layout.fillWidth: true
                    Layout.preferredHeight: 160
                    Controls.TextArea {
                        id: promptArea
                        wrapMode: Text.Wrap
                    }
                }
                RowLayout {
                    Controls.Label { text: ""; Layout.fillWidth: true }
                    Controls.Button {
                        text: qsTr("Save")
                        enabled: promptArea.text !== dialog.selOrigPrompt
                        onClicked: {
                            if (!dialog.sel) return
                            Mgr.saveRecipe(dialog.sel.id, dialog.recipeDto(dialog.sel, [
                                { setting: false, key: "prompt", value: promptArea.text }]))
                            dialog.selOrigPrompt = promptArea.text
                        }
                    }
                }

                // --- Instructions --------------------------------------------
                Controls.Label { text: qsTr("Instructions"); font.weight: Font.DemiBold }
                Controls.ScrollView {
                    Layout.fillWidth: true
                    Layout.preferredHeight: 160
                    Controls.TextArea {
                        id: instrArea
                        wrapMode: Text.Wrap
                    }
                }
                RowLayout {
                    Controls.Label { text: ""; Layout.fillWidth: true }
                    Controls.Button {
                        text: qsTr("Save")
                        enabled: instrArea.text !== dialog.selOrigInstructions
                        onClicked: {
                            if (!dialog.sel) return
                            Mgr.saveRecipe(dialog.sel.id, dialog.recipeDto(dialog.sel, [
                                { setting: false, key: "instructions", value: instrArea.text }]))
                            dialog.selOrigInstructions = instrArea.text
                        }
                    }
                }

                // --- read-only structure ------------------------------------
                Controls.Label {
                    visible: !!(dialog.sel && dialog.sel.recipe && dialog.sel.recipe.extensions
                                && dialog.sel.recipe.extensions.length > 0)
                    text: qsTr("Extensions")
                    font.weight: Font.DemiBold
                }
                Controls.Label {
                    visible: !!(dialog.sel && dialog.sel.recipe && dialog.sel.recipe.extensions
                                && dialog.sel.recipe.extensions.length > 0)
                    text: dialog.sel ? (dialog.sel.recipe.extensions || [])
                          .map(e => "· " + e.name).join("\n") : ""
                    color: Kirigami.Theme.disabledTextColor
                    wrapMode: Text.Wrap
                    Layout.fillWidth: true
                }
                Controls.Label {
                    visible: !!(dialog.sel && dialog.sel.recipe && dialog.sel.recipe.sub_recipes
                                && dialog.sel.recipe.sub_recipes.length > 0)
                    text: qsTr("Sub-recipes")
                    font.weight: Font.DemiBold
                }
                Controls.Label {
                    visible: !!(dialog.sel && dialog.sel.recipe && dialog.sel.recipe.sub_recipes
                                && dialog.sel.recipe.sub_recipes.length > 0)
                    text: dialog.sel ? (dialog.sel.recipe.sub_recipes || [])
                          .map(e => "· " + e.name).join("\n") : ""
                    color: Kirigami.Theme.disabledTextColor
                    wrapMode: Text.Wrap
                    Layout.fillWidth: true
                }
                Controls.Label {
                    visible: !!(dialog.sel && dialog.sel.recipe && dialog.sel.recipe.parameters
                                && dialog.sel.recipe.parameters.length > 0)
                    text: qsTr("Parameters")
                    font.weight: Font.DemiBold
                }
                Controls.Label {
                    visible: !!(dialog.sel && dialog.sel.recipe && dialog.sel.recipe.parameters
                                && dialog.sel.recipe.parameters.length > 0)
                    text: dialog.sel ? (dialog.sel.recipe.parameters || []).map(p =>
                        "· " + p.key + (p.requirement === "optional"
                                        ? qsTr(" (optional)") : "")
                        + (p.description ? " — " + p.description : "")).join("\n") : ""
                    color: Kirigami.Theme.disabledTextColor
                    wrapMode: Text.Wrap
                    Layout.fillWidth: true
                }

                // --- Danger --------------------------------------------------
                Controls.Label { text: qsTr("Danger"); font.weight: Font.DemiBold }
                RowLayout {
                    Layout.fillWidth: true
                    Controls.Button {
                        text: qsTr("Delete")
                        icon.name: "edit-delete"
                        flat: true
                        onClicked: {
                            if (!dialog.sel) return
                            deleteConfirm.targetId = dialog.sel.id
                            deleteConfirm.targetTitle = dialog.sel.recipe && dialog.sel.recipe.title
                                                        ? dialog.sel.recipe.title : dialog.sel.id
                            deleteConfirm.open()
                        }
                    }
                    Controls.Label {
                        text: dialog.sel ? (dialog.sel.file_path || "") : ""
                        color: Kirigami.Theme.disabledTextColor
                        elide: Text.ElideMiddle
                        Layout.fillWidth: true
                    }
                }
            }
        }

        // Confirmation before deleting a recipe (its schedule, if any, stops working).
        Controls.Dialog {
            id: deleteConfirm
            property string targetId
            property string targetTitle
            title: qsTr("Delete recipe")
            modal: true
            standardButtons: Controls.Dialog.Ok | Controls.Dialog.Cancel
            closePolicy: Controls.Popup.CloseOnEscape
            onAccepted: {
                Mgr.deleteRecipe(targetId)
                if (targetId === dialog.selectedId) dialog.selectById("")
            }
            contentItem: Controls.Label {
                wrapMode: Text.Wrap
                text: qsTr("Delete recipe \"%1\"? Its schedule (if any) stops working.").arg(deleteConfirm.targetTitle)
            }
        }
    }
}
