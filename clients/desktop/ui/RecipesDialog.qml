import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// The full recipe surface, mirroring the Android RecipeScreens. A schedule is
// one of a recipe's settings, so the scheduler lives here, not on its own
// screen. Mgr.refreshRecipes() re-lists recipes AND schedules together.
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
    width: Math.min(700, parent ? parent.width - 32 : 700)
    height: Math.min(620, parent ? parent.height - 32 : 620)

    // "Running ..." after a run-now press (the reply is the finish, not the start).
    property string note: ""

    onOpened: Mgr.refreshRecipes()

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
            return qsTr("hourly, %1-%2").arg(hhmm(parseInt(ab[0], 10)), hhmm(parseInt(ab[1], 10)))
        }
        if (/^\d+$/.test(p[2]) && p[5] === "*") return qsTr("daily at %1").arg(hhmm(parseInt(p[2], 10)))
        if (/^\d+$/.test(p[2])) {
            const days = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
            for (const d of days)
                if (d.toLowerCase() === p[5].toLowerCase())
                    return qsTr("%1 at %2").arg(d, hhmm(parseInt(p[2], 10)))
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

    contentItem: ColumnLayout {
        spacing: Kirigami.Units.smallSpacing

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

        Controls.Label {
            visible: dialog.note.length > 0
            text: dialog.note
            color: Kirigami.Theme.disabledTextColor
            wrapMode: Text.Wrap
            Layout.fillWidth: true
        }

        ListView {
            id: recipeList
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            spacing: Kirigami.Units.smallSpacing
            model: Mgr.recipes
            Controls.ScrollBar.vertical: Controls.ScrollBar { policy: Controls.ScrollBar.AsNeeded }

            delegate: Rectangle {
                id: del
                width: ListView.view.width
                implicitHeight: delCol.implicitHeight + Kirigami.Units.largeSpacing * 2
                radius: Kirigami.Units.smallSpacing
                color: Kirigami.Theme.alternateBackgroundColor
                // separatorColor is absent on the 6.10 Platform theme; the disabled
                // foreground is the closest defined grey for a 1px card outline.
                border.color: Kirigami.Theme.separatorColor ? Kirigami.Theme.separatorColor
                                                            : Kirigami.Theme.disabledTextColor
                border.width: 1

                property var entry: modelData
                property var job: dialog.jobFor(modelData)
                property string origCron: modelData.schedule_cron || ""
                property string origModel: dialog.settingOf(modelData, "goose_model")
                property string origProvider: dialog.settingOf(modelData, "goose_provider")
                property string origPrompt: modelData.recipe && modelData.recipe.prompt
                                           ? modelData.recipe.prompt : ""
                property string origInstructions: modelData.recipe && modelData.recipe.instructions
                                                  ? modelData.recipe.instructions : ""

                ColumnLayout {
                    id: delCol
                    anchors.fill: parent
                    anchors.margins: Kirigami.Units.largeSpacing
                    spacing: Kirigami.Units.smallSpacing

                    RowLayout {
                        Layout.fillWidth: true
                        Controls.Label {
                            text: del.entry.recipe && del.entry.recipe.title
                                  ? del.entry.recipe.title : del.entry.id
                            font.weight: Font.DemiBold
                            Layout.fillWidth: true
                            elide: Text.ElideRight
                        }
                        Controls.Label {
                            text: dialog.statusLine(del.entry)
                            color: Kirigami.Theme.disabledTextColor
                            elide: Text.ElideRight
                        }
                    }

                    Controls.Label {
                        text: del.entry.recipe && del.entry.recipe.description
                              ? del.entry.recipe.description
                              : qsTr("No description")
                        color: Kirigami.Theme.disabledTextColor
                        wrapMode: Text.Wrap
                        Layout.fillWidth: true
                    }

                    // --- actions row: run, run-now, pause/resume ---------------
                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Kirigami.Units.smallSpacing
                        Controls.Button {
                            text: qsTr("Start session")
                            icon.name: "media-playback-start"
                            onClicked: Mgr.runRecipe(del.entry.id)
                        }
                        Controls.Button {
                            visible: !!del.job
                            text: qsTr("Run now")
                            flat: true
                            enabled: !!del.job && !del.job.currentlyRunning
                            onClicked: {
                                Mgr.runScheduleNow(del.job.id)
                                dialog.note = qsTr("Running — a briefing takes a few minutes and notifies if it has something.")
                            }
                        }
                        Controls.Switch {
                            visible: !!del.job
                            checked: !!del.job && !del.job.paused
                            onToggled: Mgr.setSchedulePaused(del.job.id, !checked)
                            Controls.ToolTip.visible: hovered
                            Controls.ToolTip.text: checked ? qsTr("Pause schedule") : qsTr("Enable schedule")
                        }
                        Item { Layout.fillWidth: true }
                    }

                    // --- Schedule ----------------------------------------------
                    Controls.Label { text: qsTr("Schedule"); font.weight: Font.DemiBold }
                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Kirigami.Units.smallSpacing
                        Controls.TextField {
                            id: cronField
                            Layout.fillWidth: true
                            placeholderText: qsTr("Cron: sec min hour day month weekday")
                            text: del.origCron
                        }
                        Controls.Button {
                            text: qsTr("Save")
                            enabled: cronField.text.trim() !== del.origCron
                            onClicked: {
                                Mgr.scheduleRecipe(del.entry.id, cronField.text.trim())
                                del.origCron = cronField.text.trim()
                            }
                        }
                        Controls.Button {
                            text: qsTr("Unschedule")
                            flat: true
                            visible: del.origCron.length > 0
                            onClicked: {
                                Mgr.scheduleRecipe(del.entry.id, "")
                                cronField.text = ""
                                del.origCron = ""
                            }
                        }
                    }
                    Controls.Label {
                        text: qsTr("Server local time.")
                        color: Kirigami.Theme.disabledTextColor
                        font.pointSize: Kirigami.Theme.defaultFont.pointSize * 0.85
                    }

                    // --- Model -------------------------------------------------
                    Controls.Label { text: qsTr("Model"); font.weight: Font.DemiBold }
                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Kirigami.Units.smallSpacing
                        Controls.TextField {
                            id: modelField
                            Layout.fillWidth: true
                            placeholderText: qsTr("Model (blank = server default)")
                            text: del.origModel
                        }
                        Controls.TextField {
                            id: providerField
                            Layout.fillWidth: true
                            placeholderText: qsTr("Provider (blank = server default)")
                            text: del.origProvider
                        }
                        Controls.Button {
                            text: qsTr("Save")
                            enabled: modelField.text.trim() !== del.origModel
                                     || providerField.text.trim() !== del.origProvider
                            onClicked: {
                                Mgr.saveRecipe(del.entry.id, dialog.recipeDto(del.entry, [
                                    { setting: true, key: "goose_model", value: modelField.text.trim() },
                                    { setting: true, key: "goose_provider", value: providerField.text.trim() }]))
                                del.origModel = modelField.text.trim()
                                del.origProvider = providerField.text.trim()
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
                    Controls.TextArea {
                        id: promptArea
                        Layout.fillWidth: true
                        implicitHeight: Math.min(160, contentHeight + 8)
                        wrapMode: Text.Wrap
                        text: del.origPrompt
                    }
                    RowLayout {
                        Controls.Label { text: ""; Layout.fillWidth: true }
                        Controls.Button {
                            text: qsTr("Save")
                            enabled: promptArea.text !== del.origPrompt
                            onClicked: {
                                Mgr.saveRecipe(del.entry.id, dialog.recipeDto(del.entry, [
                                    { setting: false, key: "prompt", value: promptArea.text }]))
                                del.origPrompt = promptArea.text
                            }
                        }
                    }

                    // --- Instructions --------------------------------------------
                    Controls.Label { text: qsTr("Instructions"); font.weight: Font.DemiBold }
                    Controls.TextArea {
                        id: instrArea
                        Layout.fillWidth: true
                        implicitHeight: Math.min(160, contentHeight + 8)
                        wrapMode: Text.Wrap
                        text: del.origInstructions
                    }
                    RowLayout {
                        Controls.Label { text: ""; Layout.fillWidth: true }
                        Controls.Button {
                            text: qsTr("Save")
                            enabled: instrArea.text !== del.origInstructions
                            onClicked: {
                                Mgr.saveRecipe(del.entry.id, dialog.recipeDto(del.entry, [
                                    { setting: false, key: "instructions", value: instrArea.text }]))
                                del.origInstructions = instrArea.text
                            }
                        }
                    }

                    // --- read-only structure ------------------------------------
                    Controls.Label {
                        visible: !!(del.entry.recipe && del.entry.recipe.extensions
                                    && del.entry.recipe.extensions.length > 0)
                        text: qsTr("Extensions")
                        font.weight: Font.DemiBold
                    }
                    Controls.Label {
                        visible: !!(del.entry.recipe && del.entry.recipe.extensions
                                    && del.entry.recipe.extensions.length > 0)
                        text: (del.entry.recipe.extensions || [])
                              .map(e => "· " + e.name).join("\n")
                        color: Kirigami.Theme.disabledTextColor
                        wrapMode: Text.Wrap
                        Layout.fillWidth: true
                    }
                    Controls.Label {
                        visible: !!(del.entry.recipe && del.entry.recipe.sub_recipes
                                    && del.entry.recipe.sub_recipes.length > 0)
                        text: qsTr("Sub-recipes")
                        font.weight: Font.DemiBold
                    }
                    Controls.Label {
                        visible: !!(del.entry.recipe && del.entry.recipe.sub_recipes
                                    && del.entry.recipe.sub_recipes.length > 0)
                        text: (del.entry.recipe.sub_recipes || [])
                              .map(e => "· " + e.name).join("\n")
                        color: Kirigami.Theme.disabledTextColor
                        wrapMode: Text.Wrap
                        Layout.fillWidth: true
                    }
                    Controls.Label {
                        visible: !!(del.entry.recipe && del.entry.recipe.parameters
                                    && del.entry.recipe.parameters.length > 0)
                        text: qsTr("Parameters")
                        font.weight: Font.DemiBold
                    }
                    Controls.Label {
                        visible: !!(del.entry.recipe && del.entry.recipe.parameters
                                    && del.entry.recipe.parameters.length > 0)
                        text: (del.entry.recipe.parameters || []).map(p =>
                            "· " + p.key + (p.requirement === "optional"
                                            ? qsTr(" (optional)") : "")
                            + (p.description ? " — " + p.description : "")).join("\n")
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
                                deleteConfirm.targetId = del.entry.id
                                deleteConfirm.targetTitle = del.entry.recipe && del.entry.recipe.title
                                                            ? del.entry.recipe.title : del.entry.id
                                deleteConfirm.open()
                            }
                        }
                        Controls.Label {
                            text: del.entry.file_path || ""
                            color: Kirigami.Theme.disabledTextColor
                            elide: Text.ElideMiddle
                            Layout.fillWidth: true
                        }
                    }
                }
            }

            footer: Controls.Label {
                visible: Mgr.recipes.length === 0
                text: qsTr("No recipes found.")
                color: Kirigami.Theme.disabledTextColor
                horizontalAlignment: Text.AlignHCenter
                width: recipeList.width
                padding: Kirigami.Units.largeSpacing
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
            onAccepted: Mgr.deleteRecipe(targetId)
            contentItem: Controls.Label {
                wrapMode: Text.Wrap
                text: qsTr("Delete recipe \"%1\"? Its schedule (if any) stops working.").arg(deleteConfirm.targetTitle)
            }
        }
    }
}
