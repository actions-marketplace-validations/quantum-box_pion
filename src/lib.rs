//! pion selects the tests a change can affect.
//!
//! The first level of resolution is the Cargo dependency graph: a change to a
//! crate can affect that crate and every workspace crate that depends on it.
//! Finer levels (definitions inside a crate, API scenarios) are described in
//! `docs/design.md` and build on the same [`plan::TestPlan`].

pub mod change;
pub mod git;
pub mod plan;
pub mod workspace;

pub use change::{ChangeSet, ChangedFile, FileKind, RepoPath};
pub use plan::{FallbackCause, Reason, Selection, TestPlan};
pub use workspace::{PackageName, WorkspaceGraph};
