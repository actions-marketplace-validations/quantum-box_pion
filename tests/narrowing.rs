//! Narrowing inside crates and scenario selection, against a real
//! rust-analyzer index of `tests/fixtures/workspace`.
//!
//! ```text
//! scenarios/sign_in.yaml -> sign_in_handler -> SignIn::execute -> Token::is_expired
//!                                                    `-> dyn UserRepository::find
//! scenarios/sign_up.yaml -> sign_up_handler -> sign_up -> dyn UserRepository::insert
//!                        `-> health_handler
//! ```

use std::collections::BTreeMap;
use std::process::Command;
use std::sync::OnceLock;

use camino::{Utf8Path, Utf8PathBuf};
use pion::git::ChangedLine;
use pion::lockfile::{LockChange, LockedPackage};
use pion::scenario::{self, Scenario};
use pion::scip::Index;
use pion::{
    ChangeSet, FallbackCause, Narrowing, Planner, Reason, ReferenceGraph, RepoPath, ScenarioReason,
    Selection, TestPlan, WorkspaceGraph,
};

fn root() -> Utf8PathBuf {
    Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/workspace")
        .canonicalize_utf8()
        .expect("fixture workspace exists")
}

/// The fixture's index, built once per test run.
fn index() -> &'static Index {
    static INDEX: OnceLock<Index> = OnceLock::new();
    INDEX.get_or_init(|| {
        let output = Utf8Path::new(env!("CARGO_TARGET_TMPDIR")).join("fixture.scip");
        let status = Command::new("rust-analyzer")
            .arg("scip")
            .arg(root())
            .arg("--output")
            .arg(&output)
            .status()
            .expect("rust-analyzer is installed (rustup component add rust-analyzer)");
        assert!(status.success(), "rust-analyzer scip failed");
        Index::decode(&std::fs::read(&output).expect("index was written")).expect("valid index")
    })
}

struct Fixture {
    graph: WorkspaceGraph,
    symbols: ReferenceGraph,
    scenarios: Vec<Scenario>,
    scenario_roots: Vec<RepoPath>,
}

impl Fixture {
    fn load() -> Self {
        let root = root();
        let graph = WorkspaceGraph::load(&root.join("Cargo.toml"), &root).expect("workspace loads");
        let symbols = ReferenceGraph::build(index(), &graph);
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

    /// Plans for a change to the lines of `file` that contain `needle`.
    fn plan_edit(&self, file: &str, needle: &str) -> TestPlan {
        let line = line_of(file, needle);
        self.plan_lines(
            file,
            vec![ChangedLine {
                line,
                gap_to: None,
                text: Some(needle.to_owned()),
                addition_only: false,
            }],
        )
    }

    fn plan_lines(&self, file: &str, lines: Vec<ChangedLine>) -> TestPlan {
        let path = path(file);
        let changes = ChangeSet::classify([path.clone()], &self.graph);
        let lines = BTreeMap::from([(path, lines)]);
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

fn path(raw: &str) -> RepoPath {
    RepoPath::parse(raw).expect("valid path")
}

/// The zero-based number of the only line of `file` that contains `needle`.
fn line_of(file: &str, needle: &str) -> u32 {
    let text = std::fs::read_to_string(root().join(file)).expect("fixture file");
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
    u32::try_from(matches[0]).expect("line number fits")
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
    let fixture = Fixture::load();
    let plan = fixture.plan_edit("core/src/token.rs", "self.age > LIMIT");

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
    let fixture = Fixture::load();
    // `SignIn::execute` calls `find` on `&dyn UserRepository`, never on
    // `MemoryRepository` by name.
    let plan = fixture.plan_edit("auth/src/repo.rs", "Some(id)");

    assert_eq!(selected(&plan), ["fx-auth: tests::rejects_expired_token"]);
    assert_eq!(scenarios(&plan), ["sign_in.yaml"]);
}

#[test]
fn a_default_trait_method_reaches_unit_and_integration_tests() {
    let fixture = Fixture::load();
    let plan = fixture.plan_edit("auth/src/repo.rs", "id > 0");

    assert_eq!(
        selected(&plan),
        ["fx-auth: sign_up_flow", "fx-auth: tests::creates_user"]
    );
    assert_eq!(scenarios(&plan), ["sign_up.yaml"]);
}

#[test]
fn a_struct_field_reaches_everything_that_uses_the_struct() {
    let fixture = Fixture::load();
    let plan = fixture.plan_edit("core/src/token.rs", "pub age: u32");

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
    let fixture = Fixture::load();
    let plan = fixture.plan_edit("api/src/lib.rs", "true");

    // `health_handler` has no unit test; `sign_up.yaml` requests
    // `/v1/health/{{vars.probe}}`.
    assert!(plan.crates.is_empty());
    assert_eq!(scenarios(&plan), ["sign_up.yaml"]);
    let reasons = &plan.scenarios[&path("api/tests/scenarios/sign_up.yaml")];
    assert!(matches!(reasons[..], [ScenarioReason::Reaches { .. }]));
}

#[test]
fn a_line_outside_every_definition_falls_back_to_the_crate() {
    let fixture = Fixture::load();
    let plan = fixture.plan_edit("core/src/token.rs", "use crate::LIMIT;");

    assert_eq!(selected(&plan), ["fx-api: *", "fx-auth: *", "fx-core: *"]);
    let core = fixture.graph.owner_of(&path("core/src/token.rs")).unwrap();
    assert_eq!(
        plan.crates[core].reasons,
        [Reason::FellBack {
            cause: FallbackCause::OutsideDefinitions {
                path: path("core/src/token.rs"),
                line: 1
            }
        }]
    );
    // fx-api runs everything, so its scenarios cannot be narrowed either.
    assert_eq!(scenarios(&plan), ["sign_in.yaml", "sign_up.yaml"]);
}

#[test]
fn an_added_blank_line_between_items_selects_nothing() {
    let fixture = Fixture::load();
    let between_items = line_of("core/src/token.rs", "impl Token {") - 1;
    let plan = fixture.plan_lines(
        "core/src/token.rs",
        vec![ChangedLine {
            line: between_items,
            gap_to: None,
            text: Some(String::new()),
            addition_only: true,
        }],
    );

    assert!(plan.crates.is_empty());
    assert!(plan.scenarios.is_empty());
}

#[test]
fn a_file_missing_from_the_index_falls_back_to_the_crate() {
    let fixture = Fixture::load();
    let plan = fixture.plan_lines("tools/src/brand_new.rs", vec![]);

    assert_eq!(selected(&plan), ["fx-auth: *", "fx-tools: *"]);
}

#[test]
fn a_changed_scenario_selects_itself_and_no_crate() {
    let fixture = Fixture::load();
    let changes = ChangeSet::classify([path("api/tests/scenarios/sign_in.yaml")], &fixture.graph);
    let plan = Planner::new(&fixture.graph)
        .with_scenarios(&fixture.scenarios, &fixture.scenario_roots)
        .plan(&changes)
        .expect("plan");

    assert!(plan.crates.is_empty());
    assert_eq!(scenarios(&plan), ["sign_in.yaml"]);
    assert_eq!(
        plan.scenarios[&path("api/tests/scenarios/sign_in.yaml")],
        [ScenarioReason::FileChanged]
    );
}

#[test]
fn the_filterset_names_tests_inside_narrowed_crates() {
    let fixture = Fixture::load();
    let plan = fixture.plan_edit("auth/src/repo.rs", "Some(id)");

    assert_eq!(
        plan.nextest_filterset().as_deref(),
        Some("(package(=fx-auth) & (test(=tests::rejects_expired_token)))")
    );
    assert!(plan.selects("fx-auth", "tests::rejects_expired_token"));
    assert!(!plan.selects("fx-auth", "tests::creates_user"));
}

#[test]
fn a_lockfile_change_with_no_package_difference_selects_nothing() {
    let fixture = Fixture::load();
    let changes = ChangeSet::classify([path("Cargo.lock")], &fixture.graph);
    let unchanged = LockChange::Packages(vec![]);
    let plan = Planner::new(&fixture.graph)
        .with_lock_change(&unchanged)
        .plan(&changes)
        .expect("plan");

    assert!(plan.crates.is_empty());
}

#[test]
fn a_lockfile_package_the_graph_does_not_know_selects_every_crate() {
    let fixture = Fixture::load();
    let changes = ChangeSet::classify([path("Cargo.lock")], &fixture.graph);
    let unknown = LockChange::Packages(vec![LockedPackage {
        name: "not-a-dependency".into(),
        version: "1.0.0".into(),
        source: Some("registry+https://github.com/rust-lang/crates.io-index".into()),
        checksum: None,
    }]);
    let plan = Planner::new(&fixture.graph)
        .with_lock_change(&unknown)
        .plan(&changes)
        .expect("plan");

    assert_eq!(
        selected(&plan),
        ["fx-api: *", "fx-auth: *", "fx-core: *", "fx-tools: *"]
    );
}

/// A change that has no line of its own in the indexed revision: it sits
/// between two lines.
fn gap(after_line: u32, added: Option<&str>) -> ChangedLine {
    ChangedLine {
        line: after_line,
        gap_to: Some(after_line + 1),
        text: added.map(str::to_owned),
        addition_only: added.is_some(),
    }
}

#[test]
fn lines_inserted_inside_a_function_change_that_function() {
    let fixture = Fixture::load();
    // Planned against an index of the base revision: the new lines go
    // between two lines of `is_expired`.
    let signature = line_of("core/src/token.rs", "pub fn is_expired");
    let plan = fixture.plan_lines(
        "core/src/token.rs",
        vec![gap(signature, Some("        let limit = LIMIT;"))],
    );

    assert_eq!(
        selected(&plan),
        [
            "fx-auth: tests::rejects_expired_token",
            "fx-core: token::tests::token_roundtrip",
        ]
    );
}

#[test]
fn an_item_inserted_between_items_falls_back_to_the_crate() {
    let fixture = Fixture::load();
    // After the closing brace of `struct Token`, before `impl Token`.
    let between_items = line_of("core/src/token.rs", "impl Token {") - 2;
    let plan = fixture.plan_lines(
        "core/src/token.rs",
        vec![gap(between_items, Some("pub fn brand_new() {}"))],
    );

    assert_eq!(selected(&plan), ["fx-api: *", "fx-auth: *", "fx-core: *"]);
}

#[test]
fn a_comment_inserted_between_items_selects_nothing() {
    let fixture = Fixture::load();
    let between_items = line_of("core/src/token.rs", "impl Token {") - 2;
    let plan = fixture.plan_lines(
        "core/src/token.rs",
        vec![gap(between_items, Some("\n// Tokens expire.\n"))],
    );

    assert!(plan.crates.is_empty());
}

#[test]
fn one_plan_gives_every_later_job_its_inputs() {
    let fixture = Fixture::load();
    let plan = fixture.plan_edit("core/src/token.rs", "self.age > LIMIT");
    let outputs: BTreeMap<&str, String> = plan.github_outputs().into_iter().collect();

    assert_eq!(outputs["any"], "true");
    assert_eq!(outputs["packages"], r#"["fx-auth","fx-core"]"#);
    assert_eq!(
        outputs["filtersets"],
        concat!(
            r#"{"fx-auth":"(package(=fx-auth) & (test(=tests::rejects_expired_token)))","#,
            r#""fx-core":"(package(=fx-core) & (test(=token::tests::token_roundtrip)))"}"#
        )
    );
    assert_eq!(
        outputs["scenarios"],
        r#"["api/tests/scenarios/sign_in.yaml"]"#
    );
    assert_eq!(outputs["uncovered"], "[]");
    // Step outputs are one line each.
    assert!(outputs.values().all(|value| !value.contains('\n')));
}

#[test]
fn a_plan_that_selects_nothing_says_so() {
    let fixture = Fixture::load();
    let changes = ChangeSet::classify([path("docs/guide.md")], &fixture.graph);
    let plan = Planner::new(&fixture.graph).plan(&changes).expect("plan");
    let outputs: BTreeMap<&str, String> = plan.github_outputs().into_iter().collect();

    assert_eq!(outputs["any"], "false");
    assert_eq!(outputs["packages"], "[]");
    assert_eq!(outputs["filterset"], "");
    assert_eq!(outputs["matrix"], r#"{"include":[]}"#);
    assert_eq!(outputs["uncovered"], r#"["docs/guide.md"]"#);
}
