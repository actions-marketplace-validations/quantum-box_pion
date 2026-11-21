//! Release decisions against the fixture workspace in
//! `tests/fixtures/workspace`:
//!
//! ```text
//! fx-api --> fx-auth --> fx-core
//!              `--(dev)--> fx-tools
//! ```

use std::collections::BTreeSet;

use camino::Utf8PathBuf;
use pion::lockfile::Lockfile;
use pion::manifest::ManifestChange;
use pion::reach::{LockPair, Reach, ReachReason, ReachReport};
use pion::{ChangeSet, PackageName, RepoPath, WorkspaceGraph};

fn fixture() -> WorkspaceGraph {
    let root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/workspace")
        .canonicalize_utf8()
        .expect("fixture workspace exists");
    WorkspaceGraph::load(&root.join("Cargo.toml"), &root).expect("fixture workspace loads")
}

fn changes(graph: &WorkspaceGraph, paths: &[&str]) -> ChangeSet {
    let paths = paths
        .iter()
        .map(|path| RepoPath::parse(path).expect("valid fixture path"));
    ChangeSet::classify(paths, graph)
}

fn packages(graph: &WorkspaceGraph, names: &[&str]) -> BTreeSet<PackageName> {
    names
        .iter()
        .map(|name| graph.package(name).expect("fixture crate"))
        .collect()
}

const ALL: [&str; 4] = ["fx-api", "fx-auth", "fx-core", "fx-tools"];

fn reached(report: &ReachReport) -> Vec<&str> {
    report
        .packages
        .iter()
        .filter(|(_, reach)| reach.reached)
        .map(|(package, _)| package.as_str())
        .collect()
}

#[test]
fn a_change_reaches_the_binaries_built_from_the_crate() {
    let graph = fixture();
    let report = Reach::new(&graph)
        .of(
            &changes(&graph, &["core/src/token.rs"]),
            &packages(&graph, &ALL),
        )
        .expect("reach");

    assert_eq!(reached(&report), ["fx-api", "fx-auth", "fx-core"]);
}

#[test]
fn a_dev_dependency_is_not_shipped() {
    let graph = fixture();
    let report = Reach::new(&graph)
        .of(
            &changes(&graph, &["tools/src/lib.rs"]),
            &packages(&graph, &ALL),
        )
        .expect("reach");

    // fx-auth uses fx-tools in its tests only.
    assert_eq!(reached(&report), ["fx-tools"]);
}

#[test]
fn an_integration_test_is_not_shipped() {
    let graph = fixture();
    let report = Reach::new(&graph)
        .of(
            &changes(&graph, &["auth/tests/flow.rs"]),
            &packages(&graph, &ALL),
        )
        .expect("reach");

    assert!(reached(&report).is_empty());
}

#[test]
fn a_crate_manifest_reaches_its_dependents() {
    let graph = fixture();
    let report = Reach::new(&graph)
        .of(
            &changes(&graph, &["auth/Cargo.toml"]),
            &packages(&graph, &ALL),
        )
        .expect("reach");

    assert_eq!(reached(&report), ["fx-api", "fx-auth"]);
}

#[test]
fn a_file_outside_every_crate_is_reported_and_reaches_nothing() {
    let graph = fixture();
    let report = Reach::new(&graph)
        .of(
            &changes(&graph, &["docs/readme.md"]),
            &packages(&graph, &ALL),
        )
        .expect("reach");

    assert!(reached(&report).is_empty());
    assert_eq!(report.outside, [RepoPath::parse("docs/readme.md").unwrap()]);
}

#[test]
fn an_unnarrowed_workspace_file_reaches_everything() {
    let graph = fixture();
    for path in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"] {
        let report = Reach::new(&graph)
            .of(&changes(&graph, &[path]), &packages(&graph, &ALL))
            .expect("reach");

        assert_eq!(reached(&report), ALL, "{path}");
    }
}

#[test]
fn a_root_manifest_change_reaches_the_crates_built_from_the_dependency() {
    let graph = fixture();
    let change = ManifestChange::Dependencies(BTreeSet::from(["fx-core".to_owned()]));
    let report = Reach::new(&graph)
        .with_manifest_change(&change)
        .of(&changes(&graph, &["Cargo.toml"]), &packages(&graph, &ALL))
        .expect("reach");

    assert_eq!(reached(&report), ["fx-api", "fx-auth", "fx-core"]);
    assert_eq!(
        report.packages[&graph.package("fx-api").unwrap()].reasons,
        [ReachReason::WorkspaceDependencyChanged {
            name: "fx-core".into()
        }]
    );
}

#[test]
fn a_new_workspace_member_reaches_nothing() {
    let graph = fixture();
    let change = ManifestChange::Dependencies(BTreeSet::new());
    let report = Reach::new(&graph)
        .with_manifest_change(&change)
        .of(&changes(&graph, &["Cargo.toml"]), &packages(&graph, &ALL))
        .expect("reach");

    assert!(reached(&report).is_empty());
}

const LOCK: &str = r#"
version = 4

[[package]]
name = "fx-api"
version = "0.0.0"
dependencies = [
 "fx-auth",
]

[[package]]
name = "fx-auth"
version = "0.0.0"
dependencies = [
 "fx-core",
 "serde",
]

[[package]]
name = "fx-core"
version = "0.0.0"

[[package]]
name = "fx-tools"
version = "0.0.0"
dependencies = [
 "rand",
]

[[package]]
name = "rand"
version = "0.9.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "r090"

[[package]]
name = "serde"
version = "1.0.200"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aaa"
"#;

fn with_locks(graph: &WorkspaceGraph, head: &str) -> ReachReport {
    let (base, head) = (Lockfile::parse(LOCK), Lockfile::parse(head));
    Reach::new(graph)
        .with_locks(LockPair {
            base: &base,
            head: &head,
        })
        .of(&changes(graph, &["Cargo.lock"]), &packages(graph, &ALL))
        .expect("reach")
}

#[test]
fn a_lock_change_reaches_the_crates_resolved_differently() {
    let graph = fixture();
    let report = with_locks(&graph, &LOCK.replace("1.0.200", "1.0.201"));

    assert_eq!(reached(&report), ["fx-api", "fx-auth"]);
    assert_eq!(
        report.packages[&graph.package("fx-auth").unwrap()].reasons,
        [
            ReachReason::LockChanged {
                name: "serde".into(),
                version: "1.0.200".into()
            },
            ReachReason::LockChanged {
                name: "serde".into(),
                version: "1.0.201".into()
            },
        ]
    );
}

#[test]
fn a_member_version_bump_in_the_lock_reaches_nothing() {
    let graph = fixture();
    let head = LOCK.replace(
        "name = \"fx-core\"\nversion = \"0.0.0\"",
        "name = \"fx-core\"\nversion = \"0.0.1\"",
    );
    assert_ne!(head, LOCK);

    assert!(reached(&with_locks(&graph, &head)).is_empty());
}

#[test]
fn a_dependency_pointed_at_another_locked_package_reaches_the_crate() {
    let graph = fixture();
    // fx-core starts to use a package that was already locked for fx-tools.
    let head = LOCK.replace(
        "name = \"fx-core\"\nversion = \"0.0.0\"\n",
        "name = \"fx-core\"\nversion = \"0.0.0\"\ndependencies = [\n \"rand\",\n]\n",
    );
    assert_ne!(head, LOCK);

    assert_eq!(
        reached(&with_locks(&graph, &head)),
        ["fx-api", "fx-auth", "fx-core"]
    );
}

#[test]
fn a_lock_format_change_reaches_everything() {
    let graph = fixture();
    let report = with_locks(&graph, &LOCK.replace("version = 4", "version = 5"));

    assert_eq!(reached(&report), ALL);
}
