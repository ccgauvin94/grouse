# grouse-unstable

The retirement boundary for the goose-fork compatibility shim: every
`_goose/unstable/*` method, plus the SDK's `unstable_elicitation` feature and the
goose-custom `status_message` / `message_usage` notifications.

This crate is temporary. It exists so existing goose-fork servers keep working
while the official goose GDK absorbs each feature upstream. Retire each method
(and move its consumers to the GDK surface) as it lands upstream.

## What this crate actually owns

- **`declare_grouse_unstable_listener!`** — the one declaration of
  `GrouseUnstableListener`. `grouse-core` invokes it in `lib.rs` at the old
  declaration site. The uniffi attribute expands *there*, which keeps the
  callback interface in the `grouse_core` bindings package: uniffi namespaces
  are per-crate, so exporting the trait directly from this crate would move it
  to `uniffi.grouse_unstable` and break every Kotlin/Swift import. The trait is
  therefore materialized in `grouse-core` and re-exported here, so both paths
  name the same type.
- **`UNSTABLE_INTENTS`** — the manifest of all 39 public `GrouseUnstable`
  methods (the constructor is not an intent). The disconnected no-op contract
  test and the manifest are generated from one macro invocation, so adding an
  intent to the inventory automatically adds its test case. Five intents that
  the old hand-written test omitted (`context_report`, `extension_catalog`,
  `tools_call_named`, `dictation_config`, `dictation_transcribe`) are now in it.

Implementations stay in `grouse-core/src/unstable.rs` — this crate is a
boundary, not a second client. `grouse-core` depends on this crate; this crate
depends back on `grouse-core` only for its tests (a dev-dependency cycle, which
Cargo permits), so the normal build graph stays acyclic.

`core/grouse-core/src/lib.rs` must invoke the declaration macro and re-export
`UNSTABLE_INTENTS`; without that adaptation the core will not compile. See the
handoff notes for the exact replacement text.
