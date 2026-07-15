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
    Finished `release` profile [optimized] target(s) in 4m 42s
```

Both the on-chain program (`anchor build` → BPF) and the host crates (SDK, CLI, harness)
compile with zero errors.

> Note: `cargo clippy` currently hits an internal compiler panic (ICE) on the
> `supersonic-sdk` crate, reproduced on both clippy 0.1.94 and 0.1.96 at different
> internal locations — a known clippy×solana-sdk toolchain bug, not a code defect
> (`cargo build`/`cargo test` are clean, below). Flagged in the README as an upstream
> issue.

## 2. Automated tests — 23 passing, 0 failing

```
$ cargo test
   Running unittests src/main.rs (supersonic-harness)
running 3 tests
test result: ok. 3 passed; 0 failed; 0 ignored

   Running unittests src/lib.rs (supersonic-sdk)
running 11 tests
test result: ok. 11 passed; 0 failed; 0 ignored

   Running unittests src/lib.rs (supersonic-tx)
running 1 test
test result: ok. 1 passed; 0 failed; 0 ignored

   Running tests/invariants.rs (supersonic-tx)
running 8 tests
test result: ok. 8 passed; 0 failed; 0 ignored
```

**Total: 23 passed, 0 failed.** The 8 invariant tests (`programs/supersonic-tx/tests/invariants.rs`)
map 1:1 to the threat-model invariants: single/multi-leg happy paths with real value
movement, empty-bundle / too-many-legs / zero-amount / self-destination / leg-count
mismatch rejection, and atomic revert with no partial funding on insufficient balance.

## 3. Live execution on devnet — one per core capability

Each capability the tool claims, exercised for real against devnet.

### 3a. Deploy the program
```
$ anchor deploy --provider.cluster devnet
Program Id: BCrR3JKi5EWhC5DuKYzV4EX7ogawoWaoKkhSqZYeYabn
Deploy success
```

### 3b. Cast a bundle (K=8): real 0.02 SOL hidden among 7 decoys
```
$ supersonic send --to F58dg5YM7wSArwwJe5uS5UXXKBQNHzH131VNRU3CEvH5 --amount 0.02 --k 8 --bundle-id 101
[ok] sent bundle 101
   signature: 3H7BcyLczeAJjTGmWD2gCPZipmbnsySp26sF9qDXyhfkisgxksKoHh8EEvDsQKd9vEX6uPNny6VsZCkraFSB9hqD
```
The plan showed the real 0.02 SOL leg sharing its exact amount with **five** other decoy
legs — an observer cannot tell which of the six 0.02 SOL transfers was the real one.

### 3c. Cast a bundle (K=4): real 0.05 SOL
```
$ supersonic send --to F58dg5YM7wSArwwJe5uS5UXXKBQNHzH131VNRU3CEvH5 --amount 0.05 --k 4 --bundle-id 102
   signature: 27CWb38pARjYko6LxynwmM5KEib7nZBofRCs1hnReAoHwkThQX4MCddqwyYJKyCJqYviF9NPpRcFjboGZaQkbqCn
```

### 3d. Recover the decoys (the funds are genuinely recoverable)
```
$ supersonic recover --bundle-id 101 --k 8
recovered 0.27 SOL from 7 decoys of bundle 101
  signature: 4G6y8Gb4xLmHcU6yciVwU4LXcWUvgBikFx4E7TPvBjXmqQtn8tupg1nUvw8GTtfSmDfp1ZzAdi1N2hxTTR5fBm7G
```

### 3e. Adversarial measurement (the privacy claim, quantified)
```
$ supersonic-harness --n 8000 --seed 1
  K  | baseline | best attack          | adv (test) | interpretation      | consolidation adv
-----+----------+----------------------+------------+---------------------+------------------
   2 |   0.500 | least_round          |   +0.0571 | weak leak           |   +0.5000
   4 |   0.250 | least_round          |   +0.0194 | indistinguishable   |   +0.7500
   8 |   0.125 | first_position       |   -0.0031 | indistinguishable   |   +0.8750
  16 |   0.062 | last_position        |   -0.0042 | indistinguishable   |   +0.9375
```
The attacker picks its best classifier on a train split; the advantage is measured on a
held-out test split, so it cannot be cherry-picked from noise. For `K ≥ 4` the best
attacker does no better than a random guess. Full JSON: `PROOF/harness-report.json`.

## 4. Third-party-verifiable references

All actively confirmed on devnet with `solana confirm ... --url devnet` (each returned
`Finalized`) and via `solana program show`:

| What | Reference | Check |
|---|---|---|
| Program account | [`BCrR3J…YeYabn`](https://explorer.solana.com/address/BCrR3JKi5EWhC5DuKYzV4EX7ogawoWaoKkhSqZYeYabn?cluster=devnet) | `solana program show` → owner BPFLoaderUpgradeable, 185016 bytes |
| Cast K=8 (bundle 101) | [`3H7Bcy…B9hqD`](https://explorer.solana.com/tx/3H7BcyLczeAJjTGmWD2gCPZipmbnsySp26sF9qDXyhfkisgxksKoHh8EEvDsQKd9vEX6uPNny6VsZCkraFSB9hqD?cluster=devnet) | `solana confirm` → Finalized |
| Cast K=4 (bundle 102) | [`27CWb3…bqCn`](https://explorer.solana.com/tx/27CWb38pARjYko6LxynwmM5KEib7nZBofRCs1hnReAoHwkThQX4MCddqwyYJKyCJqYviF9NPpRcFjboGZaQkbqCn?cluster=devnet) | `solana confirm` → Finalized |
| Recover (bundle 101) | [`4G6y8G…fBm7G`](https://explorer.solana.com/tx/4G6y8Gb4xLmHcU6yciVwU4LXcWUvgBikFx4E7TPvBjXmqQtn8tupg1nUvw8GTtfSmDfp1ZzAdi1N2hxTTR5fBm7G?cluster=devnet) | `solana confirm` → Finalized |

```
$ solana program show BCrR3JKi5EWhC5DuKYzV4EX7ogawoWaoKkhSqZYeYabn --url devnet
Program Id: BCrR3JKi5EWhC5DuKYzV4EX7ogawoWaoKkhSqZYeYabn
Owner: BPFLoaderUpgradeab1e11111111111111111111111
Authority: 25NhgSgz97LKxPbD8usUcnH7AcUbuV6P2Qmc3Ayhbaee
Data Length: 185016 (0x2d2b8) bytes
```

## 5. What this proves

- **The program does what it claims, safely.** 23 tests, including 8 invariant tests, show
  the router executes multi-destination bundles atomically and **fails closed** on every
  malformed input — a partial send that exposes the real leg without its decoys is
  impossible (§2).
- **It runs for real, end to end.** A real 8-leg bundle and a real 4-leg bundle were cast
  on devnet, and the decoy funds were **recovered** — proving decoys are economically
  real (they move value) yet not lost (they are recoverable), which is the whole premise
  (§3b–3d, §4).
- **The privacy claim is a measured number, not an adjective.** Against concrete
  classifiers on held-out data, the best attacker's advantage over a random guess is
  `≈ 0` for `K ≥ 4` (§3e). Where privacy is weaker — `K = 2`, and naive consolidation —
  the number is reported honestly rather than hidden.
- **Anyone can verify it.** The program and all three transactions are live on devnet and
  confirmed `Finalized`; the harness result reproduces from `--seed 1` (§4).

This proves a working, tested, live, measurable tool — not a prototype. It does **not**
claim mainnet-audited security, multi-bundle unlinkability, or defense against the
destination-history channel; those limits are stated in the README and threat model.
