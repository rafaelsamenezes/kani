// Copyright Kani Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Verify a Kani-produced goto binary with ESBMC instead of CBMC.
//!
//! ESBMC reads CBMC goto-binaries natively (`esbmc --binary prog.out`), detecting
//! the format by magic header and synthesising the CPROVER additions in-process.
//! No conversion step is involved.
//!
//! This is the MVP: it reports a single SUCCESSFUL/FAILED verdict per harness by
//! reading ESBMC's terminal banner. Per-property results need a machine-readable
//! emitter on the ESBMC side and are not wired up here.

use anyhow::{Result, bail};
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use kani_metadata::HarnessMetadata;

use crate::args::common::Verbosity;
use crate::call_cbmc::{ExitStatus, FailedProperties, VerificationResult, VerificationStatus};
use crate::session::KaniSession;
use crate::util::render_command;

/// Name of the ESBMC executable. Looked up on `PATH`; unlike CBMC, Kani does not
/// pin or bundle a version.
const ESBMC_BIN: &str = "esbmc";

impl KaniSession {
    /// Verify a goto binary with ESBMC.
    pub fn run_esbmc(
        &self,
        file: &Path,
        harness: &HarnessMetadata,
    ) -> Result<VerificationResult> {
        let args = self.esbmc_flags(file, harness)?;

        let mut cmd = Command::new(ESBMC_BIN);
        cmd.args(&args);

        if self.args.common_args.verbose() {
            println!("[Kani] Running: `{}`", render_command(&cmd).to_string_lossy());
        }

        let start_time = Instant::now();
        let output = cmd.output().map_err(|e| {
            anyhow::anyhow!(
                "Failed to run `{ESBMC_BIN}`: {e}. The ESBMC backend requires `{ESBMC_BIN}` on your PATH."
            )
        })?;
        let runtime = start_time.elapsed();

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !self.args.common_args.quiet {
            print!("{stdout}");
            eprint!("{stderr}");
        }

        Ok(interpret_esbmc_output(&stdout, &stderr, output.status.code(), runtime))
    }

    /// Build the ESBMC command line for `file`.
    fn esbmc_flags(&self, file: &Path, harness: &HarnessMetadata) -> Result<Vec<OsString>> {
        let mut args: Vec<OsString> = Vec::new();

        // Read the CBMC goto-binary directly.
        args.push("--binary".into());
        args.push(file.to_owned().into_os_string());

        // Deliberately NOT passing `--function <harness>`.
        //
        // Kani emits one goto binary per harness, and CBMC has already generated
        // that binary's `__CPROVER__start` to run `__CPROVER_initialize` and then
        // call this harness. ESBMC bridges its synthesised `__ESBMC_main` onto
        // `__CPROVER__start` on its own, so the harness is still the entry point.
        //
        // `--function` instead retargets straight at the harness body, which skips
        // `__CPROVER_initialize` entirely: every `static` is then read as
        // uninitialised, so constants read as nondet and vtables dispatch into
        // Kani's `undefined function should be unreachable` stubs. That produced
        // several hundred spurious counterexamples across `tests/kani`.

        // Kani already emits Rust-semantics checks into the goto model, and ESBMC
        // runs its own goto_check over the loaded binary. Without these two the
        // C-semantics checks are layered on top and over-report.
        //
        // Deliberately NOT blanket-disabling every check: Kani relies on some
        // checker-inserted UB guards (e.g. CBMC's `bit_count` check for
        // `ctlz(0)`/`cttz(0)`), so the selection has to stay per-check.
        args.push("--no-standard-checks".into());
        args.push("--no-library-assertions".into());

        // Kani assumes malloc cannot fail; see model-checking/kani#891.
        args.push("--force-malloc-success".into());

        if let Some(unwind_value) = crate::call_cbmc::resolve_unwind_value(&self.args, harness) {
            args.push("--unwind".into());
            args.push(unwind_value.to_string().into());
        }

        if !self.args.checks.unwinding_on() {
            args.push("--no-unwinding-assertions".into());
        }

        self.handle_esbmc_solver_args(&mut args)?;

        // ESBMC's own SIGALRM handler can still report an already-established
        // violation, which a hard process kill would discard.
        if let Some(timeout) = self.args.harness_timeout {
            let duration: Duration = timeout.into();
            args.push("--timeout".into());
            args.push(duration.as_secs().to_string().into());
        }

        args.extend(self.args.esbmc_args.iter().cloned());

        Ok(args)
    }

    /// Translate Kani's `--solver` onto ESBMC's solver flags.
    ///
    /// Kani's `CbmcSolver` enum is CBMC-specific. The SMT solvers map across; the
    /// SAT solvers do not, and are rejected in `args::validate` rather than being
    /// silently ignored.
    fn handle_esbmc_solver_args(&self, args: &mut Vec<OsString>) -> Result<()> {
        use kani_metadata::CbmcSolver;

        let Some(solver) = &self.args.solver else { return Ok(()) };

        match solver {
            CbmcSolver::Bitwuzla => args.push("--bitwuzla".into()),
            CbmcSolver::Z3 => args.push("--z3".into()),
            CbmcSolver::Cvc5 => args.push("--cvc5".into()),
            // Kani's default is Cadical, which the user did not choose. Fall back
            // to ESBMC's default rather than erroring.
            CbmcSolver::Cadical => {}
            other => bail!(
                "The ESBMC backend does not support the `{}` solver. \
                 Supported: bitwuzla, z3, cvc5.",
                other.as_ref()
            ),
        }
        Ok(())
    }
}

/// Map ESBMC's output onto a [`VerificationResult`].
///
/// Note this deliberately does NOT reuse `VerificationResult::from`, which derives
/// status purely from the property table and ignores the exit code. ESBMC aborts,
/// segfaults and times out on a non-trivial slice of real harnesses; routing those
/// through a property-less `Ok(vec![])` would report them as SUCCESSFUL.
fn interpret_esbmc_output(
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
    runtime: Duration,
) -> VerificationResult {
    let combined_tail = || {
        let s = if stderr.trim().is_empty() { stdout } else { stderr };
        s.lines().rev().take(5).collect::<Vec<_>>().join("\n")
    };

    // ESBMC writes its verdict banner to *stderr*, not stdout (`log_result` /
    // `log_fail` in bmc.cpp). Scan both so this keeps working if that changes.
    let has = |needle: &str| stdout.contains(needle) || stderr.contains(needle);

    let status = if has("VERIFICATION SUCCESSFUL") {
        Some(VerificationStatus::Success)
    } else if has("VERIFICATION FAILED") {
        Some(VerificationStatus::Failure)
    } else {
        None
    };

    match status {
        Some(VerificationStatus::Success) => VerificationResult {
            status: VerificationStatus::Success,
            failed_properties: FailedProperties::None,
            results: Ok(vec![]),
            runtime,
            generated_concrete_test: false,
            coverage_results: None,
        },
        Some(VerificationStatus::Failure) => VerificationResult {
            status: VerificationStatus::Failure,
            failed_properties: FailedProperties::Other,
            results: Ok(vec![]),
            runtime,
            generated_concrete_test: false,
            coverage_results: None,
        },
        // No verdict: a timeout, a crash, or a graceful decline from the CBMC
        // adapter. All of these are failures, never successes.
        None => {
            let timed_out = stdout.contains("Timed out")
                || stderr.contains("Timed out")
                || matches!(exit_code, Some(124) | Some(137));
            eprintln!("[Kani] ESBMC did not produce a verdict:\n{}", combined_tail());
            VerificationResult {
                status: VerificationStatus::Failure,
                failed_properties: FailedProperties::None,
                results: Err(if timed_out {
                    ExitStatus::Timeout
                } else {
                    ExitStatus::Other(exit_code.unwrap_or(-1))
                }),
                runtime,
                generated_concrete_test: false,
                coverage_results: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn successful_run_is_success() {
        let r = interpret_esbmc_output("...\nVERIFICATION SUCCESSFUL\n", "", Some(0), secs(1));
        assert_eq!(r.status, VerificationStatus::Success);
    }

    #[test]
    fn failed_run_is_failure() {
        let r = interpret_esbmc_output("...\nVERIFICATION FAILED\n", "", Some(1), secs(1));
        assert_eq!(r.status, VerificationStatus::Failure);
    }

    /// ESBMC actually writes its verdict to stderr; reading only stdout made every
    /// harness look like a crash.
    #[test]
    fn verdict_on_stderr_is_read() {
        let ok = interpret_esbmc_output("", "VERIFICATION SUCCESSFUL\n", Some(0), secs(1));
        assert_eq!(ok.status, VerificationStatus::Success);
        assert!(ok.results.is_ok());

        let bad = interpret_esbmc_output("", "VERIFICATION FAILED\n", Some(1), secs(1));
        assert_eq!(bad.status, VerificationStatus::Failure);
        assert!(bad.results.is_ok(), "a real verdict must not be reported as a crash");
    }

    /// The case that matters: a crash must never be reported as success.
    #[test]
    fn crash_without_verdict_is_failure_not_success() {
        let r = interpret_esbmc_output("", "Segmentation fault", Some(139), secs(1));
        assert_eq!(r.status, VerificationStatus::Failure);
        assert!(matches!(r.results, Err(ExitStatus::Other(139))));
    }

    #[test]
    fn adapter_decline_is_failure() {
        let out = "ERROR: CBMC adapter: __CPROVER_DYNAMIC_OBJECT is not yet supported\n";
        let r = interpret_esbmc_output(out, "", Some(6), secs(1));
        assert_eq!(r.status, VerificationStatus::Failure);
    }

    #[test]
    fn timeout_is_reported_as_timeout() {
        let r = interpret_esbmc_output("ERROR: Timed out\n", "", Some(1), secs(1));
        assert_eq!(r.status, VerificationStatus::Failure);
        assert!(matches!(r.results, Err(ExitStatus::Timeout)));
    }
}
