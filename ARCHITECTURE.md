# supersonic-tx — Architecture

This document explains *how* the pieces fit and *why* the boundaries are where they
are. For the security reasoning (adversaries, what's observable, the metric, the
invariants) read [`THREAT_MODEL.md`](./THREAT_MODEL.md) first — the architecture is
downstream of it.

## The one-sentence design

An observer of a Solana transaction sees the net movement of value; so to hide *which*
transfer a user actually meant, every transfer in a bundle must move real value and be
statistically indistinguishable from the real one — and that ambiguity is manufactured
off-chain (the SDK) while atomicity and fail-closed safety are enforced on-chain (the
program).

```
        ┌──────────────────────────────────────────────────────────────┐
        │  supersonic-cli   (bin: `supersonic`)                         │
        │  plan / send / recover / inspect · wallet-derived recovery    │
        └───────────────┬──────────────────────────────┬───────────────┘
                        │ uses                          │ sends tx / sweeps
        ┌───────────────▼───────────────┐              ▼
        │  supersonic-sdk  (lib)        │        Solana RPC (devnet)
        │  • decoy amount generation    │              │
        │    (log-space + roundness)    │              │  execute_bundle
        │  • recoverable decoy dests    │        ┌─────▼──────────────────┐
        │  • bundle → instruction       │        │ supersonic-tx (program)│
        └───────────────┬───────────────┘        │ atomic multi-transfer  │
                        │ measured by             │ I1–I4 enforced         │
        ┌───────────────▼───────────────┐        └────────────────────────┘
        │ supersonic-harness (bin)      │
        │ adversarial classifiers →     │
        │ advantage over 1/K baseline   │
        └───────────────────────────────┘
```

## Components

### 1. `programs/supersonic-tx` — the on-chain router (Anchor)

A single global program with one instruction, `execute_bundle(legs: Vec<Leg>)`. Each
`Leg` is just an `amount`; destinations ride in `remaining_accounts`, one per leg, in
order. The program:

- moves `amount` lamports from the signer to each destination, all in one transaction;
- is **oblivious** to which leg is real — every leg takes the identical code path;
- enforces the testable invariants (atomicity, fail-closed, bounds, 1:1 leg/dest,
  no zero legs, no self-sends) and **nothing else**. It deliberately does not inspect
  destinations, because any on-chain "is this the user's own?" check would brand decoys
  and leak exactly what we hide.

Why a program at all, if it's "just transfers"? Because it gives the ecosystem a stable
program id to **route through** (the composability the bounty asks for), and it makes
all-or-nothing atomicity a property the caller cannot get wrong — a partial send that
landed the real leg without its decoys would defeat the whole point.

Why Anchor (not Pinocchio/native)? The composability requirement is best served by a
published IDL that third parties can generate clients from, and the program's real
complexity is instruction/CPI shape, not compute-unit micro-optimization. The tradeoff
is recorded in `THREAT_MODEL`/README honesty sections.

### 2. `sdk/` — `supersonic-sdk` (the privacy engineering)

This is where the noise is made *believable*. Given a real intent (`dest`, `amount`)
and an anonymity-set size `K`, it produces an ordered bundle of 1 real + K−1 decoy legs:

- **Amounts** (`amounts.rs`): decoys are drawn log-normally around the real amount so
  the real value is not a positional outlier, then **roundness-matched** so a round real
  (e.g. `1.0 SOL`) is hidden among equally-round decoys instead of standing out against
  jittered ones.
- **Destinations**: decoy destinations are derived deterministically from a master seed
  via a domain-separated KDF, so they are **recoverable** by — and only by — the holder
  of the seed. The real destination is user-supplied and never derived. Two sourcing
  modes (`DecoyMode`): `Fresh` (default — a brand-new keypair per bundle, zero prior
  history) and `WarmPool` (`warming.rs` — drawn from a pool of addresses the user
  pre-warms with real signature history and a real token account via `supersonic warm`,
  a distinct bundle-seeded random subset per bundle rather than a fixed prefix).
- **Placement**: the real leg is inserted at a seed-determined random index.
- **Encoding**: builds the exact Anchor instruction (8-byte discriminator + Borsh
  `Vec<Leg>`) and account list, so the SDK needs no dependency on the program crate.

Everything is deterministic in `(master_seed, bundle_id)`, which is what makes decoys
recoverable and the whole system reproducible and testable.

### 3. `cli/` — `supersonic` (the usable tool)

Thin wrapper over the SDK + RPC: `plan` (dry run), `send` (cast to devnet), `recover`
(sweep decoys, auto-detecting which `DecoyMode` a past bundle used), `inspect` (local
records), `warm` (build real signature history + a real token account on warm-pool
slots ahead of use). The master recovery secret is derived from the wallet key
(`sha256(tag ‖ keypair)`), so there is no extra secret to manage and the same wallet
that cast a bundle can always recover it. `recover` prints the honest
consolidation-linkage warning from `THREAT_MODEL §4.6`; `warm` prints the honest
funding-graph-not-closed warning from `THREAT_MODEL §4.7`.

### 4. `harness/` — `supersonic-harness` (the proof)

The reason any privacy claim here is a number and not an adjective. It generates many
bundles with the real SDK, then runs concrete adversary classifiers (max/min amount,
roundest/least-round, log-median outlier, fixed position) and measures
`advantage = accuracy − 1/K` on a held-out test split (the adversary picks its best
attack on train, so the number can't be cherry-picked from noise). It also reports the
honest naive-consolidation worst case, and — via `mainnet_channel.rs` /
`mainnet_fixture.rs` — destination-history, funding-graph, and token-holdings measured
against 377 real, passively-observed mainnet-beta addresses, evaluated only on a
`held_out` partition decided at collection time (`calibration` is reserved for whatever
mechanism fits something to the data). Output is a table plus a machine-readable JSON
that `PROOF.md` cites. `harness/tests/pinocchio_invariants.rs` additionally proves the
program's 8 core invariants hold against `bench/pinocchio-router` too, via Mollusk.

## Composability (how other tools "cast through" this)

- **Program interface:** the published Anchor IDL for `execute_bundle` lets any program
  or client construct a bundle without reading this source.
- **SDK surface:** `plan_bundle`, `build_instruction`, and `derive_decoy_keypair` are
  the integration points. Proven externally, not just in principle:
  [PR #3](https://github.com/solanabr/supersonic-tx/pull/3) is a separate contributor's
  `account-cooker` work routing a real devnet transaction through this router using only
  the public SDK surface.
- **Warm-pool decoys** (`warming.rs`) are this project's own answer to "host decoy
  destinations so they stay live" — the user's own SDK-managed pool, not a dependency on
  an external tool. It closes destination-history and token-holdings; it does not
  diversify who funds the pool, so funding-graph remains open regardless (`THREAT_MODEL
  §4.7`) — an external, mature multi-party account-cooker/mirror-pool is still what would
  close that specific residual.

## What lives where (invariant → enforcement)

| Concern | Enforced by | Tested in |
|---|---|---|
| Atomic all-or-nothing (I3) | program | `tests/invariants.rs` |
| Fail-closed on malformed input (I4) | program | `tests/invariants.rs` |
| Non-custodial (I1) | program — structural: no pool/escrow/PDA in `ExecuteBundle` | structural (no vault account exists to hold funds) |
| No third-party leak from decoys (I2) | SDK construction (recoverable dests) | SDK unit tests |
| Amount indistinguishability | SDK generation | harness (measured) |
| Consolidation linkage (open) | operational (delay/disperse) | harness (worst case reported) |
