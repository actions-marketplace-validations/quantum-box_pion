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

fn main() -> ExitCode {
    // cargo calls a workspace wrapper as `<wrapper> <rustc> <args…>`.
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    rustc_driver::catch_with_exit_code(|| rustc_driver::run_compiler(&arguments, &mut Recorder))
}
