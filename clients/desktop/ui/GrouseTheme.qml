// SPDX-License-Identifier: AGPL-3.0-or-later

// Grouse design tokens, hand-synced singleton (design/tokens.json v1.1.0).
// Only the surfaces the design-language.md §4 flags are tokenized here; the
// rest of the app drives through Kirigami.Theme.colorScheme so every standard
// control picks up the platform palette. See design/design-language.md.
//
// The decoded-token groups are `var` JS objects, NOT nested `QtObject{}`
// literals: inline anonymous QtObjects assigned to grouped properties hit a
// qmlcachegen (AOT) regression in the Qt 6.10 SDK that miscompiles the leaf
// property assignments as signals ("Cannot assign a value to a signal").
// Plain JS objects sidestep that path entirely and expose the same
// `Theme.semantic.light.status.online` accessors.
//
// Values are kept in lock-step with design/tokens.json — the Android project
// pins the same values in its ThemeTokensTest. A code generator does not exist
// for the Qt platform, so this is the hand-maintained Qt adaptation the
// design doc prescribes.

pragma Singleton
import QtQuick

QtObject {
    property string grouseVersion: "1.1.0"

    // radius.scale (tokens.json). Digit-leading keys become twoXl..fiveXl.
    property var radius: ({
        none: 0,
        xs: 8,
        sm: 10,
        md: 12,
        lg: 14,
        xl: 16,
        twoXl: 20,
        threeXl: 26,
        fourXl: 28,
        fiveXl: 32,
        full: 999
    })

    // spacing.scale (tokens.json, 4dp unit). Digit keys become s1..s16.
    property var spacing: ({
        zero: 0,
        half: 2,
        s1: 4,
        s2: 8,
        s3: 12,
        s4: 16,
        s5: 20,
        s6: 24,
        s8: 32,
        s10: 40,
        s12: 48
    })

    // semantic (tokens.json) — light/dark per scheme; only `.status.*` is
    // consumed by the app today, the rest is retained in lock-step with the
    // token source for future surfaces.
    property var semantic: ({
        light: {
            background: "#FAF7F0",
            surface: "#FDFBF4",
            primary: "#4C662B",
            onPrimary: "#FFFFFF",
            text: "#282828",
            textSecondary: "#6F6B61",
            secondary: "#586249",
            accent: "#FABD2F",
            danger: "#FB4934",
            chatUser: { fill: "#CDEDA3", text: "#102000" },
            chatAgent: { fill: "#FAF7F0", text: "#282828" },
            status: { online: "#3DDC84", connecting: "#F5A623", offline: "#FB4934" }
        },
        dark: {
            background: "#282828",
            surface: "#3C3836",
            primary: "#B1D18A",
            onPrimary: "#1F3701",
            text: "#EBDBB2",
            textSecondary: "#A89984",
            secondary: "#BFCBAD",
            accent: "#FABD2F",
            danger: "#FB4934",
            chatUser: { fill: "#354E16", text: "#CDEDA3" },
            chatAgent: { fill: "#282828", text: "#EBDBB2" },
            status: { online: "#3DDC84", connecting: "#F5A623", offline: "#FB4934" }
        }
    })
}
