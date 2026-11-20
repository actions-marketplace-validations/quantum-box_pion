use std::process::ExitCode;

use camino::Utf8PathBuf;
use clap::{Parser, Subcommand, ValueEnum};
use pion::git::{self, Revision};
use pion::{ChangeSet, Reason, TestPlan, WorkspaceGraph};

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
    /// Print which crates' tests to run for the changes since a revision
    Plan(PlanArgs),
}

#[derive(clap::Args)]
struct PlanArgs {
    /// Revision to compare against (branch, tag or commit)
    #[arg(long)]
    base: Revision,
    /// Revision to plan for; the working tree when omitted
    #[arg(long)]
    head: Option<Revision>,
    /// Path to the workspace `Cargo.toml`
    #[arg(long, default_value = "Cargo.toml")]
    manifest_path: Utf8PathBuf,
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,
    /// Exit with status 2 when a changed file is covered by no rule
    #[arg(long)]
    deny_uncovered: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    /// Crates and the reasons they were selected
    Text,
    /// The whole plan as JSON
    Json,
    /// A filterset for `cargo nextest run -E`
    Nextest,
}

fn main() -> ExitCode {
    let Cargo::Pion(action) = Cargo::parse();
    let result = match action {
        Action::Plan(args) => plan(&args),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn plan(args: &PlanArgs) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let manifest = args.manifest_path.canonicalize_utf8()?;
    let manifest_dir = manifest
        .parent()
        .ok_or("the manifest path has no parent directory")?;
    let repository = git::repository_root(manifest_dir)?.canonicalize_utf8()?;

    let graph = WorkspaceGraph::load(&manifest, &repository)?;
    let paths = git::changed_paths(&repository, &args.base, args.head.as_ref())?;
    let changes = ChangeSet::classify(paths, &graph);
    let plan = TestPlan::for_changes(&changes, &graph)?;

    match args.format {
        Format::Json => println!("{}", serde_json::to_string_pretty(&plan)?),
        Format::Nextest => match plan.nextest_filterset() {
            Some(filterset) => println!("{filterset}"),
            None => eprintln!("no test is affected"),
        },
        Format::Text => print_text(&changes, &plan),
    }

    if args.deny_uncovered && !plan.uncovered.is_empty() {
        return Ok(ExitCode::from(2));
    }
    Ok(ExitCode::SUCCESS)
}

fn print_text(changes: &ChangeSet, plan: &TestPlan) {
    println!(
        "{} changed file(s), {} crate(s) to test",
        changes.files().len(),
        plan.crates.len()
    );
    for (package, crate_plan) in &plan.crates {
        println!("\n{package}");
        for reason in &crate_plan.reasons {
            match reason {
                Reason::FileChanged { path } => println!("  changed: {path}"),
                Reason::DependsOnCrate { package } => println!("  depends on: {package}"),
                Reason::FellBack { cause } => match cause {
                    pion::FallbackCause::WorkspaceGlobal { path } => {
                        println!("  workspace-wide input changed: {path}")
                    }
                },
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
