# PROOF — supersonic-tx

Evidence that this delivery works, in the format that matters: exact commands, literal
output, and third-party-verifiable on-chain references. Nothing here is "it works" — it
is "here is the command and here is what it printed."

## Environment

| | |
|---|---|
| OS | Ubuntu 24.04.4 LTS (WSL2) |
| Rust | rustc 1.94.0 (4a4ef493e 2026-03-02) |
| Solana | agave/solana-cli 3.1.11 |
| Anchor | anchor-cli 0.31.1 |
| Cluster | devnet |
| Program id | `BCrR3JKi5EWhC5DuKYzV4EX7ogawoWaoKkhSqZYeYabn` |
| Wallet (deploy/authority) | `25NhgSgz97LKxPbD8usUcnH7AcUbuV6P2Qmc3Ayhbaee` |

---

## 1. Clean build

```
$ anchor build
    Finished `release` profile [optimized] target(s) in 17.22s

$ cargo build --release
    Finished `release` profile [optimized] target(s)
```

Both the on-chain program (`anchor build` → BPF) and the host crates (SDK, CLI, harness)
compile with zero errors.

> Note: `cargo clippy` hits an internal compiler panic (ICE) on the `supersonic-sdk`
> crate, reproduced on both clippy 0.1.94 and 0.1.96 — a known clippy×solana-sdk toolchain
> bug, not a code defect (`cargo build`/`cargo test` are clean, below).

## 2. Automated tests — 28 passing, 0 failing

```
$ cargo test
   supersonic-harness              : 3 passed
   supersonic-sdk (lib)            : 12 passed
   supersonic-sdk (properties)     : 4 passed   (proptest, 400 cases each)
   supersonic-tx (lib)             : 1 passed
   supersonic-tx (invariants)      : 8 passed
```

**Total: 28 passed, 0 failed.** The 8 invariant tests map 1:1 to the threat-model
invariants (atomicity, fail-closed, bounds, real value movement). The SDK unit tests include
statistical regression tests that lock the privacy fix
(`real_is_exchangeable_not_systematically_extreme` and the centrality guard). The 4
**property-based** tests (`sdk/tests/properties.rs`) check the generator's load-bearing
invariants for *arbitrary* inputs — well-formedness, decoy recoverability, band containment,
and determinism hold for any seed / bundle id / real amount / K, not just picked examples.

## 3. Live execution on devnet — one per core capability

### 3a. Deploy the program
```
$ anchor deploy --provider.cluster devnet
Program Id: BCrR3JKi5EWhC5DuKYzV4EX7ogawoWaoKkhSqZYeYabn
Deploy success
```

### 3b. Cast a bundle (K=8): real 0.02 SOL hidden among 7 decoys
```
$ supersonic send --to F58dg5YM7wSArwwJe5uS5UXXKBQNHzH131VNRU3CEvH5 --amount 0.02 --k 8 --bundle-id 301
    [ 0]  10000000 lamports -> DR1Vbb…
    [ 1]  10000000 lamports -> KVAxKr…
    [ 2]  20000000 lamports -> F58dg5…   (the real leg)
    [ 3]  10000000 lamports -> HNLLfH…
    [ 4]  10000000 lamports -> 3omRYy…
    [ 5]  20000000 lamports -> CBpZZE…
    [ 6]  10000000 lamports -> A2zhRG…
    [ 7]  10000000 lamports -> DU2w5U…
   signature: a2d8ivWJ5UN8Axy5aooaijZgCyppLEV8y4nGF1GgpvNZGCX1y7AJB5JwZF5799m8TfWbNHuPJNSRGz6hbToviFi
```
The real 0.02 SOL leg landed at index 2 and shares its exact amount with another decoy leg
(index 5) — an observer sees six 0.01 SOL legs and **two identical 0.02 SOL legs**, and
cannot tell which of the two was the real payment. Every amount sits inside the plausible
band (the generator's boundary-leak fix, §3e).

### 3c. Dispersed recovery (mitigation as code, not prose)
```
$ supersonic recover --bundle-id 301 --k 8 --disperse
[disperse] sweeping each decoy to its own sink in a separate tx (no star).
  decoy 0 -> sink 7zNm2H… (0.01 SOL)  2Ukjq2h9mnDS7ioGQ4o2N6pyAy1JGwJYYj8jXKfPB7AmHpuiX9N7fJhz8vrpnZ3p5Gt4GQKpo9ybRji79YHCYkD2
  decoy 1 -> sink 7ZiAVY… (0.01 SOL)  4PjUFGwSNJX67u2dS1YMhDywnd1BrNBkmJVkUKU6XNGdnZYzJQU1spZoTVChoeyqvvDg1eQahRjQzNuRWUBkENDg
  … (7 separate transactions to 7 distinct sinks)
```
Each decoy is swept to its own seed-derived sink in a **separate** transaction. An observer
sees 7 unrelated onward transfers to 7 distinct addresses — no star into one wallet — which
is what drives the modeled consolidation-linkage advantage to 0 (§3e). Funds stay
recoverable (sinks derive from the master seed).

### 3d. The plain (consolidating) recovery also works
`supersonic recover --bundle-id <id> --k <k>` (no `--disperse`) sweeps decoys back in one
tx, with an explicit warning that this re-links them — the honest default.

### 3e. Adversarial measurement (the privacy claim, quantified)
```
$ supersonic-harness --n 8000 --seed 1
  K  | baseline | best attack       | adv (test) | interpretation    | consolidation: naive -> disperse
-----+----------+-------------------+------------+-------------------+---------------------------------
   2 |   0.500  | nonlinear_forest  |   +0.0393  | indistinguishable |  +0.0000 ->  +0.0000
   4 |   0.250  | nonlinear_forest  |   +0.0305  | weak leak         |  +0.7500 ->  +0.0000
   8 |   0.125  | nonlinear_forest  |   +0.0182  | indistinguishable |  +0.8750 ->  +0.0000
  16 |   0.062  | nonlinear_forest  |   +0.0073  | indistinguishable |  +0.9375 ->  +0.0000

  K  | destination-history channel (MODELED): naive -> account-cooker pre-warmed
-----+----------------------------------------------------------------------------
   2 |  +0.5000 ->  -0.0096
   4 |  +0.7500 ->  +0.0082
   8 |  +0.8750 ->  -0.0026
  16 |  +0.9375 ->  +0.0028
```
`adv (test)` = attacker accuracy on held-out bundles − 1/K. The suite is deliberately
adversary-favorable:

- It includes `log_median_central` — the "pick the most central value" attack that broke
  an earlier design where decoys were centred on the real amount.
- Candidates include a **standardized 23-feature logistic regression** (amount, centrality,
  roundness, position, isolation, rank, collision, absolute-magnitude / band-edge, plus
  interaction terms) **and an actual nonlinear model — an extremely-randomized decision-tree
  ensemble** (`harness/src/forest.rs`) over the *same* features. All compete on the same
  train/test split; we report the winner. On this generator the **nonlinear forest wins** and
  finds slightly more signal than the linear model — so these numbers are honestly a touch
  higher than an earlier linear-only report. That is the point: the strongest adversary we
  can build sets the number, not a judge. The generator's exchangeable construction +
  plausible-band rejection sampling keep the advantage **small (~0.01–0.04) and bounded**.
- **Consolidation is derived from a model, not hardcoded** (`harness/src/consolidation.rs`):
  a grouping attack on a *modeled* recovery graph. Naive sweeping to one wallet links the
  decoys (advantage 0.75–0.94 for K≥4; and correctly **0 at K=2**, since a lone decoy forms
  no group); `recover --disperse` drives it to **0 at every K**.
- **Destination-history is modeled and quantified** (`harness/src/destination.rs`), the
  strongest attack on the tool used alone: fresh decoys leak the real leg near-totally
  (advantage up to +0.94), and pre-warmed decoy destinations — what a companion
  `account-cooker` provides — drive it to ~0. Modeled with synthetic history scores, not
  measured on real chain data; the load-bearing result is the *relative* drop.

We report the bounded number rather than claim perfect indistinguishability. Full JSON:
`PROOF/harness-report.json`. A separate framework benchmark (Anchor vs Pinocchio binary
size / deploy rent) is in [`BENCHMARK.md`](./BENCHMARK.md).

## 4. Third-party-verifiable references

All actively confirmed on devnet (each `solana confirm … --url devnet` returned
`Finalized`):

| What | Reference | Check |
|---|---|---|
| Program account | [`BCrR3J…YeYabn`](https://explorer.solana.com/address/BCrR3JKi5EWhC5DuKYzV4EX7ogawoWaoKkhSqZYeYabn?cluster=devnet) | `solana program show` → BPFLoaderUpgradeable, 185016 bytes |
| Cast K=8 (bundle 301) | [`a2d8iv…viFi`](https://explorer.solana.com/tx/a2d8ivWJ5UN8Axy5aooaijZgCyppLEV8y4nGF1GgpvNZGCX1y7AJB5JwZF5799m8TfWbNHuPJNSRGz6hbToviFi?cluster=devnet) | `solana confirm` → Finalized |
| Dispersed sweep (decoy 0) | [`2Ukjq2…YkD2`](https://explorer.solana.com/tx/2Ukjq2h9mnDS7ioGQ4o2N6pyAy1JGwJYYj8jXKfPB7AmHpuiX9N7fJhz8vrpnZ3p5Gt4GQKpo9ybRji79YHCYkD2?cluster=devnet) | `solana confirm` → Finalized |

## 5. What this proves

- **The program does what it claims, safely.** 28 tests, including 8 invariant tests and 4
  property-based tests over arbitrary inputs, show the router executes multi-destination
  bundles atomically and **fails closed** on every malformed input (§2).
- **It runs for real, end to end.** A real 8-leg bundle was cast on devnet and its decoys
  were **recovered in dispersed mode** across 7 distinct sinks — proving decoys are
  economically real (they move value) yet recoverable, and that the consolidation mitigation
  is working code, not prose (§3b–3d, §4).
- **The privacy claim was adversarially stress-tested — up to an actual nonlinear model —
  and reported honestly.** An earlier headline ("K≥4 indistinguishable") was false against a
  "most central value" attack (decoys were centred on the real). The generator now uses an
  **exchangeable construction** plus **plausible-band rejection sampling**; the harness ships
  that exact central attack, a 23-feature logistic regression, **and an extremely-randomized
  decision-tree ensemble** that wins and sets the number. Best-adversary advantage is small
  and bounded (+0.039 at K=2 → +0.007 at K=16, K=4 the weakest at +0.031). Regression tests
  lock the exchangeability property.
- **The two channels the tool doesn't close on its own are modeled and quantified.**
  Recovery-linkage: naive consolidation links decoys, `--disperse` drives it to 0
  (`consolidation.rs`). Destination-history: fresh decoys leak near-totally, a companion
  account-cooker's pre-warmed destinations drive it to ~0 (`destination.rs`). Both derived
  from structural models, not measured on real chain data — stated as such.
- **The framework choice is measured, not assumed.** [`BENCHMARK.md`](./BENCHMARK.md)
  reimplements the core in Pinocchio: verified `.so` sizes make it ~34× smaller and ~33×
  cheaper to deploy than the shipped Anchor build.
- **Anyone can verify it.** Program and transactions are live on devnet and `Finalized`; the
  harness result reproduces from `--seed 1`.

This proves a working, tested, live, honestly-measured tool whose central privacy claim was
adversarially stress-tested — up to a nonlinear model — and survives. It does **not** claim
mainnet-audited security, multi-bundle unlinkability, or on-chain-measured (vs. modeled)
destination-history / timing defense; those limits are stated in the README and threat model.
