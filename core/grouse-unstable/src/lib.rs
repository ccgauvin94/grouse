//! grouse-unstable: the goose-fork `_goose/unstable/*` compatibility shim.
//!
//! The `GrouseUnstable` interface currently lives in `grouse-core` (next to the
//! stable `Core`) so both uniffi interfaces share one scaffolding unit and one
//! set of shared types. This crate is the retirement boundary: as the GDK
//! absorbs each unstable feature, its method leaves `GrouseUnstable` and,
//! eventually, the whole interface moves here or disappears.

pub use grouse_core::{GrouseUnstable, GrouseUnstableListener};
