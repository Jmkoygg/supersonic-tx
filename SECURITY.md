# Security

`supersonic-tx` is devnet-validated software from a Superteam Brasil bounty submission,
not an audited production system. Read [`THREAT_MODEL.md`](./THREAT_MODEL.md) for the
adversary model and [`AUDIT.md`](./AUDIT.md) for the full independent audit that has run
against this code so far (`solanabr/auditor-skill`, six rounds — three informal plus
three formal rounds, the last two clean and closing the audit cycle — five real findings
fixed, including the local-storage plaintext issue below). `PROOF.md §3g` carries a short
summary of the same audit with a link to the full account.

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

## Fixed findings

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
