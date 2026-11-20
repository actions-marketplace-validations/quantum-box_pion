//! What changed between two revisions, classified by how the change spreads.

use std::collections::BTreeSet;
use std::fmt;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use crate::workspace::{PackageName, WorkspaceGraph};

/// A path relative to the repository root, with `/` separators.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RepoPath(Utf8PathBuf);

/// Why a string is not a repository-relative path.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RepoPathError {
    #[error("path is empty")]
    Empty,
    #[error("path `{0}` is absolute")]
    Absolute(String),
    #[error("path `{0}` leaves the repository")]
    Escapes(String),
}

impl RepoPath {
    /// Accepts the paths `git diff --name-only` prints: relative, normalized,
    /// never pointing outside the repository.
    pub fn parse(raw: &str) -> Result<Self, RepoPathError> {
        if raw.is_empty() {
            return Err(RepoPathError::Empty);
        }
        let path = Utf8Path::new(raw);
        if path.is_absolute() {
            return Err(RepoPathError::Absolute(raw.to_owned()));
        }
        if path.components().any(|part| part.as_str() == "..") {
            return Err(RepoPathError::Escapes(raw.to_owned()));
        }
        Ok(Self(path.to_owned()))
    }

    pub fn as_path(&self) -> &Utf8Path {
        &self.0
    }

    pub fn file_name(&self) -> Option<&str> {
        self.0.file_name()
    }
}

impl fmt::Display for RepoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.as_str().fmt(f)
    }
}

/// How a changed file can affect tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileKind {
    /// A `.rs` file inside a workspace crate.
    RustSource { package: PackageName },
    /// A crate's `Cargo.toml` or `build.rs`: compile-time inputs of the crate.
    CrateManifest { package: PackageName },
    /// Any other file inside a crate (SQL, GraphQL, fixtures, snapshots).
    /// Macros such as `include_str!` and `sqlx::query!` read these at compile
    /// time, so they count as input of the owning crate.
    CrateData { package: PackageName },
    /// A file that changes how every crate builds.
    WorkspaceGlobal,
    /// A file no workspace crate owns.
    Outside,
}

impl FileKind {
    /// The crate whose tests this file can affect directly.
    pub fn package(&self) -> Option<&PackageName> {
        match self {
            Self::RustSource { package }
            | Self::CrateManifest { package }
            | Self::CrateData { package } => Some(package),
            Self::WorkspaceGlobal | Self::Outside => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangedFile {
    pub path: RepoPath,
    #[serde(flatten)]
    pub kind: FileKind,
}

/// The files that differ between a base revision and a head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeSet {
    files: Vec<ChangedFile>,
}

impl ChangeSet {
    /// Classifies each path against the workspace. Duplicate paths collapse.
    pub fn classify(paths: impl IntoIterator<Item = RepoPath>, graph: &WorkspaceGraph) -> Self {
        let unique: BTreeSet<RepoPath> = paths.into_iter().collect();
        let files = unique
            .into_iter()
            .map(|path| {
                let kind = classify_path(&path, graph);
                ChangedFile { path, kind }
            })
            .collect();
        Self { files }
    }

    pub fn files(&self) -> &[ChangedFile] {
        &self.files
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Files at the workspace root that change how every crate builds.
const GLOBAL_FILES: [&str; 5] = [
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "rust-toolchain",
    "clippy.toml",
];
const GLOBAL_DIRS: [&str; 1] = [".cargo"];

fn classify_path(path: &RepoPath, graph: &WorkspaceGraph) -> FileKind {
    let Some(in_workspace) = graph.workspace_relative(path) else {
        return FileKind::Outside;
    };
    let is_global_dir = GLOBAL_DIRS.iter().any(|dir| in_workspace.starts_with(dir));
    if is_global_dir || GLOBAL_FILES.contains(&in_workspace.as_str()) {
        return FileKind::WorkspaceGlobal;
    }
    let Some(package) = graph.owner_of(path) else {
        return FileKind::Outside;
    };
    let package = package.clone();
    match path.file_name() {
        Some("Cargo.toml" | "build.rs") => FileKind::CrateManifest { package },
        Some(name) if name.ends_with(".rs") => FileKind::RustSource { package },
        _ => FileKind::CrateData { package },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_path_rejects_paths_outside_the_repository() {
        assert_eq!(RepoPath::parse(""), Err(RepoPathError::Empty));
        assert_eq!(
            RepoPath::parse("/etc/passwd"),
            Err(RepoPathError::Absolute("/etc/passwd".into()))
        );
        assert_eq!(
            RepoPath::parse("a/../../b"),
            Err(RepoPathError::Escapes("a/../../b".into()))
        );
        assert!(RepoPath::parse("packages/auth/src/lib.rs").is_ok());
    }
}
