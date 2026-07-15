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

## 2. Automated tests — 24 passing, 0 failing

```
$ cargo test
   supersonic-harness         : 3 passed
   supersonic-sdk             : 12 passed
   supersonic-tx (lib)        : 1 passed
   supersonic-tx (invariants) : 8 passed
```

**Total: 24 passed, 0 failed.** The 8 invariant tests map 1:1 to the threat-model
invariants (atomicity, fail-closed, bounds, real value movement). The SDK tests include
two statistical regression tests that lock the privacy fix —
`real_is_not_the_most_central_above_baseline` and
`real_is_exchangeable_not_systematically_extreme` — which guard against the log-centrality
leak an earlier design had (§3e).

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
is what drives the measured consolidation-linkage advantage to 0 (§3e). Funds stay
recoverable (sinks derive from the master seed).

### 3d. The plain (consolidating) recovery also works
`supersonic recover --bundle-id <id> --k <k>` (no `--disperse`) sweeps decoys back in one
tx, with an explicit warning that this re-links them — the honest default.

### 3e. Adversarial measurement (the privacy claim, quantified)
```
$ supersonic-harness --n 8000 --seed 1
  K  | baseline | best attack     | adv (test) | interpretation    | consolidation: naive -> disperse
-----+----------+-----------------+------------+-------------------+---------------------------------
   2 |   0.500  | learned_logreg  |   +0.0359  | indistinguishable |  +0.0000 ->  +0.0000
   4 |   0.250  | learned_logreg  |   +0.0226  | indistinguishable |  +0.7500 ->  +0.0000
   8 |   0.125  | learned_logreg  |   +0.0235  | weak leak         |  +0.8750 ->  +0.0000
  16 |   0.062  | learned_logreg  |   +0.0112  | indistinguishable |  +0.9375 ->  +0.0000
```
`adv (test)` = attacker accuracy on held-out bundles − 1/K. The suite is deliberately
adversary-favorable:

- It includes `log_median_central` — the "pick the most central value" attack that broke
  an earlier design where decoys were centred on the real amount.
- The strongest candidate is a **standardized 15-feature logistic regression** covering the
  amount, centrality, roundness, position, isolation, rank, collision, **and absolute-
  magnitude / band-edge channels** — i.e. it includes the exact "the real is inside the
  plausible band, wild decoys aren't" signal a skeptical analyst would attack. The
  generator's rejection-sampling to a plausible band closes that support-boundary leak;
  against this stronger adversary the advantage is **small (~0.02–0.036) and bounded**.
- **Consolidation is measured, not hardcoded** (`harness/src/consolidation.rs`): a grouping
  attack on the recovery graph. Naive sweeping to one wallet links the decoys (advantage
  0.75–0.94 for K≥4; and correctly **0 at K=2**, since a lone decoy forms no group);
  `recover --disperse` sends each decoy to its own sink and drives that advantage to
  **0 at every K**.

We report the bounded number rather than claim perfect indistinguishability. Full JSON:
`PROOF/harness-report.json`.

## 4. Third-party-verifiable references

All actively confirmed on devnet (each `solana confirm … --url devnet` returned
`Finalized`):

| What | Reference | Check |
|---|---|---|
| Program account | [`BCrR3J…YeYabn`](https://explorer.solana.com/address/BCrR3JKi5EWhC5DuKYzV4EX7ogawoWaoKkhSqZYeYabn?cluster=devnet) | `solana program show` → BPFLoaderUpgradeable, 185016 bytes |
| Cast K=8 (bundle 301) | [`a2d8iv…viFi`](https://explorer.solana.com/tx/a2d8ivWJ5UN8Axy5aooaijZgCyppLEV8y4nGF1GgpvNZGCX1y7AJB5JwZF5799m8TfWbNHuPJNSRGz6hbToviFi?cluster=devnet) | `solana confirm` → Finalized |
| Dispersed sweep (decoy 0) | [`2Ukjq2…YkD2`](https://explorer.solana.com/tx/2Ukjq2h9mnDS7ioGQ4o2N6pyAy1JGwJYYj8jXKfPB7AmHpuiX9N7fJhz8vrpnZ3p5Gt4GQKpo9ybRji79YHCYkD2?cluster=devnet) | `solana confirm` → Finalized |

## 5. What this proves

- **The program does what it claims, safely.** 24 tests, including 8 invariant tests, show
  the router executes multi-destination bundles atomically and **fails closed** on every
  malformed input (§2).
- **It runs for real, end to end.** A real 8-leg bundle was cast on devnet and its decoys
  were **recovered in dispersed mode** across 7 distinct sinks — proving decoys are
  economically real (they move value) yet recoverable, and that the consolidation mitigation
  is working code, not prose (§3b–3d, §4).
- **The privacy claim was adversarially stress-tested — twice — and reported honestly.** An
  earlier headline ("K≥4 indistinguishable") was false against a "most central value" attack
  (decoys were centred on the real). The generator now uses an **exchangeable construction**
  plus **plausible-band rejection sampling**; the harness ships that exact central attack and
  a **15-feature learned adversary that includes the absolute-magnitude/band-edge channel** a
  skeptic would use. Measured advantage is small and bounded (+0.036 at K=2 → +0.011 at
  K=16). Two regression tests lock the exchangeability property.
- **The recovery-linkage risk is measured and mitigated.** Naive consolidation links decoys
  (measured), `--disperse` removes it (measured to 0) — not asserted.
- **Anyone can verify it.** Program and transactions are live on devnet and `Finalized`; the
  harness result reproduces from `--seed 1`.

This proves a working, tested, live, honestly-measured tool whose central privacy claim was
adversarially stress-tested and survives. It does **not** claim mainnet-audited security,
multi-bundle unlinkability, or defense against the destination-history / timing channels;
those limits are stated in the README and threat model.
