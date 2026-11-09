//! The client-visible surface, as DATA — the input to the parity ratchet
//! (`docs/PARITY.md`, `scripts/check-parity.py`).
//!
//! Everything listed here crosses the core boundary into BOTH clients: a record
//! field a client reads off `Item`, a variant of the transcript stream it must
//! render, a callback it must answer. The Rust compiler cannot check the other
//! side of that boundary — Kotlin reads these as uniffi records, QML as JSON —
//! so a field the core grows is silently invisible to a client that has never
//! heard of it. That is the defect class this module exists to make loud:
//! `Item::files` shipped on the phone and did not exist on the desktop for a
//! release, and nothing failed.
//!
//! The inventory is checked twice, and neither check is a promise:
//!
//! 1. **At compile time**, by an exhaustive destructuring pattern with no `..`
//!    rest. Adding a field to `Item`, a variant to `TranscriptOp`, or a variant
//!    to `ItemKind` breaks THIS build ("pattern does not mention field `foo`" /
//!    "non-exhaustive patterns: `Foo::Bar` not covered"), and the fix is to add
//!    it to the macro call below — which is the same list the CI checker reads.
//!    So the inventory cannot quietly fall behind the types.
//! 2. **In CI**, by `scripts/check-parity.py`: every id here must carry a
//!    disposition in `docs/PARITY.md` for every active client, and a `yes`
//!    disposition must point at a token that really exists in that client's
//!    source. An `excluded:` needs a reason and moves a pinned counter, so
//!    opting a client out is a visible act in a diff rather than an omission.
//!
//! `LISTENER_EVENTS` is the one hand-kept list: the `CoreListener` trait gives
//! every callback a default body, so an added `fn on_*` compiles fine without
//! this module noticing. The checker therefore parses the trait out of `lib.rs`
//! and refuses to run if the two disagree, which closes that hole for free.
//!
//! Ids are the ledger's primary keys: append, never renumber.

/// A struct whose fields both clients render, with a compile-time completeness
/// guard. Emits `PRE.id`, `PRE.field`, … in declaration order.
macro_rules! record_surface {
    ($ty:ident, $name:ident, $prefix:literal : $($field:ident),+ $(,)?) => {
        pub static $name: &[&str] = &[$(concat!($prefix, ".", stringify!($field))),+];

        // The guard. `let Item { a, b } = v;` with NO `..` rest fails to
        // compile the moment a field is added, which is what keeps `$name`
        // honest — the list and the pattern are written once, in one call.
        const _: () = {
            #[allow(dead_code, unused_variables)]
            fn exhaustive(v: &$ty) {
                let $ty { $($field),+ } = v;
            }
        };
    };
}

use crate::attachments::UploadedFile;
use crate::{ActiveWork, Item, ItemKind, SubagentStep, ToolCall, TranscriptOp};

/// The same for an enum whose variants all carry fields (`{ .. }` matches them).
macro_rules! struct_enum_surface {
    ($ty:ident, $name:ident, $prefix:literal : $($variant:ident),+ $(,)?) => {
        pub static $name: &[&str] = &[$(concat!($prefix, ".", stringify!($variant))),+];

        const _: () = {
            // No `_` arm: a new variant is a non-exhaustive match.
            #[allow(dead_code)]
            fn exhaustive(v: &$ty) {
                match v {
                    $($ty::$variant { .. } => {}),+
                }
            }
        };
    };
}

/// The same for an enum of unit variants (the bare path is the pattern that is
/// valid for every one of them).
macro_rules! unit_enum_surface {
    ($ty:ident, $name:ident, $prefix:literal : $($variant:ident),+ $(,)?) => {
        pub static $name: &[&str] = &[$(concat!($prefix, ".", stringify!($variant))),+];

        const _: () = {
            #[allow(dead_code)]
            fn exhaustive(v: &$ty) {
                match v {
                    $($ty::$variant => {}),+
                }
            }
        };
    };
}

record_surface!(
    Item,
    ITEM_FIELDS,
    "item":
    id,
    kind,
    text,
    detail,
    output,
    status,
    app_key,
    calls,
    thought,
    summary,
    steps,
    attachments,
    files,
    images,
    message_id,
    seq,
);

record_surface!(
    ToolCall,
    CALL_FIELDS,
    "call":
    id,
    title,
    detail,
    output,
    status,
    thought,
    seq,
    steps,
);

record_surface!(
    SubagentStep,
    STEP_FIELDS,
    "step":
    name,
    detail,
);

record_surface!(
    ActiveWork,
    WORK_FIELDS,
    "work":
    id,
    title,
    detail,
    status,
    running,
    subagent,
    steps,
);

record_surface!(
    UploadedFile,
    FILE_FIELDS,
    "file":
    name,
    mime,
    bytes,
    path,
    sha256,
);

struct_enum_surface!(
    TranscriptOp,
    TRANSCRIPT_OPS,
    "op":
    Reset,
    Upsert,
    AppendText,
    AppendOutput,
    Remove,
    Window,
);

unit_enum_surface!(
    ItemKind,
    ITEM_KINDS,
    "kind":
    User,
    Agent,
    Thought,
    Tool,
    ToolGroup,
    Chart,
    McpApp,
    File,
    Error,
);

/// Every [`crate::CoreListener`] callback, in trait order.
///
/// Hand-kept, because the trait's defaults mean an added `fn on_*` needs no
/// call-site change. `scripts/check-parity.py` parses the trait and fails if
/// this list and the trait disagree — so the list is a copy, not a second
/// source of truth.
pub static LISTENER_EVENTS: &[&str] = &[
    "listener.on_status",
    "listener.on_sessions",
    "listener.on_item",
    "listener.on_usage",
    "listener.on_run_ended",
    "listener.on_speak",
    "listener.on_config",
    "listener.on_permission_request",
    "listener.on_session_touched",
    "listener.on_projects",
    "listener.on_roam_peer_status",
    "listener.on_roam_sessions",
    "listener.on_peer_new_session",
    "listener.on_active_run",
    "listener.on_commands",
    "listener.on_session_open_failed",
    "listener.on_tasks",
    "listener.on_thread_ready",
    "listener.on_replay_progress",
    "listener.on_file",
];

/// Every id in the inventory, in one list. What the checker reads.
pub fn all() -> Vec<&'static str> {
    let mut out = Vec::new();
    for group in [
        ITEM_FIELDS,
        CALL_FIELDS,
        STEP_FIELDS,
        WORK_FIELDS,
        FILE_FIELDS,
        TRANSCRIPT_OPS,
        ITEM_KINDS,
        LISTENER_EVENTS,
    ] {
        out.extend_from_slice(group);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// One id per line, no duplicates, no empties. Cheap, but it is the shape
    /// the checker assumes and a duplicate would make a ledger row ambiguous.
    #[test]
    fn ids_are_unique_and_prefixed() {
        let ids = all();
        assert!(ids.len() > 50, "inventory looks truncated: {}", ids.len());
        let mut seen = HashSet::new();
        for id in &ids {
            assert!(!id.is_empty());
            let (prefix, rest) = id.split_once('.').expect("an id is `group.name`");
            // i(tem) / c(all) / s(tep) / w(ork) / f(ile) / o(p) / k(ind) / l(istener).
            assert!(['i', 'c', 's', 'w', 'f', 'o', 'k', 'l'].contains(&prefix.chars().next().unwrap()));
            assert!(!rest.is_empty(), "empty name in {id}");
            assert!(seen.insert(*id), "duplicate id: {id}");
        }
    }

    /// The groups the checker requires, so a rename of a const is caught here
    /// (in `cargo test`) rather than by a red CI job that has no compiler.
    #[test]
    fn the_groups_the_checker_knows_are_all_present() {
        for (group, n) in [
            (ITEM_FIELDS, 16),
            (CALL_FIELDS, 8),
            (STEP_FIELDS, 2),
            (FILE_FIELDS, 5),
            (TRANSCRIPT_OPS, 6),
            (ITEM_KINDS, 9),
            (LISTENER_EVENTS, 20),
        ] {
            assert_eq!(group.len(), n, "{} changed size: {:?}", group.len(), group);
        }
    }
}
