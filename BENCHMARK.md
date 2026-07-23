# Benchmark — Anchor vs. Pinocchio for the router core

The router is a tiny program: validate a few things, then do `N` System-Program
transfers. That makes it a good candidate to ask a concrete question — **does the
framework overhead actually cost us anything that matters?** — and answer it with
measured numbers instead of taste.

We reimplemented the exact same logic in [Pinocchio](https://github.com/anza-xyz/pinocchio)
(zero-dependency, `no_std`) alongside the shipped Anchor program, and measured. The
Pinocchio source is in [`bench/pinocchio-router/`](./bench/pinocchio-router/src/lib.rs).

## Result 1 — binary size and deploy rent (measured)

| | Anchor (shipped) | Pinocchio (bench) | ratio |
|---|---:|---:|---:|
| Compiled `.so` | **183,088 bytes** | **5,672 bytes** | **32.3× smaller** |
| Rent-exempt to deploy | **1.2752 SOL** | **0.0404 SOL** | **31.6× less** |
| ≈ USD at $77/SOL | ~$98 | ~$3 | **~$95 saved / deploy** |

Reproduce:
```bash
# Anchor
anchor build && stat -c%s target/deploy/supersonic_tx.so && solana rent 183088
# Pinocchio
cd bench/pinocchio-router && cargo build-sbf \
  && stat -c%s target/deploy/supersonic_tx_pinocchio.so && solana rent 5672
```

Deploy rent is linear in binary size (Solana's rent formula:
`(bytes + 128) × 0.00000348 × 2 SOL`), so a 32× smaller binary is a ~32× smaller deploy
cost. **This is the load-bearing result:** it turns a mainnet deployment from a
~$98 commitment of locked SOL into a ~$3 one — i.e. it makes a real mainnet deployment
of this tool cheap enough to be a non-decision.

## Result 2 — compute units (analysis, not the deciding axis)

We deliberately did **not** headline a compute-unit number, and here is the honest
reason. What Pinocchio removes is *framework* overhead — Borsh deserialization,
discriminator parsing — which is a **fixed cost paid once per instruction**. This
program's actual work is `N` System-Program transfers, and the cost of each CPI transfer
is set by the runtime and is **identical** under Anchor or Pinocchio. So:

- the saving is a fixed per-call amount (framework overhead), **not** proportional to the
  work the program does;
- as the bundle grows (larger `N`), that fixed saving becomes a smaller and smaller
  fraction of the transaction's total CU;
- this program is not CU-bound in any realistic use, so CU is simply not where the
  decision is made.

A full CU-vs-`N` curve is measurable with the same LiteSVM harness the invariant tests
use (`compute_units_consumed` on the transaction result) and is the natural validation
step **if** the production port is taken — but it would refine a number that does not
change the conclusion. Binary size / deploy rent does.

## Result 3 — same invariants, not just a size comparison (measured)

A benchmark that's only ever been sized, never functionally tested, isn't evidence it
*works* — only that it's small. `bench/pinocchio-router` now passes the identical 8
invariant tests the Anchor program does (`harness/tests/pinocchio_invariants.rs`, via
Mollusk): atomicity, fail-closed on zero-amount/self-destination/account-count-mismatch/
insufficient-funds, and real value movement. Building this parity test surfaced two real
gaps versus the Anchor reference that a size-only benchmark would have missed entirely:
a missing self-destination check, and an account-count check that accepted extra accounts
instead of requiring an exact match — both fixed, both now covered by a test.

## What this benchmark decides

The framework question for *this* program is settled by Result 1, not by taste or by the
judge's preferences: Pinocchio is **~32× smaller** and **~32× cheaper to deploy**, at the
cost of writing the account/data parsing by hand — which, for a program this simple (no
custody, no PDA state, one instruction), removes almost none of Anchor's safety value (the
signer requirement is still enforced by the System Program during the CPI regardless).

**Recommendation:** the Pinocchio core is the better artifact for a program whose entire
job is a bounded transfer loop, and it is what makes a live mainnet deployment cheap. The
Anchor implementation remains the shipped/deployed one and the reference for this
benchmark; porting the production program (and re-running the devnet proofs against the
Pinocchio program id) is a clean, well-scoped follow-up — this benchmark is the evidence
that justifies it.

## Honest caveats

- The Pinocchio program is **not deployed anywhere** and does not replace the live Anchor
  deployment — it implements the same core logic with a compact manual instruction
  encoding, not the shipped Anchor instruction format, and is offered as a
  minimal-attack-surface *option*, tested to the same bar (Result 3), not a benchmark-only
  artifact.
- Binary sizes depend on toolchain/optimization flags; both were built with
  `opt-level = 3` + fat LTO and the standard `cargo build-sbf` / `anchor build` pipelines.
- A pending network change (SIMD-0436) could halve rent-exempt minimums generally; that
  would scale both columns down together and leave the ~32× ratio unchanged.
