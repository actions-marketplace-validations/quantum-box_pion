use std::collections::BTreeMap;
use std::process::{Command, ExitCode};

use camino::Utf8PathBuf;
use clap::{Parser, Subcommand, ValueEnum};
use pion::git::{self, Revision};
use pion::lockfile::Lockfile;
use pion::scip::Index;
use pion::{
    ChangeSet, FallbackCause, Narrowing, Planner, Reason, ReferenceGraph, ScenarioReason,
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
    /// A directory of API scenario files to select from; repeatable
    #[arg(long = "scenarios", value_name = "DIR")]
    scenario_dirs: Vec<Utf8PathBuf>,
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
    /// The JUnit report of a run of every test
    #[arg(long)]
    junit: Utf8PathBuf,
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
    let status = Command::new("rust-analyzer")
        .arg("scip")
        .arg(workspace)
        .arg("--output")
        .arg(&args.output)
        .status()?;
    if !status.success() {
        return Err("`rust-analyzer scip` failed".into());
    }
    println!("{}", args.output);
    Ok(ExitCode::SUCCESS)
}

fn audit_plan(args: &AuditArgs) -> Result<ExitCode, Failure> {
    let plan: TestPlan = serde_json::from_str(&std::fs::read_to_string(&args.plan)?)?;
    let outcomes = audit::read_junit(&std::fs::read_to_string(&args.junit)?);
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
        Some(_) => git::changed_lines(&repository, &args.base, head)?,
        None => BTreeMap::new(),
    };

    let (scenarios, scenario_roots) =
        scenario::discover(&args.scenario_dirs, &repository, &graph, symbols.as_ref())?;

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
