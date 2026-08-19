# Grouse Design Language

The single source of truth for how Grouse looks across every native client. The tokens live in
[`tokens.json`](./tokens.json); this document explains the *why*, the naming rules, and how each
platform renders the shared tokens with its native toolkit.

> **One contract, native rendering.** Tokens describe *what* a surface is (a color role, a size
> step, a corner radius). They never describe *how* to draw it. Each platform maps a token to its
> own mechanism — a QML singleton, a Compose `MaterialTheme`, a SwiftUI `Color` extension, a TUI
> palette table — so the UI stays idiomatic to each toolkit while remaining visually one product.

---

## 1. Look and feel

The identity is **gruvbox warmth on paper** — a dark, warm, low-contrast neutral ramp paired with a
cream paper light mode, and a goose-green accent. It is soft, rounded, and chat-first.

**Palette.** The brand colors come straight from grouse-android's launcher icon and window frame:

| Role | Light | Dark | Source |
|---|---|---|---|
| Background | `#FAF7F0` (paper) | `#282828` (gruvbox bg) | `res/values/colors.xml` → `window_bg`; `res/values-night/colors.xml` |
| Primary / accent | `#4C662B` (goose green) | `#B1D18A` | `ui/theme/Color.kt` → `Green40` / `Green80` |
| Accent (brand yellow) | `#FABD2F` | `#FABD2F` | `drawable/ic_launcher_foreground.xml` (beak) |
| Danger / brand red | `#FB4934` | `#FB4934` | `drawable/ic_launcher_foreground.xml` (comb) |
| Text | `#282828` (warm ink) | `#EBDBB2` (gruvbox fg) | gruvbox ramp (icon body `#ebdbb2`) |

The grouse icon is literally the brand: a plump `#ebdbb2` goose on a `#282828` field, with a
`#fabd2f` beak and a `#fb4934` eyebrow-comb — see the `<path>` fills in
`app/src/main/res/drawable/ic_launcher_foreground.xml`. Those four hexes (`#282828`, `#ebdbb2`,
`#fabd2f`, `#fb4934`) are the raw palette seed; everything else is derived from them plus the
cream paper and the goose-green fallback.

**Warmth over neutrality.** The app deliberately avoids blue-grey Material defaults. The light
mode is *cream* (`#FAF7F0`), not white; the dark mode is *gruvbox brown-black* (`#282828`), not
`#000`. Muted/secondary text is a warm grey (`#A89984` in dark mode), never a cool slate. New
surfaces should keep that warm cast — when in doubt, tint toward yellow, not blue.

**Rounded chat bubbles.** Chat is the centerpiece. The user bubble is asymmetric — a rounded
rectangle with one tight corner on the bottom-left, like a message tail — expressed in Compose as
`RoundedCornerShape(16.dp, 16.dp, 4.dp, 16.dp)` in `Screens.kt` (`UserBubble`). The assistant
renders as *plain text on the background* with no bubble at all, so the two sides read as two
different voices rather than two mirrored boxes. Tool calls collapse into soft 14dp "chips"
(`secondaryContainer` at 55% alpha); errors get a compact 8dp red container. The composer at the
bottom is one big 28dp rounded, subtly-outlined field (modelled on Claude's), *not* a Material
text field with its own inner surface.

**Drawer navigation.** Primary navigation is a modal navigation drawer (`ModalNavigationDrawer` in
`MainActivity.kt`), not bottom tabs. Sections are uppercase `labelMedium` headers in the primary
color (`PROJECTS`, `CHATS`, `REMOTE`), with list rows and rounded 16dp cards beneath. It reads as
a warm, organized index of sessions rather than a bare menu.

**Screenshots.** The two captures in the Android repo record this look in dark mode:

- `docs/screenshots/drawer.png` — the drawer on the warm dark surface, with the primary-tinted
  section headers and carded rows.
- `docs/screenshots/chat.png` — the chat: right-aligned user bubbles, left-aligned assistant text,
  tool chips, and the rounded composer pinned at the bottom.

Because these captures were taken on Android 12+, they show **Material You dynamic color** (the
surface samples to warm browns like `#211A13` on a `#100C08` background, with a `#F8BB71` accent)
rather than the fixed goose-green fallback. The token palette in `tokens.json` is the *fixed*
reference (what the app looks like on API < 31 and what every other platform must match); Android
12+ keeps dynamic color as an opt-in enhancement that overrides `semantic.*`, never the raw brand
ramp.

---

## 2. Token groups

`tokens.json` has four canonical groups, plus a `component` group for the surfaces built from them:

1. **`color`** — `raw` (the fixed brand palette) and `semantic` (role-based `light` / `dark`
   schemes). Semantic roles: `background`, `surface`, `primary`, `text`, `textSecondary`,
   `secondary`, `accent`, `danger`, `chatUser`, `chatAgent`, plus a `status` sub-role
   (`online` / `connecting` / `offline`) for connection-state dots. `raw` additionally carries
   `chart.series` — the fixed multi-series palette servers render into chart specs (shared with
   the desktop's `ChartBubble.qml`).
2. **`type`** — `family` and the Material 3 `scale` (size in sp, weight, line-height). grouse-android
   ships the stock M3 scale (`ui/theme/Type.kt` → `GooseTypography = Typography()`); we pin those
   values as tokens so the other platforms match, and leave the scale open to tuning later without
   touching per-platform wiring.
3. **`spacing`** — a 4dp base scale (`0 … 64`) plus a `half` (2dp) step. The chat surface is
   allowed tighter 2dp increments (6/10/14/18dp) for bubble/list density, as used throughout
   `Screens.kt`.
4. **`radius`** — a named corner scale (`none … full`), anchored on the M3 `Shapes` overrides in
   `ui/theme/Theme.kt` (`extraSmall 10 / small 14 / medium 20 / large 26 / extraLarge 32`) and the
   component-specific corners in `Screens.kt` (composer 28, user-bubble tail, tool chip 14).

**`component`** folds the recurring surfaces into named tokens — `chatBubble` (user/agent/tool/
error), `composer`, `chip`, `drawer` — so each platform renders the *same* component semantics
without re-deriving them from raw color/radius values.

---

## 3. Naming rules

- **Semantic over cosmetic.** Name a token by what it *is for*, never by its value or its color.
  `primary`, `textSecondary`, `chatUser.fill` — never `green40` in UI code (that's a raw palette
  name only).
- **kebab/lowerCamel by platform.** JSON uses `camelCase` keys (`textSecondary`, `chatUser`). Each
  platform maps to its own idiom (see §4) but keeps the *meaning* identical.
- **`on*` reads "text/icon drawn on top of".** `onPrimary` is the content color for a `primary`
  fill. Every filled role gets an `on*` partner.
- **Light/dark are separate schemes**, both fully specified. Never special-case a single color in
  a component; pick a role and let the scheme supply both variants.
- **The raw palette is a source, not a target.** UI code consumes `semantic.*`. `raw.*` exists so
  a designer or a new platform can re-derive the scheme and see the provenance; it is not for
  direct use in screens.
- **Scale steps are named, not numeric, in code.** `spacing.4`, `radius.xl` — a rename of a step
  must not force a value change at every call site.
- **No magic numbers.** If a component needs a value outside the scale, add a step to the scale
  first, then use it. The chat's 2dp "tight" increments are the one sanctioned exception, and they
  are documented in the token's `note`.

---

## 4. Native adaptation

The tokens are shared; the rendering is native. Each platform maps `tokens.json` to its toolkit
idiom:

### Kirigami / Qt (desktop)

- Generate a **QML singleton** (e.g. `Grouse.Theme`) from the JSON at build time — expose
  `Grouse.Theme.color.semantic.dark.background`, `Grouse.Theme.spacing[4]`,
  `Grouse.Theme.radius.xl` as read-only properties.
- Drive the whole app through **`Kirigami.Theme.colorScheme`** and
  `Kirigami.Theme.inherit` / `colorSet` so every control (buttons, dialogs, cards) picks up the
  palette automatically: set `backgroundColor`/`backgroundColor2` to `semantic.background`, the
  positive/active hues to `primary`/`accent`, negative to `danger`. Add a
  `QML ColorImageProvider` for the two scheme variants and let `Kirigami.Theme.colorSet` swap them.
- Map the M3 type scale onto `Kirigami.Theme` font roles (`defaultFont`, `smallFont`) or explicit
  `font.pointSize` from the sp values; radii map to `Kirigami.Units.cornerRadius` and
  `PlasmaCore.Units.smallSpacing`-style spacing.

### Compose (Android)

- Replace the hand-rolled `FallbackLight`/`FallbackDark` in `ui/theme/Color.kt` with values read
  from the tokens, then feed them into `lightColorScheme`/`darkColorScheme` as today's `GooseTheme`
  already does in `ui/theme/Theme.kt`.
- Keep the `Shapes(...)` override as the radius mapping (`extraSmall → radius.sm`, `small → lg`,
  `medium → 2xl`, `large → 3xl`, `extraLarge → 5xl`) and the `GooseTypography` override for the
  type scale.
- Material You dynamic color stays as the Android-only enhancement: it overrides `semantic.*`
  when available (`dynamicColor && SDK_INT >= S`), exactly as the current theme does.

### SwiftUI (macOS, later)

- Define a `Color` extension (`extension Color { static let grouseBackground = … }`) or an
  `AssetCatalog`/`Color.xcassets` generated from `semantic.*`, and an `NSColor`/`UIColor`
  adaptation for the dark variant via `Color(light:dark:)` or dynamic color providers.
- Type scale → `Font.custom`/system sizes in points (1sp ≈ 1pt at default scale); spacing/radius
  stay in points.

### TUI (CLI, Rust)

- Compile the palette into a small static table (or embed the JSON via `include_str!`) and expose
  `ansi`/`ratatui` `Color` constants: `background → terminal default`, `primary/accent → a 256-color
  or truecolor pick (e.g. goose green `#4C662B`), `danger → red`, `textSecondary → a dim/gray
  attribute`. Map "bubble" semantics to indentation/padding rather than fills, and radii are a
  no-op (terminals have square cells).
