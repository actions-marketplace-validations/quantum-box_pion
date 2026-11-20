//! A rustc wrapper that records which definition refers to which.
//!
//! Set as `RUSTC_WORKSPACE_WRAPPER`, it compiles each workspace crate exactly
//! as rustc would and, once the crate has type-checked, writes the crate's
//! definitions and the references between them to `$PION_REFS_DIR`. cargo
//! only recompiles what changed, so the records stay current at the cost of
//! the compile that happens anyway.

#![feature(rustc_private)]

extern crate rustc_driver;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_span;

mod record;

use std::process::ExitCode;

use rustc_driver::{Callbacks, Compilation};
use rustc_interface::interface::Compiler;
use rustc_middle::ty::TyCtxt;

struct Recorder;

impl Callbacks for Recorder {
    fn after_analysis<'tcx>(&mut self, _compiler: &Compiler, tcx: TyCtxt<'tcx>) -> Compilation {
        if let Some(directory) = std::env::var_os("PION_REFS_DIR") {
            // Recording must never break the build it rides on: neither an
            // I/O error nor a panic inside the compiler's own queries.
            let written = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                record::write(tcx, directory.as_ref())
            }));
            match written {
                Ok(Ok(())) => {}
                Ok(Err(error)) => eprintln!("pion-rustc: could not write references: {error}"),
                Err(_) => eprintln!(
                    "pion-rustc: recording references panicked for crate `{}`; no record was written",
                    tcx.crate_name(rustc_hir::def_id::LOCAL_CRATE)
                ),
            }
        }
        Compilation::Continue
    }
}

/// Set for the process that compiles and records.
const INNER: &str = "PION_RUSTC_INNER";

fn main() -> ExitCode {
    // cargo calls a workspace wrapper as `<wrapper> <rustc> <args…>`.
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if std::env::var_os(INNER).is_some() {
        return rustc_driver::catch_with_exit_code(|| {
            rustc_driver::run_compiler(&arguments, &mut Recorder)
        });
    }

    // Compile and record in a child process, and hold back what it prints.
    // If it fails for any reason, a real compile error or a fault in the
    // recorder, run the untouched rustc instead and let that speak: the
    // build then behaves exactly as it would without this wrapper.
    let recorded = std::env::current_exe().and_then(|wrapper| {
        std::process::Command::new(wrapper)
            .args(&arguments)
            .env(INNER, "1")
            .output()
    });
    let marker = failure_marker(&arguments);
    if let Ok(output) = &recorded {
        if output.status.success() {
            use std::io::Write as _;
            let _ = std::io::stdout().write_all(&output.stdout);
            let _ = std::io::stderr().write_all(&output.stderr);
            if let Some(marker) = &marker {
                let _ = std::fs::remove_file(marker);
            }
            return ExitCode::SUCCESS;
        }
    }
    let Some((rustc, rest)) = arguments.split_first() else {
        eprintln!("pion-rustc: expected `<rustc> <args…>`");
        return ExitCode::FAILURE;
    };
    match std::process::Command::new(rustc).args(rest).status() {
        Ok(status) if status.success() => {
            // The crate compiles, so the recorder is what failed. Leave a
            // marker: whoever reads the records must know this crate's are
            // missing or stale, and not take silence for "nothing refers".
            if let Some(marker) = &marker {
                let package = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
                if let Some(directory) = marker.parent() {
                    let _ = std::fs::create_dir_all(directory);
                }
                let _ = std::fs::write(marker, package);
            }
            ExitCode::SUCCESS
        }
        Ok(status) => ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1)),
        Err(error) => {
            eprintln!("pion-rustc: could not run `{rustc}`: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Where to note that recording failed for the crate these arguments
/// compile: `$PION_REFS_DIR/<crate>.<test|build>.failed`. `None` when no
/// records are being written or the arguments name no crate.
fn failure_marker(arguments: &[String]) -> Option<std::path::PathBuf> {
    let directory = std::env::var_os("PION_REFS_DIR")?;
    let name = arguments
        .iter()
        .position(|argument| argument == "--crate-name")
        .and_then(|index| arguments.get(index + 1))?;
    let kind = if arguments.iter().any(|argument| argument == "--test") {
        "test"
    } else {
        "build"
    };
    Some(std::path::Path::new(&directory).join(format!("{name}.{kind}.failed")))
}
