# Security

`supersonic-tx` is devnet-validated software from a Superteam Brasil bounty submission,
not an audited production system. Read [`THREAT_MODEL.md`](./THREAT_MODEL.md) for the
adversary model and [`PROOF.md §3g`](./PROOF.md) for the independent audit that has run
against this code so far (`solanabr/auditor-skill`, two rounds, two real findings fixed).

## Reporting a vulnerability

If you find a security issue, please report it privately rather than opening a public
issue: open a GitHub security advisory on this repository, or reach the maintainer
listed on the PR this repository was submitted through. Include the exact command/input
that reproduces the issue, same as the reproducibility standard the rest of this repo
holds itself to (`PROOF.md`).

## What's in scope

- The on-chain program (`programs/supersonic-tx`, and the Pinocchio alternative in
  `bench/pinocchio-router`) — fund-safety issues (invariants I1–I4, `THREAT_MODEL.md §5`).
- The SDK's decoy/recovery derivation (`sdk/src/`) — anything that would make a decoy
  unrecoverable, or that would let someone other than the seed holder derive it.
- The CLI's transaction-building and fund-handling logic (`cli/src/main.rs`) — the class
  of bug already found and fixed once here (`PROOF.md §3g`, the `warm` fee-payer issue).

## What's explicitly out of scope (already known, already stated)

The channels and limitations documented in `THREAT_MODEL.md §6` and `§4.7` — most
notably that the funding-graph channel is not closed by anything shipped here — are
known, not vulnerabilities to report. See those documents before filing.

## Known, stated hardening gaps (not fixed, not hidden)

- `master_seed` and derived `Keypair`s are held in process memory without explicit
  zeroization (no `zeroize` crate) — a process memory dump or swap could expose them.
  Common gap for a CLI tool at this stage; flagged, not yet addressed.
- `supersonic warm` does not print a cost estimate (SOL + ATA rent + fees) before
  spending, for runs under the existing sanity cap. Below the cap, nothing stops a
  larger-than-intended run from a typo.
- Associated Token Account rent opened by `supersonic warm` (~0.00204 SOL/slot) is not
  reclaimable via any CLI command today.
- The deployed devnet program's bytecode has not been verified byte-for-byte against a
  reproducible build: rebuilding from this source locally produces a `.so` of the
  identical size (182,296 bytes) but a different hash than `solana program dump` returns
  for the live deployment — consistent with known SBF toolchain build
  non-determinism (embedded build metadata), not confirmed evidence of a source
  mismatch, but not proven identical either. A `solana-verify`/Ellipsis-Labs-style
  verified build is the correct next step and hasn't been run.
