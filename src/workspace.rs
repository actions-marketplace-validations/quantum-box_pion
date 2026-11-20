//! The Cargo workspace as a graph of crates.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use camino::{Utf8Path, Utf8PathBuf};
use guppy::graph::{DependencyDirection, PackageGraph};
use guppy::{MetadataCommand, PackageId};
use serde::Serialize;

use crate::change::RepoPath;

/// The name of a workspace crate, as written in its `Cargo.toml`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PackageName(String);

impl PackageName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PackageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("`cargo metadata` failed: {0}")]
    Metadata(#[from] guppy::Error),
    #[error("workspace crate `{0}` is not in the dependency graph")]
    UnknownPackage(PackageName),
    #[error("workspace root `{workspace}` is not inside the repository `{repository}`")]
    OutsideRepository {
        workspace: Utf8PathBuf,
        repository: Utf8PathBuf,
    },
}

/// The crates of one workspace and the dependencies between them.
pub struct WorkspaceGraph {
    graph: PackageGraph,
    /// The workspace root, relative to the repository root. Empty when the
    /// two are the same directory.
    root: Utf8PathBuf,
    /// Crate directory (relative to the repository root) -> crate.
    directories: BTreeMap<Utf8PathBuf, PackageName>,
    ids: BTreeMap<PackageName, PackageId>,
}

impl WorkspaceGraph {
    /// Runs `cargo metadata` for the workspace that owns `manifest_path`.
    /// `repository` is the root that changed paths are relative to.
    pub fn load(manifest_path: &Utf8Path, repository: &Utf8Path) -> Result<Self, WorkspaceError> {
        let mut command = MetadataCommand::new();
        command.manifest_path(manifest_path);
        let graph = command.build_graph()?;

        let workspace = graph.workspace().root();
        let root = workspace
            .strip_prefix(repository)
            .map_err(|_| WorkspaceError::OutsideRepository {
                workspace: workspace.to_owned(),
                repository: repository.to_owned(),
            })?
            .to_owned();

        let mut directories = BTreeMap::new();
        let mut ids = BTreeMap::new();
        for (path, package) in graph.workspace().iter_by_path() {
            let name = PackageName(package.name().to_owned());
            directories.insert(root.join(path), name.clone());
            ids.insert(name, package.id().clone());
        }
        Ok(Self {
            graph,
            root,
            directories,
            ids,
        })
    }

    /// The path of `path` inside the workspace root, or `None` when the file
    /// is outside the workspace.
    pub fn workspace_relative<'a>(&self, path: &'a RepoPath) -> Option<&'a Utf8Path> {
        path.as_path().strip_prefix(&self.root).ok()
    }

    /// Every workspace crate.
    pub fn packages(&self) -> BTreeSet<PackageName> {
        self.ids.keys().cloned().collect()
    }

    /// The crate whose directory contains `path`. Crates can nest, so the
    /// deepest directory wins.
    pub fn owner_of(&self, path: &RepoPath) -> Option<&PackageName> {
        path.as_path()
            .ancestors()
            .skip(1)
            .find_map(|dir| self.directories.get(dir))
    }

    /// The workspace crates whose tests can observe a change to `changed`.
    ///
    /// A crate's tests are built from the crate, its dev-dependencies, and
    /// everything those depend on through normal and build dependencies. So a
    /// change reaches every crate that depends on a changed crate through
    /// normal or build dependencies, plus the crates that have one of those
    /// as a direct dependency of any kind. A dev-dependency is followed for
    /// that last step only: it is not part of what the crate offers its own
    /// dependents.
    pub fn affected_by(
        &self,
        changed: &BTreeSet<PackageName>,
    ) -> Result<BTreeMap<PackageName, Dependency>, WorkspaceError> {
        let mut affected = BTreeMap::new();
        for origin in changed {
            let id = self
                .ids
                .get(origin)
                .ok_or_else(|| WorkspaceError::UnknownPackage(origin.clone()))?;
            let built_from_origin = self
                .graph
                .query_reverse([id])?
                .resolve_with_fn(|_, link| !link.dev_only());
            for package in built_from_origin.packages(DependencyDirection::Reverse) {
                let direct_dependents = package.reverse_direct_links().map(|link| link.from());
                for dependent in std::iter::once(package).chain(direct_dependents) {
                    if !dependent.in_workspace() {
                        continue;
                    }
                    affected
                        .entry(PackageName(dependent.name().to_owned()))
                        .or_insert_with(|| Dependency::DependsOn(origin.clone()));
                }
            }
        }
        for package in changed {
            affected.insert(package.clone(), Dependency::Changed);
        }
        Ok(affected)
    }
}

/// How a crate is tied to a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dependency {
    /// A file of the crate itself changed.
    Changed,
    /// The crate depends on a crate that changed. When several changed crates
    /// reach it, the first in name order is kept as the example.
    DependsOn(PackageName),
}
