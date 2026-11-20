//! Narrowing from the records `pion-rustc` writes while the fixture
//! workspace compiles.
//!
//! The records in `tests/fixtures/records` are checked in, because writing
//! them needs the nightly toolchain the wrapper is built for. Regenerate
//! them after changing the fixture workspace or the wrapper:
//!
//! ```bash
//! cargo +<nightly> build --manifest-path driver/Cargo.toml
//! rm -rf tests/fixtures/records
//! cd tests/fixtures/workspace
//! RUSTC_WORKSPACE_WRAPPER=$PWD/../../../driver/target/debug/pion-rustc \
//!   PION_REFS_DIR=$PWD/../records \
//!   cargo +<nightly> check --all-targets --target-dir ../../../target/tmp/refs-target
//! ```

use std::collections::BTreeMap;

use camino::Utf8PathBuf;
use pion::git::ChangedLine;
use pion::scenario::{self, Scenario};
use pion::{
    ChangeSet, Narrowing, Planner, ReferenceGraph, RepoPath, Selection, TestPlan, WorkspaceGraph,
    records,
};

fn fixtures() -> Utf8PathBuf {
    Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .canonicalize_utf8()
        .expect("fixtures exist")
}

struct Fixture {
    graph: WorkspaceGraph,
    symbols: ReferenceGraph,
    scenarios: Vec<Scenario>,
    scenario_roots: Vec<RepoPath>,
}

impl Fixture {
    fn load() -> Self {
        let root = fixtures().join("workspace");
        let graph = WorkspaceGraph::load(&root.join("Cargo.toml"), &root).expect("workspace loads");
        let records = records::read_directory(&fixtures().join("records")).expect("records load");
        let symbols = ReferenceGraph::from_records(&records, &graph);
        let (scenarios, scenario_roots) = scenario::discover(
            &[root.join("api/tests/scenarios")],
            &root,
            &graph,
            Some(&symbols),
            |path| std::fs::read_to_string(root.join(path.as_path())).ok(),
        )
        .expect("scenarios load");
        Self {
            graph,
            symbols,
            scenarios,
            scenario_roots,
        }
    }

    /// Plans for a change to the only line of `file` that contains `needle`.
    fn plan_edit(&self, file: &str, needle: &str) -> TestPlan {
        let text =
            std::fs::read_to_string(fixtures().join("workspace").join(file)).expect("fixture file");
        let matches: Vec<usize> = text
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains(needle))
            .map(|(index, _)| index)
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "`{needle}` should be on one line of {file}"
        );
        let path = RepoPath::parse(file).expect("valid path");
        let changes = ChangeSet::classify([path.clone()], &self.graph);
        let lines = BTreeMap::from([(
            path,
            vec![ChangedLine {
                line: u32::try_from(matches[0]).expect("line number fits"),
                gap_to: None,
                text: Some(needle.to_owned()),
                addition_only: false,
            }],
        )]);
        Planner::new(&self.graph)
            .with_narrowing(Narrowing {
                symbols: &self.symbols,
                lines: &lines,
            })
            .with_scenarios(&self.scenarios, &self.scenario_roots)
            .plan(&changes)
            .expect("plan")
    }
}

/// The selected tests as `crate: name` lines; `crate: *` for a whole crate.
fn selected(plan: &TestPlan) -> Vec<String> {
    let mut lines = Vec::new();
    for (package, crate_plan) in &plan.crates {
        match &crate_plan.selection {
            Selection::All => lines.push(format!("{package}: *")),
            Selection::Tests(tests) => {
                lines.extend(tests.iter().map(|test| format!("{package}: {test}")))
            }
        }
    }
    lines
}

fn scenarios(plan: &TestPlan) -> Vec<&str> {
    plan.scenarios
        .keys()
        .map(|path| path.as_path().file_name().expect("file name"))
        .collect()
}

#[test]
fn a_method_body_selects_the_tests_and_scenarios_that_call_it() {
    let plan = Fixture::load().plan_edit("core/src/token.rs", "self.age > LIMIT");

    assert_eq!(
        selected(&plan),
        [
            "fx-auth: tests::rejects_expired_token",
            "fx-core: token::tests::token_roundtrip",
        ]
    );
    assert_eq!(scenarios(&plan), ["sign_in.yaml"]);
}

#[test]
fn a_trait_impl_reaches_callers_of_the_trait_method() {
    let plan = Fixture::load().plan_edit("auth/src/repo.rs", "Some(id)");

    assert_eq!(selected(&plan), ["fx-auth: tests::rejects_expired_token"]);
    assert_eq!(scenarios(&plan), ["sign_in.yaml"]);
}

#[test]
fn a_default_trait_method_reaches_unit_and_integration_tests() {
    let plan = Fixture::load().plan_edit("auth/src/repo.rs", "id > 0");

    assert_eq!(
        selected(&plan),
        ["fx-auth: sign_up_flow", "fx-auth: tests::creates_user"]
    );
    assert_eq!(scenarios(&plan), ["sign_up.yaml"]);
}

#[test]
fn a_struct_field_reaches_everything_that_uses_the_struct() {
    let plan = Fixture::load().plan_edit("core/src/token.rs", "pub age: u32");

    assert_eq!(
        selected(&plan),
        [
            "fx-auth: tests::rejects_expired_token",
            "fx-core: token::tests::token_roundtrip",
        ]
    );
}

#[test]
fn a_handler_selects_only_the_scenarios_that_request_it() {
    let plan = Fixture::load().plan_edit("api/src/lib.rs", "true");

    assert!(plan.crates.is_empty());
    assert_eq!(scenarios(&plan), ["sign_up.yaml"]);
}

#[test]
fn a_call_made_by_derived_code_is_followed() {
    // `Config` derives `Default`. The derived impl, which no source file
    // contains, calls `Limit::default`; the test only calls
    // `Config::default`.
    let plan = Fixture::load().plan_edit("core/src/config.rs", "Limit(3)");

    assert_eq!(
        selected(&plan),
        ["fx-core: config::tests::default_config_has_a_limit"]
    );
}

#[test]
fn a_line_outside_every_definition_falls_back_to_the_crate() {
    let plan = Fixture::load().plan_edit("core/src/token.rs", "use crate::LIMIT;");

    assert_eq!(selected(&plan), ["fx-api: *", "fx-auth: *", "fx-core: *"]);
}
