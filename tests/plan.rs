//! Plans against the fixture workspace in `tests/fixtures/workspace`:
//!
//! ```text
//! fx-api --> fx-auth --> fx-core
//!              `--(dev)--> fx-tools
//! ```

use camino::Utf8PathBuf;
use pion::{ChangeSet, FallbackCause, Reason, RepoPath, TestPlan, WorkspaceGraph};

fn fixture() -> WorkspaceGraph {
    let root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/workspace")
        .canonicalize_utf8()
        .expect("fixture workspace exists");
    WorkspaceGraph::load(&root.join("Cargo.toml"), &root).expect("fixture workspace loads")
}

fn plan(graph: &WorkspaceGraph, paths: &[&str]) -> TestPlan {
    let paths = paths
        .iter()
        .map(|path| RepoPath::parse(path).expect("valid fixture path"));
    let changes = ChangeSet::classify(paths, graph);
    TestPlan::for_changes(&changes, graph).expect("plan")
}

fn selected(plan: &TestPlan) -> Vec<&str> {
    plan.crates.keys().map(|package| package.as_str()).collect()
}

fn path(raw: &str) -> RepoPath {
    RepoPath::parse(raw).expect("valid path")
}

#[test]
fn a_change_reaches_the_crate_and_its_dependents() {
    let graph = fixture();
    let plan = plan(&graph, &["core/src/lib.rs"]);

    assert_eq!(selected(&plan), ["fx-api", "fx-auth", "fx-core"]);
    assert_eq!(
        plan.crates[&graph.owner_of(&path("core/src/lib.rs")).unwrap().clone()].reasons,
        [Reason::FileChanged {
            path: path("core/src/lib.rs")
        }]
    );
    assert!(plan.uncovered.is_empty());
}

#[test]
fn a_leaf_change_selects_only_the_leaf() {
    let graph = fixture();
    let plan = plan(&graph, &["api/src/lib.rs"]);

    assert_eq!(selected(&plan), ["fx-api"]);
}

#[test]
fn a_dev_dependency_reaches_the_crate_that_tests_with_it_but_no_further() {
    let graph = fixture();
    let plan = plan(&graph, &["tools/src/lib.rs"]);

    // fx-auth's tests link fx-tools. fx-api links fx-auth's library, which
    // does not contain fx-tools.
    assert_eq!(selected(&plan), ["fx-auth", "fx-tools"]);
}

#[test]
fn a_non_rust_file_inside_a_crate_selects_that_crate() {
    let graph = fixture();
    let plan = plan(&graph, &["auth/queries/find_user.sql"]);

    assert_eq!(selected(&plan), ["fx-api", "fx-auth"]);
}

#[test]
fn a_workspace_wide_input_selects_every_crate() {
    let graph = fixture();
    let plan = plan(&graph, &["Cargo.lock"]);

    assert_eq!(
        selected(&plan),
        ["fx-api", "fx-auth", "fx-core", "fx-tools"]
    );
    for crate_plan in plan.crates.values() {
        assert_eq!(
            crate_plan.reasons,
            [Reason::FellBack {
                cause: FallbackCause::WorkspaceGlobal {
                    path: path("Cargo.lock")
                }
            }]
        );
    }
}

#[test]
fn a_file_no_crate_owns_is_reported_and_selects_nothing() {
    let graph = fixture();
    let plan = plan(&graph, &["docs/guide.md"]);

    assert!(plan.crates.is_empty());
    assert_eq!(plan.uncovered, [path("docs/guide.md")]);
    assert_eq!(plan.nextest_filterset(), None);
}

#[test]
fn the_nextest_filterset_names_each_selected_crate() {
    let graph = fixture();
    let plan = plan(&graph, &["auth/src/lib.rs"]);

    assert_eq!(
        plan.nextest_filterset().as_deref(),
        Some("package(=fx-api) + package(=fx-auth)")
    );
}
