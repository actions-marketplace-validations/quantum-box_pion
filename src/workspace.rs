//! The Cargo workspace as a graph of crates.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use camino::{Utf8Path, Utf8PathBuf};
use guppy::graph::{DependencyDirection, PackageGraph};
use guppy::{MetadataCommand, PackageId};
use serde::{Deserialize, Serialize};

use crate::change::RepoPath;

/// The name of a workspace crate, as written in its `Cargo.toml`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

    /// The repository path of a file named relative to the workspace root,
    /// the form a SCIP index uses.
    pub fn repo_path(&self, workspace_relative: &str) -> Option<RepoPath> {
        RepoPath::parse(self.root.join(workspace_relative).as_str()).ok()
    }

    /// The repository path of the workspace's `Cargo.lock`.
    pub fn lockfile(&self) -> Option<RepoPath> {
        self.repo_path("Cargo.lock")
    }

    /// The workspace crate called `name`, if there is one.
    pub fn package(&self, name: &str) -> Option<PackageName> {
        self.ids
            .keys()
            .find(|package| package.as_str() == name)
            .cloned()
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
            for dependent in self.tests_built_from(id)? {
                affected
                    .entry(dependent)
                    .or_insert_with(|| Dependency::DependsOn(origin.clone()));
            }
        }
        for package in changed {
            affected.insert(package.clone(), Dependency::Changed);
        }
        Ok(affected)
    }

    /// The workspace crates whose tests are built from the external package
    /// `name` at `version`, or `None` when the dependency graph has no such
    /// package.
    pub fn dependents_of_external(
        &self,
        name: &str,
        version: &str,
    ) -> Result<Option<BTreeSet<PackageName>>, WorkspaceError> {
        let mut dependents = BTreeSet::new();
        let mut found = false;
        for package in self.graph.packages() {
            if package.name() != name || package.version().to_string() != version {
                continue;
            }
            found = true;
            dependents.extend(self.tests_built_from(package.id())?);
        }
        Ok(found.then_some(dependents))
    }

    /// The workspace crates `package` is built from: itself and everything it
    /// depends on through normal and build dependencies. Its
    /// dev-dependencies are not part of what gets shipped, so they are left
    /// out.
    pub fn built_from(
        &self,
        package: &PackageName,
    ) -> Result<BTreeSet<PackageName>, WorkspaceError> {
        let id = self
            .ids
            .get(package)
            .ok_or_else(|| WorkspaceError::UnknownPackage(package.clone()))?;
        let closure = self
            .graph
            .query_forward([id])?
            .resolve_with_fn(|_, link| !link.dev_only());
        Ok(closure
            .packages(DependencyDirection::Forward)
            .filter(|dependency| dependency.in_workspace())
            .map(|dependency| PackageName(dependency.name().to_owned()))
            .collect())
    }

    /// The names of every package `package` is built from, workspace crates
    /// and external packages alike, through normal and build dependencies.
    pub fn built_from_names(
        &self,
        package: &PackageName,
    ) -> Result<BTreeSet<String>, WorkspaceError> {
        let id = self
            .ids
            .get(package)
            .ok_or_else(|| WorkspaceError::UnknownPackage(package.clone()))?;
        let closure = self
            .graph
            .query_forward([id])?
            .resolve_with_fn(|_, link| !link.dev_only());
        Ok(closure
            .packages(DependencyDirection::Forward)
            .map(|dependency| dependency.name().to_owned())
            .collect())
    }

    /// The workspace crates whose tests are built from a package called
    /// `name`, at any version. Empty when the graph has no such package:
    /// nothing is built from it.
    pub fn dependents_of_named(&self, name: &str) -> Result<BTreeSet<PackageName>, WorkspaceError> {
        let mut dependents = BTreeSet::new();
        for package in self.graph.packages() {
            if package.name() == name {
                dependents.extend(self.tests_built_from(package.id())?);
            }
        }
        Ok(dependents)
    }

    /// The directory of `package`, relative to the repository root.
    pub fn directory_of(&self, package: &PackageName) -> Option<&Utf8Path> {
        self.directories
            .iter()
            .find(|(_, owner)| *owner == package)
            .map(|(directory, _)| directory.as_path())
    }

    /// The repository path of the workspace's root `Cargo.toml`.
    pub fn root_manifest(&self) -> Option<RepoPath> {
        self.repo_path("Cargo.toml")
    }

    fn tests_built_from(&self, id: &PackageId) -> Result<BTreeSet<PackageName>, WorkspaceError> {
        let built_from = self
            .graph
            .query_reverse([id])?
            .resolve_with_fn(|_, link| !link.dev_only());
        let mut crates = BTreeSet::new();
        for package in built_from.packages(DependencyDirection::Reverse) {
            let direct_dependents = package.reverse_direct_links().map(|link| link.from());
            for dependent in std::iter::once(package).chain(direct_dependents) {
                if dependent.in_workspace() {
                    crates.insert(PackageName(dependent.name().to_owned()));
                }
            }
        }
        Ok(crates)
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
