- **Feature Name:** ESBMC backend (`esbmc`)
- **Feature Request Issue:** *TBD — open a feature request and link it here*
- **RFC PR:** *TBD*
- **Status:** Under Review
- **Version:** 0
- **Proof-of-concept:** [rafaelsamenezes/kani, branch `esbmc-backend-mvp`](https://github.com/rafaelsamenezes/kani/tree/esbmc-backend-mvp)

-------------------

## Summary

Allow Kani to hand the goto model it already produces to [ESBMC](https://github.com/esbmc/esbmc) instead of CBMC, behind `-Z esbmc`.
Kani's compilation pipeline is untouched: the same goto binary is verified by a second, independently developed model checker.

## User Impact

Kani today has exactly one verification engine. Every Kani verdict — including the `SUCCESSFUL` ones — rests on CBMC being right.
When CBMC reports a spurious counterexample, times out, or (worst case) misses a bug, a Kani user has no second opinion to appeal to.

A second backend gives users three things:

1. **A second opinion on hard harnesses.** ESBMC and CBMC use different symbolic execution engines, different memory models, and different SMT encodings. A harness that ESBMC dispatches in seconds may be one CBMC cannot finish, and vice versa. Users stuck on a timeout gain something to try that is not "reduce the bound".
2. **Differential testing of the verifier itself.** Because both backends consume *the same goto binary*, a disagreement is a bug in one of the two tools — a signal Kani cannot currently produce. This is the strongest argument for the feature, and it is a benefit to Kani developers as much as to users. The proof-of-concept has already produced such signal in both directions (see *Open questions*).
3. **Solver reach.** ESBMC drives Bitwuzla, Z3 and CVC5, and has its own incremental SMT strategies.

The downsides are real and should be stated plainly:

- **A second thing to keep working.** Kani pins and bundles a specific CBMC version. A second backend means a second compatibility surface, and goto-binary format drift will break it.
- **Divergent results are confusing.** Two backends that disagree put the user in the position of adjudicating. Kani must never present a `-Z esbmc` success as equivalent evidence to a CBMC success until the gaps below are closed.
- **Feature parity is far off.** The MVP reports a single verdict per harness. No property table, no counterexamples, no coverage, no concrete playback.

This RFC proposes the feature as **unstable**, precisely so that these can be worked through in the open.

## User Experience

Users opt in per invocation:

```
kani main.rs -Z esbmc
cargo kani -Z esbmc
```

`esbmc` must be on `PATH`. Kani bundles CBMC; it does **not** bundle ESBMC, and this RFC does not propose that it should. If the binary is missing, Kani errors out at the point of invocation rather than falling back to CBMC — a silent fallback would defeat the purpose of asking for a second opinion.

A passing harness looks exactly as it does today:

```rust
// kani main_true.rs -Z esbmc
#[kani::proof]
fn main() {
    let a: u8 = kani::any();
    kani::assume(a < 100);
    assert!(a as u16 + 1 > a as u16);
}
```
```
VERIFICATION:- SUCCESSFUL
Complete - 1 successfully verified harnesses, 0 failures, 1 total.
```

and a failing one:

```rust
// kani main_false.rs -Z esbmc
#[kani::proof]
fn main() {
    let a: u8 = kani::any();
    let b: u8 = kani::any();
    assert!(a >= b);
}
```
```
VERIFICATION:- FAILED
Verification failed for - main
```

### What is different from the CBMC backend

**Only a top-level verdict.** Kani's per-property result table, and the counterexample trace, are not reproduced. A user who needs to know *which* property failed must re-run without `-Z esbmc`, or read ESBMC's own output. This is the single largest UX gap and the first thing to fix after the MVP.

**Unsupported combinations are hard errors, not silent no-ops.** Passing any of the following together with `-Z esbmc` fails argument validation with a message naming the conflict:

| Option | Reason |
| --- | --- |
| `--cbmc-args` | CBMC-specific; `--esbmc-args` is the counterpart |
| `-Z lean` | a third backend cannot be selected at the same time |
| `--concrete-playback` | requires a counterexample trace |
| `--coverage` | requires per-property coverage results |
| `--extra-pointer-checks` | no ESBMC equivalent |
| `-Z loop-contracts` | would silently degrade to bounded unwinding |
| `--solver <SAT solver>` | only `bitwuzla`, `z3`, `cvc5` map across |

This is deliberate. Silently ignoring `--solver kissat` and then printing `SUCCESSFUL` is exactly the class of bug that destroys trust in a second backend.

**Unsupported constructs fail loudly.** Where ESBMC's CBMC adapter cannot translate part of the model, it declines with an error and Kani reports a failure — never a success. A no-verdict run (crash, timeout, declined construct) is always treated as a failure.

**Timeouts differ.** Harnesses that CBMC finishes quickly can exhaust ESBMC, particularly those exercising heap-allocating `std` code. Users should expect a different performance profile, not a uniformly faster or slower one.

**What already works.** `#[kani::proof_for_contract]` verifies, because `requires`/`ensures` are already `ASSERT`/`ASSUME` instructions in the goto model by the time the backend sees it. Note that `modifies` clauses are *not* enforced.

## Software Design

*Left empty for version 0, per the RFC template.*

The one architectural note worth recording early: the backend is a `kani-driver` change only. `kani-compiler` is untouched, and every `goto-instrument` pass is shared with the CBMC path except `--add-library`, which is skipped because ESBMC synthesises the CPROVER additions itself. Keeping the two pipelines otherwise identical is what makes the differential-testing benefit above real.

## Rationale and alternatives

**Why a flag rather than a separate tool?** Reusing Kani's frontend is the entire point. A standalone "ESBMC for Rust" would re-solve MIR-to-goto codegen and would not give the differential signal, because the two tools would no longer be verifying the same model.

**Why consume the goto binary rather than emit ESBMC's IR?** ESBMC already reads CBMC goto binaries. Reusing that path means Kani emits nothing new, and any future Kani codegen change is automatically picked up by both backends.

**Why not bundle ESBMC?** Bundling commits Kani to a version pin, a build, and a distribution story for a second large C++ dependency. Requiring it on `PATH` is right for an unstable feature; the question should be revisited at stabilization.

**Impact of not doing this.** Kani remains single-engine, and the class of bugs where CBMC and Kani are jointly wrong stays invisible.

**Precedent.** `-Z lean` established that an alternative backend can live behind an unstable flag, and `args/mod.rs` already carries a `// TODO: error out for other CBMC-backend-specific arguments`, so the possibility of non-CBMC backends is anticipated in the codebase.

## Open questions

These must be resolved before stabilization.

- **A confirmed soundness gap.** On the identical goto binary, `ctlz_nonzero`/`cttz_nonzero` applied to `0` is caught by CBMC's `bit_count` check and missed by ESBMC, which generates no VC for it at all. Kani does not emit its own assertion here; it relies on the checker synthesising the UB precondition. Any construct where Kani delegates a check to the backend is a place where a second backend can silently under-report, and this class needs an audit, not just a fix for this instance.
- **Version compatibility.** Which ESBMC versions are supported, how is that checked, and what happens on goto-binary format drift?
- **Property-level results.** What is the minimum output fidelity for stabilization — is a bare verdict ever acceptable, or must the property table be reproduced?
- **Counterexamples.** Without a trace there is no `--concrete-playback`, which is a significant part of how users act on a Kani failure.
- **Presenting disagreement.** If the two backends disagree, what should Kani tell the user?

## Out of scope / Future Improvements

- Per-property results, counterexample traces and `--concrete-playback`.
- `--coverage` support.
- Enforcement of `modifies` clauses.
- `-Z loop-contracts`.
- Bundling or pinning an ESBMC version as part of Kani's release.
- A CI job running the regression suite under both backends and reporting divergences.
