- **Feature Name:** ESBMC backend (`esbmc`)
- **Feature Request Issue:** [#4773](https://github.com/model-checking/kani/issues/4773)
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
2. **Differential testing of the verifier itself.** Because both backends consume *the same goto binary*, a disagreement is a bug in one of the two tools — a signal Kani cannot currently produce. This is the strongest argument for the feature, and it is a benefit to Kani developers as much as to users. The proof-of-concept has already produced such signal: a soundness gap on the ESBMC side, and a class of crashes ESBMC's own maintainers have since reproduced (see *Current state* and *Open questions*). To be clear about the direction of that signal so far, it has almost all pointed at ESBMC; the sole test where ESBMC succeeds and CBMC does not is a `fixme`-marked one, which is not evidence of a CBMC bug.
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

**Unsupported constructs usually fail loudly, but there are two holes.** Where ESBMC's CBMC adapter cannot translate part of the model it declines with a named error, and Kani turns a no-verdict run — crash, timeout, or declined construct — into a failure rather than a success. Two measured exceptions matter to users, and neither is fixed yet:

- **A crash is presented as a verification failure.** When ESBMC segfaults, the user sees `VERIFICATION:- FAILED`, which is indistinguishable from a genuine counterexample. It is the safe direction to fail in, but it tells the user their proof is broken when in fact the checker died. On Kani's own suite, 9 harnesses "agreed" with CBMC purely because a crash happened to coincide with an expected failure.
- **A vacuous run is presented as a success.** On 8 harnesses ESBMC evaluates *zero* properties and reports `SUCCESSFUL`. `Coroutines/rustc-coroutine-tests/smoke.rs` is the extreme case: 841 properties under CBMC, 0 under ESBMC. Nothing in the current output distinguishes "verified" from "checked nothing".

**Expect timeouts and memory exhaustion, not just slowness.** Harnesses that CBMC finishes quickly can exhaust ESBMC, particularly those exercising heap-allocating `std` code. Memory is a category of its own: 30 harnesses exceed an 8 GB cap, and several exceed 40 GB when run unbounded.

**Contracts do not work yet.** `requires`/`ensures` are already `ASSERT`/`ASSUME` instructions in the goto model by the time the backend sees it, so `#[kani::proof_for_contract]` ought to be within reach — but in practice every one of the 11 `FunctionContracts` tests fails: 6 decline on zero-initialising `allocate` and 5 on `__CPROVER_DYNAMIC_OBJECT`, both emitted by contract instrumentation. `modifies` clauses are not enforced either.

## Current state

Measured on ESBMC 8.5.0 against Kani's own `tests/kani`, both backends driven through the identical runner and consuming a goto binary produced by the same pipeline.

The comparable set is the 527 tests the CBMC arm passes, excluding `fixme`-marked ones. A test the CBMC arm does not pass is either Kani/CBMC future work or a limitation of the harness used here, and says nothing about ESBMC.

**Agreement: 261/527 (~50%)**, counting only harnesses where ESBMC reaches the same verdict *for the same reason* — matched on the violated property, since both backends emit property tables keyed alike. Counting verdicts alone gives 282/527, but that figure is misleading: it credits crashes that coincide with an expected failure, and vacuous successes.

The 266 ESBMC does not get right are dominated by one thing, and it is not verification weakness:

| | count |
| --- | --- |
| ESBMC crashes (SIGSEGV during SMT encoding) | ~90 |
| Constructs the adapter declines cleanly | 56 |
| Genuine spurious counterexamples | 44 |
| Timeout or memory exhaustion | 30 |
| Vacuous successes (zero properties checked) | 8 |
| Missed bugs (unsound) | 4 |

The declines are concentrated and nameable: atomic sections 36 (emitted by Rust coroutines), SIMD 8, `symbolic_type` exception 8, `__CPROVER_DYNAMIC_OBJECT` 5, zero-initialising `allocate` 4.

### Performance

Timed on the harnesses where both backends agree for the same reason, each checker invocation isolated in its own cgroup pinned to a dedicated CPU, median of 3 runs, compilation excluded.

| | median | p95 | max |
| --- | --- | --- | --- |
| CBMC | 124 ms | 529 ms | 14.6 s |
| ESBMC | 1122 ms | 1330 ms | 16.8 s |

Read as a headline that is ~8x slower per harness, but the shape matters more than the ratio: **roughly 90% of ESBMC's time on these harnesses is a fixed startup cost, not solving.** Its internal breakdown is 902 ms median building the GOTO program — it re-parses and re-converts the synthesised CPROVER model on every invocation — against 15 ms of symbolic execution and 3 ms in the decision procedure.

Two consequences:

- Net of that constant, the aggregate ratio is 0.73x rather than 2.80x, and ESBMC is faster on 101 of 260 harnesses rather than 6.
- On the harnesses where CBMC needs more than 5 seconds, ESBMC wins all 6 — `Intrinsics/Rotate/rotate_right` 14.6 s to 4.3 s, `LayoutRandomization/should_fail` 10.3 s to 1.8 s. That is only six data points, so it indicates a direction rather than establishing a rule.

Caching the compiled additions instead of rebuilding them per run is therefore the highest-value performance work available, and it is ESBMC-side.

### Effect of the ESBMC version

ESBMC 8.4.0 and 8.5.0 produce **identical verdicts on all 586 tests** — no regressions, no fixes, no verdict changed in either direction, and the same peak-memory distribution. The crash class described above is present in the latest upstream.

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
- **A crash class that dominates the gap.** Around 90 harnesses SIGSEGV inside ESBMC's SMT conversion of the CBMC-derived model. Backtraces converge on two sites — `smt_solver_baset::convert_ast`, and a null `type2t` dereference in `gen_zero` reached from `convert_bitcast` — so this is a small number of bugs, not ninety. It reproduces on a three-line harness (`tests/kani/Assert/assert2018.rs`: 6 VCCs, symex in 0.04 s, then a segfault), and crashing on this path is not unique to Kani's models: ESBMC's own [#7408](https://github.com/esbmc/esbmc/pull/7408) records a `--binary`-path SIGSEGV during SMT encoding, reached from `__CPROVER_OBJECT_SIZE`. Whether that is the same defect as the two sites above has not been established. This is ESBMC-side work either way, but it gates the feature, since no amount of Kani-side polish helps a checker that dies.
- **Detecting vacuity.** A backend that evaluates zero properties currently reports `SUCCESSFUL`. Kani knows how many properties the goto model contains and could refuse a success that checked none of them. Should that check live in Kani, so it protects any future backend, rather than in each backend?
- **Distinguishing a crash from a counterexample.** A no-verdict run is reported as `VERIFICATION:- FAILED`. Safe, but wrong in what it tells the user. What should Kani print when the checker dies?
- **Version compatibility.** Which ESBMC versions are supported, how is that checked, and what happens on goto-binary format drift? Two consecutive releases (8.4.0, 8.5.0) behave identically here, which says the interface is stable so far, not that it is pinned.
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
