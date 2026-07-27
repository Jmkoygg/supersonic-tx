# Security

`supersonic-tx` is devnet-validated software from a Superteam Brasil bounty submission,
not an audited production system. Read [`THREAT_MODEL.md`](./THREAT_MODEL.md) for the
adversary model and [`AUDIT.md`](./AUDIT.md) for the full independent audit that has run
against this code so far (`solanabr/auditor-skill`, seven rounds — three informal plus
four formal rounds; rounds 5–6 ran clean back to back and closed the cycle once, and
round 7 found new real issues in newly-added code and fixed them within the same round,
honestly reopening that closing counter rather than claiming a closure that no longer
holds — six real findings fixed at or above the disclosure bar, including the
local-storage plaintext issue below). `PROOF.md §3g` carries a short summary of the same
audit with a link to the full account.

## Static analysis tooling

In addition to Sec3 X-Ray (Solana-specific, on-chain program only) and `cargo audit`
(dependency advisories) — see [`README.md`](./README.md) — this project also runs
Semgrep (`p/rust` ruleset) against all of its own Rust code (`programs/`, `sdk/`, `cli/`,
`harness/`, `bench/pinocchio-router/`, third-party dependencies excluded). Versioned
report and SARIF output: [`security/semgrep-report.md`](./security/semgrep-report.md),
[`security/semgrep-report.sarif`](./security/semgrep-report.sarif). Current result: 4
findings, all reviewed and confirmed to be rule false positives for this codebase (see
the report for the per-finding rationale) — no code change required.

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
notably that the funding-graph channel is mitigated against a same-hop fee-payer
correlation attacker, but not closed against a stronger multi-hop one, by anything
shipped here — are known, not vulnerabilities to report. See those documents before
filing.

## Fixed findings

- **`supersonic warm`'s sweep-back transaction used the wrong fee-payer** (the pool-slot
  keypair itself), which is mathematically guaranteed to fail — a transaction's fee is
  debited from the fee-payer before the instruction executes, so every real run would
  have stranded ~0.005 SOL per slot with no CLI command exposed to recover it. Found by
  `solanabr/auditor-skill` round 1. **Fixed:** the pool's real `payer` (later, its
  per-slot sub-funder — see below) pays the fee; the slot only co-signs to authorize
  moving its own balance. Verified:
  `cli/tests/warm_sweep_fix_verification.rs` (a permanent regression test).
- **Docs overclaimed that `--decoy-mode warm-pool` closes the token-holdings and
  funding-graph channels together**, when at the time only token-holdings (and
  destination-history) were actually closed by the shipped mechanism. Found by
  `solanabr/auditor-skill` round 1. **Fixed:** corrected before it shipped publicly; the
  funding-graph channel's real, honest status (mitigated against a same-hop attacker as
  of round 7, not fully closed against a multi-hop one) is tracked accurately in
  `THREAT_MODEL.md §6` and measured in `PROOF.md §3f`.
- **Local bundle records were stored as plaintext JSON** (`~/.supersonic/bundles.json`),
  including `real_index` (which leg of each sent bundle was real) and the real
  destination — exactly what the rest of this tool exists to hide, readable by anything
  with local file access (shared machine, cloud backup sync, forensic image, malware),
  with no on-chain adversary needed at all. Found by `solanabr/auditor-skill` round 3.
  **Fixed:** every record is now encrypted at rest (ChaCha20-Poly1305, key derived from
  the wallet, same trust boundary as the recovery secret itself; random nonce per
  record) before it touches disk, and the store file/directory permissions are
  restricted to the owner (`0600`/`0700` on Unix) as defense in depth. Verified: a
  dedicated test (`cli/src/main.rs :: tests::local_record_is_encrypted_and_round_trips`)
  asserts the real destination and `real_index` do not appear as plaintext bytes
  anywhere in what's written to disk, that a different wallet's derived key cannot
  decrypt another wallet's records, and that nonces never repeat under the same key.
- **`resolve_recover_mode` silently assumed `DecoyMode::Fresh` when a bundle's local
  record was missing or couldn't be decrypted** — for bundles sent with
  `--decoy-mode warm-pool`, this derived the *wrong* decoy addresses on recovery and
  reported "nothing to recover" instead of erroring, a false negative that contradicts
  this tool's own claim that the same wallet that sends a bundle can always recover its
  own decoys. Found by `solanabr/auditor-skill` round 4 (F-001, severity 6).
  **Fixed:** `resolve_recover_mode` now returns a `Result` and fails loud, asking the
  caller for an explicit `--decoy-mode` override, instead of silently defaulting. Verified:
  `cli/src/main.rs :: tests::recover_mode_errors_when_record_missing_or_undecryptable_and_no_override`,
  and independently re-verified against the actual fix code (not just re-read) in round 5
  (see [`AUDIT.md`](./AUDIT.md)).
- **The local bundle-store's `bundle_id` counter was a non-atomic read-modify-write**
  with no file locking, so two concurrent `supersonic send` invocations could collide on
  the same `bundle_id` and produce a byte-identical decoy destination set across two
  distinct bundles — a linkability leak — while silently overwriting one bundle's local
  record. Found by `solanabr/auditor-skill` round 4 (F-003, severity 4). **Fixed:** the
  whole load-modify-save cycle is now serialized behind an RAII exclusive file lock
  (`with_store_lock`, via `fd-lock`). Verified:
  `cli/src/main.rs :: tests::next_bundle_id_unique_under_concurrent_callers`, and
  independently re-verified in round 5.
- **`send --decoy-mode warm-pool` derived decoys from pool slots without checking they'd
  ever actually been warmed** — a pool that was never run through `supersonic warm` (or
  warmed below the slots this bundle selects) would silently produce decoys with no real
  on-chain history, leaking exactly the destination-history tell the warm-pool mechanism
  exists to close. Found by an independent live-review pass against devnet (not an
  `auditor-skill` round). **Fixed:** `ensure_warm_pool_slots_are_warmed` now queries
  `getSignaturesForAddress` for every selected slot before `send` proceeds, and fails
  closed with an actionable error (naming the cold slot, its pubkey, and the exact
  `supersonic warm` command to fix it) instead of leaking silently. The check runs at
  `send` time, not `plan` time, so `plan` stays fully offline. Verified live against
  devnet (a cold 47-slot pool was rejected before spending anything; the same pool
  succeeded immediately after warming) and by
  `cli/src/main.rs :: tests::check_warm_pool_slots_have_history_fails_closed_on_a_single_cold_slot`.
- **`inspect` (no `--bundle-id` filter) aborted the entire listing if a single local
  record was corrupted or undecryptable**, hiding every other bundle's record along with
  it. Found by the same live-review pass. **Fixed:** a per-record decrypt failure now
  prints a `[warn] ... skipping` line and the listing continues; the `--bundle-id <id>`
  single-record path is unchanged. Verified:
  `cli/src/main.rs :: tests::list_all_lines_skips_corrupted_record_instead_of_aborting`
  (also asserts the corrupted record's plaintext fields never leak into the warning).
- **`resolve_recover_mode` still silently defaulted `--pool-size` to `32` when
  `--decoy-mode warm-pool` was passed explicitly without it** — the same failure shape as
  the F-001 fix above, one parameter over: `pool_size` directly changes the range
  `select_pool_slots` shuffles over on every draw, so a wrong guess derives a
  structurally different slot sequence, not a subset, silently reproducing the "nothing
  to recover" false-success F-001 exists to prevent for any bundle sent with a
  non-default `--pool-size`. Found by `solanabr/auditor-skill` round 7 (F-1, severity 5).
  **Fixed:** the same fail-loud pattern as F-001, scoped to `WarmPool` specifically.
  Verified: `cli/src/main.rs :: tests::recover_mode_errors_on_warm_pool_override_without_pool_size`.
- **`warm_pool`'s only sanity cap bounded round-trip cost, which is zero whenever
  `--rounds 0` regardless of `--pool-size`** — leaving the flat per-slot costs paid
  independent of `rounds` (ATA-creation rent, and the sub-funder seeding transfer this
  round's funding-graph mitigation added) with no bound at all, so an arbitrarily large
  `--pool-size 0` would pass the existing check and spend on every slot with no warning.
  Found during round 7's scoped diff-audit pass, before the full re-walk began. **Fixed:**
  an independent `pool_size_exceeds_cap` check (cap: 1,000 slots) now runs first.
  Verified: `cli/src/main.rs :: tests::pool_size_exceeds_cap_rejects_above_and_allows_at_or_below`.

## Known, stated hardening gaps (not fixed, not hidden)

- `master_seed` (the root recovery secret) and the 32-byte intermediate KDF buffers
  derived from it (e.g. the RNG/cipher/keypair seeds computed inside `bundle_rng`,
  `derive_decoy_keypair`, `derive_sink_keypair`, `derive_pool_member_keypair`,
  `select_pool_slots`, and `local_store_cipher`) are now zeroized from process memory
  via the `zeroize` crate when they go out of scope — `master_seed` itself is held as
  `zeroize::Zeroizing<[u8; 32]>` in `cli/src/main.rs::main`, wiped even on an early `?`
  return. Individual derived `Keypair`s (decoys, sinks, pool members) **are** zeroized on
  drop: `solana_sdk::signature::Keypair` (resolved here via `solana-keypair` v2.2.1) is
  `pub struct Keypair(ed25519_dalek::Keypair)`, a thin wrapper with no custom `Drop`;
  `ed25519_dalek::Keypair` holds `pub secret: SecretKey`; and `SecretKey` is declared
  `#[derive(Zeroize)] #[zeroize(drop)] pub struct SecretKey([u8; 32])` in `ed25519-dalek`
  v1.0.1 (the version resolved transitively via `solana-sdk`/`solana-keypair` in this
  workspace's `Cargo.lock`) — confirmed by reading
  `ed25519-dalek-1.0.1/src/secret.rs` (lines 32–45) and its `Cargo.toml`, which enables
  the `zeroize_derive` feature that makes `#[zeroize(drop)]` generate a real `Drop` impl
  overwriting the secret bytes, not just carry `zeroize` in the dependency graph for an
  unrelated reason. Upstream ships its own regression test for this
  (`ed25519-dalek-1.0.1/src/secret.rs::test::secret_key_zeroize_on_drop`), which reads
  the memory behind a dropped `SecretKey` and asserts the original bytes are gone.
  Because Rust's default drop glue recursively drops struct fields with no custom `Drop`
  needed at the outer levels, this chain holds all the way up: dropping a
  `solana_sdk::signature::Keypair` zeroizes its secret key bytes. This was previously
  stated here as an open gap ("`solana_sdk::signature::Keypair` is an external type with
  no `Zeroize` support") — that was incorrect; it has been corrected above. As a
  low-risk, purely local hardening on top of this (not a fix for a gap, since there
  isn't one here anymore): `cli/src/main.rs::recover()` now explicitly `drop()`s its
  decoy `Keypair`s right after signing the sweep transaction instead of at the end of
  the function, so the zeroization above happens before the `send_and_confirm_transaction_with_spinner`
  network round-trip rather than after it.
- `supersonic warm` does not print a cost estimate (SOL + ATA rent + fees) before
  spending, for runs under the existing sanity cap. Below the cap, nothing stops a
  larger-than-intended run from a typo.
- Associated Token Account rent opened by `supersonic warm` (~0.00204 SOL/slot) is not
  reclaimable via any CLI command today.
- The deployed devnet program's bytecode has not been verified byte-for-byte against a
  reproducible build: rebuilding from this source locally produces a `.so` of the
  identical size (183,088 bytes) but a different hash than `solana program dump` returns
  for the live deployment — consistent with known SBF toolchain build
  non-determinism (embedded build metadata), not confirmed evidence of a source
  mismatch, but not proven identical either. A `solana-verify`/Ellipsis-Labs-style
  verified build is the correct next step and hasn't been run.
- The devnet program (`BCrR3JKi5EWhC5DuKYzV4EX7ogawoWaoKkhSqZYeYabn`) has a single-wallet
  upgrade authority (`25NhgSgz97LKxPbD8usUcnH7AcUbuV6P2Qmc3Ayhbaee`), not a multisig
  (`solana program show BCrR3JKi5EWhC5DuKYzV4EX7ogawoWaoKkhSqZYeYabn --url devnet`).
  Acceptable at this stage — devnet, no real funds at risk — but a single key that can
  push a new implementation to this program id is a single point of failure and a
  potential backdoor vector. Rotating to a multisig (e.g. Squads) upgrade authority, or
  revoking upgradeability entirely, is a blocking requirement before any mainnet deploy,
  not an optional hardening step.
- The Pinocchio bench program's own devnet deployment
  (`3cKHNQ4YyfkEnc3YuJjSdrFAGCketGqTUobWy6gxaoLP`, `BENCHMARK.md` Result 4) has the
  **same** single-wallet upgrade authority as the Anchor program above
  (`25NhgSgz97LKxPbD8usUcnH7AcUbuV6P2Qmc3Ayhbaee` — confirmed via
  `solana program show 3cKHNQ4YyfkEnc3YuJjSdrFAGCketGqTUobWy6gxaoLP --url devnet`), which
  doubles that single key's blast radius (compromise now lets an attacker push malicious
  bytecode to two deployed programs, not one) rather than introducing a new class of
  risk. Same acceptance and same blocking requirement as above: fine for a devnet
  functional proof, not for any mainnet deploy of either program under this key.
