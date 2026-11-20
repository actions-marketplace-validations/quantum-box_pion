use std::collections::{BTreeMap, BTreeSet};
use std::process::{Command, ExitCode, Stdio};
use std::time::Instant;

use camino::Utf8PathBuf;
use clap::{Parser, Subcommand, ValueEnum};
use pion::git::{self, Revision, Side};
use pion::lockfile::Lockfile;
use pion::scip::Index;
use pion::{
    ChangeSet, FallbackCause, Narrowing, Planner, Reason, ReferenceGraph, RepoPath, ScenarioReason,
    Selection, TestPlan, WorkspaceGraph, audit, scenario,
};

type Failure = Box<dyn std::error::Error>;

/// `cargo pion ...` arrives as `cargo-pion pion ...`.
#[derive(Parser)]
#[command(name = "cargo", bin_name = "cargo")]
enum Cargo {
    /// Select the tests a change can affect
    #[command(subcommand, version)]
    Pion(Action),
}

#[derive(Subcommand)]
enum Action {
    /// Print which tests to run for the changes since a revision
    Plan(PlanArgs),
    /// Run the selected tests with cargo-nextest
    Run(RunArgs),
    /// Build the index `plan --scip` reads, with rust-analyzer
    Index(IndexArgs),
    /// List the failed tests of a full run that a plan did not select
    Audit(AuditArgs),
    /// Print what would run if given lines changed, one JSON object per line
    Impact(ImpactArgs),
}

#[derive(clap::Args)]
struct Selecting {
    /// Revision to compare against (branch, tag or commit)
    #[arg(long)]
    base: Revision,
    /// Revision to plan for; the working tree when omitted
    #[arg(long)]
    head: Option<Revision>,
    /// Path to the workspace `Cargo.toml`
    #[arg(long, default_value = "Cargo.toml")]
    manifest_path: Utf8PathBuf,
    /// A SCIP index of the working tree (see `cargo pion index`). With it,
    /// a change inside a crate selects only the tests that reach it
    #[arg(long)]
    scip: Option<Utf8PathBuf>,
    /// Which revision the index was built from. `base` lets one index,
    /// built once for a commit, serve every change made on top of it
    #[arg(long, value_enum, default_value_t = Indexed::Head)]
    scip_revision: Indexed,
    /// A directory of API scenario files to select from; repeatable
    #[arg(long = "scenarios", value_name = "DIR")]
    scenario_dirs: Vec<Utf8PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Indexed {
    /// The revision given as `--base`
    Base,
    /// The working tree
    Head,
}

#[derive(clap::Args)]
struct PlanArgs {
    #[command(flatten)]
    selecting: Selecting,
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,
    /// Exit with status 2 when a changed file is covered by no rule
    #[arg(long)]
    deny_uncovered: bool,
}

#[derive(clap::Args)]
struct RunArgs {
    #[command(flatten)]
    selecting: Selecting,
    /// Arguments passed on to `cargo nextest run`
    #[arg(last = true)]
    nextest_args: Vec<String>,
}

#[derive(clap::Args)]
struct IndexArgs {
    /// Path to the workspace `Cargo.toml`
    #[arg(long, default_value = "Cargo.toml")]
    manifest_path: Utf8PathBuf,
    /// Where to write the index
    #[arg(long, default_value = "target/pion/index.scip")]
    output: Utf8PathBuf,
}

#[derive(clap::Args)]
struct AuditArgs {
    /// A plan written by `cargo pion plan --format json`
    #[arg(long)]
    plan: Utf8PathBuf,
    /// A JUnit report of a run of every test (cargo-nextest writes one)
    #[arg(long)]
    junit: Vec<Utf8PathBuf>,
    /// A log with the output of `cargo test`, such as a CI job log
    #[arg(long)]
    libtest: Vec<Utf8PathBuf>,
}

#[derive(clap::Args)]
struct ImpactArgs {
    /// Path to the workspace `Cargo.toml`
    #[arg(long, default_value = "Cargo.toml")]
    manifest_path: Utf8PathBuf,
    /// A SCIP index of the working tree (see `cargo pion index`)
    #[arg(long)]
    scip: Utf8PathBuf,
    /// A directory of API scenario files to select from; repeatable
    #[arg(long = "scenarios", value_name = "DIR")]
    scenario_dirs: Vec<Utf8PathBuf>,
    /// Lines to treat as changed, as `path:line` with the path relative to
    /// the repository root and the line one-based
    #[arg(required = true, value_name = "PATH:LINE")]
    lines: Vec<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    /// Crates, tests and scenarios, with the reasons they were selected
    Text,
    /// The whole plan as JSON
    Json,
    /// A filterset for `cargo nextest run -E`
    Nextest,
    /// The selected crates, one per line
    Packages,
    /// A GitHub Actions matrix: one entry per crate, with its filterset
    GithubMatrix,
    /// The selected scenario files, one per line
    Scenarios,
}

fn main() -> ExitCode {
    let Cargo::Pion(action) = Cargo::parse();
    let result = match action {
        Action::Plan(args) => plan(&args),
        Action::Run(args) => run(&args),
        Action::Index(args) => index(&args),
        Action::Audit(args) => audit_plan(&args),
        Action::Impact(args) => impact(&args),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn plan(args: &PlanArgs) -> Result<ExitCode, Failure> {
    let (changes, plan) = select(&args.selecting)?;
    match args.format {
        Format::Json => println!("{}", serde_json::to_string_pretty(&plan)?),
        Format::Nextest => match plan.nextest_filterset() {
            Some(filterset) => println!("{filterset}"),
            None => eprintln!("no test is affected"),
        },
        Format::Packages => {
            for package in plan.crates.keys() {
                println!("{package}");
            }
        }
        Format::GithubMatrix => {
            let include: Vec<serde_json::Value> = plan
                .crates
                .iter()
                .map(|(package, crate_plan)| {
                    serde_json::json!({
                        "package": package,
                        "filterset": TestPlan::crate_filterset(package, crate_plan),
                    })
                })
                .collect();
            println!("{}", serde_json::json!({ "include": include }));
        }
        Format::Scenarios => {
            for scenario in plan.scenarios.keys() {
                println!("{scenario}");
            }
        }
        Format::Text => print_text(&changes, &plan),
    }
    if args.deny_uncovered && !plan.uncovered.is_empty() {
        return Ok(ExitCode::from(2));
    }
    Ok(ExitCode::SUCCESS)
}

fn run(args: &RunArgs) -> Result<ExitCode, Failure> {
    let (_, plan) = select(&args.selecting)?;
    let Some(filterset) = plan.nextest_filterset() else {
        eprintln!("no test is affected");
        return Ok(ExitCode::SUCCESS);
    };
    let status = Command::new("cargo")
        .args(["nextest", "run", "--manifest-path"])
        .arg(&args.selecting.manifest_path)
        .args(["-E", &filterset])
        .args(&args.nextest_args)
        .status()?;
    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn index(args: &IndexArgs) -> Result<ExitCode, Failure> {
    let manifest = args.manifest_path.canonicalize_utf8()?;
    let workspace = manifest.parent().ok_or("the manifest path has no parent")?;
    if let Some(directory) = args.output.parent() {
        std::fs::create_dir_all(directory)?;
    }
    let log_path = Utf8PathBuf::from(format!("{}.log", args.output));
    let started = Instant::now();
    // rust-analyzer reports thousands of diagnostics on a large workspace.
    // They go to a log file; a summary by kind is printed instead.
    let output = Command::new("rust-analyzer")
        .arg("scip")
        .arg(workspace)
        .arg("--output")
        .arg(&args.output)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    let log = String::from_utf8_lossy(&output.stderr);
    std::fs::write(&log_path, log.as_bytes())?;
    if !output.status.success() {
        return Err(format!("`rust-analyzer scip` failed; see {log_path}").into());
    }

    let size = std::fs::metadata(&args.output)?.len();
    println!(
        "{} ({} MB, {} s)",
        args.output,
        size / 1_000_000,
        started.elapsed().as_secs()
    );
    let diagnostics = summarize_diagnostics(&log);
    if !diagnostics.is_empty() {
        println!("rust-analyzer diagnostics (full log: {log_path}):");
    }
    for (kind, count) in diagnostics {
        println!("  {count:>6}  {kind}{}", diagnostic_note(&kind));
    }
    Ok(ExitCode::SUCCESS)
}

/// Counts rust-analyzer's `ERROR` and `WARN` lines by message, most frequent
/// first. The part of a message that names a place or an id is dropped so
/// that repeats of one problem count together.
fn summarize_diagnostics(log: &str) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for line in log.lines() {
        let message = ["ERROR ", "WARN "]
            .iter()
            .find_map(|level| line.split_once(level).map(|(_, message)| message));
        let Some(message) = message else { continue };
        let kind = [" def=", " at ", ": ", "("]
            .iter()
            .filter_map(|cut| message.find(cut))
            .min()
            .map_or(message, |end| &message[..end]);
        *counts.entry(kind.trim().to_owned()).or_default() += 1;
    }
    let mut kinds: Vec<(String, usize)> = counts.into_iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    kinds
}

/// What a known diagnostic means for selection.
fn diagnostic_note(kind: &str) -> &'static str {
    if kind.starts_with("Encountered enclosing definition with no name") {
        "\n          harmless: an item inside a function body has no named parent; \
         pion does not read that field"
    } else if kind.starts_with("Bug: definition") {
        "\n          a definition generated by a macro has no place in a source file; \
         references to it cannot be followed"
    } else {
        ""
    }
}

fn audit_plan(args: &AuditArgs) -> Result<ExitCode, Failure> {
    let plan: TestPlan = serde_json::from_str(&std::fs::read_to_string(&args.plan)?)?;
    if args.junit.is_empty() && args.libtest.is_empty() {
        return Err("give the results of a full run with --junit or --libtest".into());
    }
    let mut outcomes = Vec::new();
    for path in &args.junit {
        outcomes.extend(audit::read_junit(&std::fs::read_to_string(path)?));
    }
    for path in &args.libtest {
        // CI logs can hold bytes that are not UTF-8.
        let log = String::from_utf8_lossy(&std::fs::read(path)?).into_owned();
        outcomes.extend(audit::read_libtest(&log));
    }
    let failed = outcomes.iter().filter(|outcome| outcome.failed).count();
    let misses = audit::missed(&plan, &outcomes);
    println!(
        "{} test(s) ran, {failed} failed, {} failure(s) the plan did not select",
        outcomes.len(),
        misses.len()
    );
    for miss in &misses {
        println!("  {} {}", miss.package, miss.test);
    }
    Ok(if misses.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Plans for each given line on its own, as if only that line had changed.
/// Each result also carries the number of crates the crate-level plan would
/// run, to show what the reference graph saves.
fn impact(args: &ImpactArgs) -> Result<ExitCode, Failure> {
    let manifest = args.manifest_path.canonicalize_utf8()?;
    let manifest_dir = manifest.parent().ok_or("the manifest path has no parent")?;
    let repository = git::repository_root(manifest_dir)?.canonicalize_utf8()?;
    let graph = WorkspaceGraph::load(&manifest, &repository)?;
    let index = Index::decode(&std::fs::read(&args.scip)?)?;
    let symbols = ReferenceGraph::build(&index, &graph);
    let (scenarios, scenario_roots) = scenario::discover(
        &args.scenario_dirs,
        &repository,
        &graph,
        Some(&symbols),
        |path| std::fs::read_to_string(repository.join(path.as_path())).ok(),
    )?;

    for target in &args.lines {
        let (file, line) = target
            .rsplit_once(':')
            .ok_or_else(|| format!("`{target}` is not `path:line`"))?;
        let line: u32 = line.parse()?;
        let path = RepoPath::parse(file)?;
        let changes = ChangeSet::classify([path.clone()], &graph);
        let lines = BTreeMap::from([(
            path.clone(),
            vec![git::ChangedLine {
                line: line.saturating_sub(1),
                gap_to: None,
                text: None,
                addition_only: false,
            }],
        )]);
        let narrowed = Planner::new(&graph)
            .with_narrowing(Narrowing {
                symbols: &symbols,
                lines: &lines,
            })
            .with_scenarios(&scenarios, &scenario_roots)
            .plan(&changes)?;
        let crate_level = TestPlan::for_changes(&changes, &graph)?;

        let whole = narrowed
            .crates
            .values()
            .filter(|plan| plan.selection == Selection::All)
            .count();
        let tests: usize = narrowed
            .crates
            .values()
            .map(|plan| match &plan.selection {
                Selection::All => 0,
                Selection::Tests(tests) => tests.len(),
            })
            .sum();
        let definition = symbols
            .definition_at_line(&path, line.saturating_sub(1))
            .map(ToString::to_string);
        let fallbacks: BTreeSet<String> = narrowed
            .crates
            .values()
            .flat_map(|plan| &plan.reasons)
            .filter_map(|reason| match reason {
                Reason::FellBack { cause } => Some(describe(&Reason::FellBack {
                    cause: cause.clone(),
                })),
                _ => None,
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "path": path,
                "line": line,
                "definition": definition,
                "crate_level_crates": crate_level.crates.len(),
                "whole_crates": whole,
                "narrowed_crates": narrowed.crates.len() - whole,
                "tests": tests,
                "scenarios": narrowed.scenarios.len(),
                "scenarios_total": scenarios.len(),
                "fallbacks": fallbacks,
            })
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Reads the workspace and the change, and plans with every input given.
fn select(args: &Selecting) -> Result<(ChangeSet, TestPlan), Failure> {
    let manifest = args.manifest_path.canonicalize_utf8()?;
    let manifest_dir = manifest.parent().ok_or("the manifest path has no parent")?;
    let repository = git::repository_root(manifest_dir)?.canonicalize_utf8()?;
    let head = args.head.as_ref();

    let graph = WorkspaceGraph::load(&manifest, &repository)?;
    let paths = git::changed_paths(&repository, &args.base, head)?;

    let lock_change = match graph.lockfile().filter(|lock| paths.contains(lock)) {
        Some(lock) => {
            let before = git::file_at(&repository, &args.base, &lock)?;
            let after = match head {
                Some(head) => git::file_at(&repository, head, &lock)?,
                None => std::fs::read_to_string(repository.join(lock.as_path())).ok(),
            };
            before.zip(after).map(|(before, after)| {
                Lockfile::parse(&after).changes_since(&Lockfile::parse(&before))
            })
        }
        None => None,
    };

    let symbols = match &args.scip {
        Some(path) => {
            let index = Index::decode(&std::fs::read(path)?)?;
            Some(ReferenceGraph::build(&index, &graph))
        }
        None => None,
    };
    let lines = match &symbols {
        Some(_) => {
            let side = match args.scip_revision {
                Indexed::Base => Side::Base,
                Indexed::Head => Side::Head,
            };
            git::changed_lines(&repository, &args.base, head, side)?
        }
        None => BTreeMap::new(),
    };

    // Routes are read from the same revision the index describes.
    let read_source = |path: &RepoPath| match args.scip_revision {
        Indexed::Base => git::file_at(&repository, &args.base, path).ok().flatten(),
        Indexed::Head => std::fs::read_to_string(repository.join(path.as_path())).ok(),
    };
    let (scenarios, scenario_roots) = scenario::discover(
        &args.scenario_dirs,
        &repository,
        &graph,
        symbols.as_ref(),
        read_source,
    )?;

    let changes = ChangeSet::classify(paths, &graph);
    let mut planner = Planner::new(&graph).with_scenarios(&scenarios, &scenario_roots);
    if let Some(change) = &lock_change {
        planner = planner.with_lock_change(change);
    }
    if let Some(symbols) = &symbols {
        planner = planner.with_narrowing(Narrowing {
            symbols,
            lines: &lines,
        });
    }
    let plan = planner.plan(&changes)?;
    Ok((changes, plan))
}

fn print_text(changes: &ChangeSet, plan: &TestPlan) {
    println!(
        "{} changed file(s), {} crate(s) and {} scenario(s) to test",
        changes.files().len(),
        plan.crates.len(),
        plan.scenarios.len()
    );
    for (package, crate_plan) in &plan.crates {
        match &crate_plan.selection {
            Selection::All => println!("\n{package} (all tests)"),
            Selection::Tests(tests) => {
                println!("\n{package} ({} test(s))", tests.len());
                for test in tests {
                    println!("  test: {test}");
                }
            }
        }
        for reason in &crate_plan.reasons {
            println!("  {}", describe(reason));
        }
    }
    for (scenario, reasons) in &plan.scenarios {
        println!("\n{scenario}");
        for reason in reasons {
            match reason {
                ScenarioReason::FileChanged => println!("  the scenario changed"),
                ScenarioReason::Reaches { changed, handler } => {
                    println!("  {handler} reaches {changed}")
                }
                ScenarioReason::CrateNotNarrowed { package } => {
                    println!("  {package} runs all tests, so handlers were not traced")
                }
                ScenarioReason::Untraced { package } => {
                    println!(
                        "  a request could not be tied to a handler, and {package} is affected"
                    )
                }
            }
        }
    }
    if !plan.uncovered.is_empty() {
        println!("\nnot covered by any rule:");
        for path in &plan.uncovered {
            println!("  {path}");
        }
    }
}

fn describe(reason: &Reason) -> String {
    match reason {
        Reason::FileChanged { path } => format!("changed: {path}"),
        Reason::DependsOnCrate { package } => format!("depends on: {package}"),
        Reason::DependsOnExternal { name, version } => {
            format!("depends on: {name} {version} (Cargo.lock)")
        }
        Reason::Reaches { changed } => format!("reaches: {changed}"),
        Reason::FellBack { cause } => match cause {
            FallbackCause::WorkspaceGlobal { path } => {
                format!("workspace-wide input changed: {path}")
            }
            FallbackCause::NotInIndex { path } => format!("not in the index: {path}"),
            FallbackCause::OutsideDefinitions { path, line } => {
                format!("changed outside every definition: {path}:{line}")
            }
            FallbackCause::UnnamedTest { test } => format!("a reached test has no name: {test}"),
        },
    }
}
