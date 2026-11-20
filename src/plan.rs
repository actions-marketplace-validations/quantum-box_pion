//! The test plan: which tests to run for a change, and why.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::change::{ChangeSet, FileKind, RepoPath};
use crate::workspace::{Dependency, PackageName, WorkspaceError, WorkspaceGraph};

/// Which tests of a crate to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Selection {
    /// Every test of the crate.
    All,
}

/// Why a crate's tests were selected. Every selection carries its reasons so
/// that the plan can be explained and audited.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Reason {
    /// A file of the crate changed.
    FileChanged { path: RepoPath },
    /// The crate depends on a crate that changed.
    DependsOnCrate { package: PackageName },
    /// Selection could not be narrowed and fell back to something wider.
    FellBack { cause: FallbackCause },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "cause", rename_all = "snake_case")]
pub enum FallbackCause {
    /// A file that changes how every crate builds.
    WorkspaceGlobal { path: RepoPath },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CratePlan {
    pub selection: Selection,
    pub reasons: Vec<Reason>,
}

/// The tests to run for one [`ChangeSet`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TestPlan {
    /// Crates whose tests run. Crates not listed run nothing.
    pub crates: BTreeMap<PackageName, CratePlan>,
    /// Changed files no rule covers. They select no test; a caller that gates
    /// a deployment on this plan should treat a non-empty list as a failure.
    pub uncovered: Vec<RepoPath>,
}

impl TestPlan {
    /// Plans at crate resolution: a changed crate and every workspace crate
    /// that depends on it run all their tests.
    pub fn for_changes(
        changes: &ChangeSet,
        graph: &WorkspaceGraph,
    ) -> Result<Self, WorkspaceError> {
        let mut reasons: BTreeMap<PackageName, BTreeSet<Reason>> = BTreeMap::new();
        let mut changed_crates = BTreeSet::new();
        let mut global = Vec::new();
        let mut uncovered = Vec::new();

        for file in changes.files() {
            match &file.kind {
                FileKind::WorkspaceGlobal => global.push(file.path.clone()),
                FileKind::Outside => uncovered.push(file.path.clone()),
                kind => {
                    // The remaining kinds all belong to a crate.
                    let Some(package) = kind.package() else {
                        continue;
                    };
                    changed_crates.insert(package.clone());
                    reasons
                        .entry(package.clone())
                        .or_default()
                        .insert(Reason::FileChanged {
                            path: file.path.clone(),
                        });
                }
            }
        }

        for (package, dependency) in graph.affected_by(&changed_crates)? {
            if let Dependency::DependsOn(origin) = dependency {
                reasons
                    .entry(package)
                    .or_default()
                    .insert(Reason::DependsOnCrate { package: origin });
            }
        }

        for path in global {
            for package in graph.packages() {
                reasons
                    .entry(package)
                    .or_default()
                    .insert(Reason::FellBack {
                        cause: FallbackCause::WorkspaceGlobal { path: path.clone() },
                    });
            }
        }

        let crates = reasons
            .into_iter()
            .map(|(package, reasons)| {
                let plan = CratePlan {
                    selection: Selection::All,
                    reasons: reasons.into_iter().collect(),
                };
                (package, plan)
            })
            .collect();
        Ok(Self { crates, uncovered })
    }

    /// A nextest filterset that runs exactly the selected crates, or `None`
    /// when no test is selected.
    pub fn nextest_filterset(&self) -> Option<String> {
        if self.crates.is_empty() {
            return None;
        }
        let terms: Vec<String> = self
            .crates
            .keys()
            .map(|package| format!("package(={package})"))
            .collect();
        Some(terms.join(" + "))
    }
}
