//! `supersonic` — CLI for casting intent-ambiguous transfer bundles and recovering
//! decoy funds.
//!
//! The master recovery secret is derived from your wallet key, so no extra secret
//! to manage: the same wallet that sends a bundle can always recover its decoys.
//! Per-bundle metadata is stored locally under `~/.supersonic/bundles.json` so you
//! can inspect and audit what you sent — **encrypted** (ChaCha20-Poly1305, keyed by
//! a hash of your wallet, same trust boundary as the recovery secret itself). This
//! file otherwise contains exactly what the whole rest of this tool exists to hide
//! per bundle (`real_index`, the real destination) — SECURITY.md documents this as
//! a fixed gap, not left silently in cleartext.

mod warm_profile;

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{anyhow, Context, Result};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Nonce,
};
use clap::{Parser, Subcommand};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use solana_client::rpc_client::{GetConfirmedSignaturesForAddress2Config, RpcClient};
use solana_sdk::{
    commitment_config::CommitmentConfig,
    instruction::{AccountMeta, Instruction},
    native_token::{lamports_to_sol, sol_to_lamports},
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair},
    signer::Signer,
    system_instruction, system_program,
    transaction::Transaction,
};
use supersonic_sdk::{
    build_instruction, derive_decoy_keypair, derive_sink_keypair, plan_bundle_with_mode,
    warming::{derive_pool_member_keypair, select_pool_slots},
    BundlePlan, DecoyConfig, DecoyMode,
};
use zeroize::{Zeroize, Zeroizing};

/// The deployed router program id (matches `declare_id!` in the program).
const DEFAULT_PROGRAM_ID: &str = "BCrR3JKi5EWhC5DuKYzV4EX7ogawoWaoKkhSqZYeYabn";
const DEFAULT_RPC: &str = "https://api.devnet.solana.com";

// Well-known program/mint ids (same on devnet and mainnet — these are fixed
// system-level constants, not something anyone deploys).
const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const ASSOCIATED_TOKEN_PROGRAM_ID: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
const NATIVE_MINT: &str = "So11111111111111111111111111111111111111112"; // wSOL

/// Derive the associated token account address for `(owner, mint)` under the
/// standard SPL Token program, without pulling in the `spl-associated-token-
/// account` crate (which would drag a different solana-program generation, the
/// same cross-generation friction Mollusk needed pinning for elsewhere in this
/// workspace — see `harness/tests/mollusk_cu_bench.rs`).
fn derive_ata(
    owner: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
    ata_program: &Pubkey,
) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), token_program.as_ref(), mint.as_ref()],
        ata_program,
    )
    .0
}

/// The Associated Token Account program's `CreateIdempotent` instruction (data
/// `[1]`) — creates the ATA if absent, succeeds as a no-op if it already
/// exists, so warming the same pool twice doesn't fail on the second run.
fn create_ata_idempotent_ix(
    funding_account: &Pubkey,
    owner: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
    ata_program: &Pubkey,
) -> Instruction {
    let ata = derive_ata(owner, mint, token_program, ata_program);
    Instruction {
        program_id: *ata_program,
        accounts: vec![
            AccountMeta::new(*funding_account, true),
            AccountMeta::new(ata, false),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(system_program::ID, false),
            AccountMeta::new_readonly(*token_program, false),
        ],
        data: vec![1u8], // CreateIdempotent
    }
}
const MASTER_TAG: &[u8] = b"supersonic-tx/master/v1";

#[derive(Parser)]
#[command(
    name = "supersonic",
    about = "Cast fuzzy transfer bundles to obscure intent"
)]
struct Cli {
    /// Path to the signing keypair (defaults to the Solana CLI default).
    #[arg(long, global = true)]
    keypair: Option<PathBuf>,
    /// RPC endpoint.
    #[arg(long, global = true, default_value = DEFAULT_RPC)]
    rpc: String,
    /// Router program id.
    #[arg(long, global = true, default_value = DEFAULT_PROGRAM_ID)]
    program_id: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show the bundle that would be cast, without sending anything.
    Plan(PlanArgs),
    /// Cast a bundle: send the real transfer buried among K-1 recoverable decoys.
    Send(PlanArgs),
    /// Sweep the decoys of a past bundle back.
    Recover {
        #[arg(long)]
        bundle_id: u64,
        /// Anonymity set size K used when the bundle was sent.
        #[arg(long)]
        k: usize,
        /// Disperse: sweep each decoy to its own distinct sink in a separate
        /// transaction (breaks the consolidation star, THREAT_MODEL §4.6) instead of
        /// consolidating them all into your wallet at once.
        #[arg(long)]
        disperse: bool,
        /// Override the decoy mode instead of looking it up from the local
        /// record (`~/.supersonic/bundles.json`) — needed only if recovering
        /// without that local record (e.g. a different machine).
        #[arg(long, value_enum)]
        decoy_mode: Option<DecoyModeArg>,
        #[arg(long)]
        pool_size: Option<u32>,
    },
    /// Show locally-recorded bundles.
    Inspect {
        #[arg(long)]
        bundle_id: Option<u64>,
    },
    /// Build up real signature history on warm-pool decoy slots ahead of time,
    /// so they aren't zero-history the first time they're used as a decoy (the
    /// destination-history channel, THREAT_MODEL.md). Each slot gets a small
    /// real self-funded round-trip transfer, same construction as the
    /// harness's mainnet-history collector methodology, not synthesized.
    Warm {
        /// Number of warm-pool slots to (continue to) warm.
        #[arg(long, default_value_t = 32)]
        pool_size: u32,
        /// Real round-trip transfers per slot this run (each adds 2 signatures
        /// — a fund-in and a sweep-back — to that slot's history).
        #[arg(long, default_value_t = 1)]
        rounds: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
enum DecoyModeArg {
    Fresh,
    WarmPool,
}

impl DecoyModeArg {
    fn to_sdk(self, pool_size: u32) -> DecoyMode {
        match self {
            DecoyModeArg::Fresh => DecoyMode::Fresh,
            DecoyModeArg::WarmPool => DecoyMode::WarmPool { pool_size },
        }
    }
}

#[derive(clap::Args)]
struct PlanArgs {
    /// Real destination (the payment you actually want to make).
    #[arg(long)]
    to: String,
    /// Real amount, in SOL.
    #[arg(long)]
    amount: f64,
    /// Anonymity set size K (total legs incl. the real one), 2..=16.
    #[arg(long, default_value_t = 8)]
    k: usize,
    /// Bundle nonce; defaults to the next counter for this wallet.
    #[arg(long)]
    bundle_id: Option<u64>,
    /// Decoy destination source: `fresh` (default, never-used keypairs) or
    /// `warm-pool` (drawn from a warmed pool — see the `warm` subcommand).
    #[arg(long, value_enum, default_value_t = DecoyModeArg::Fresh)]
    decoy_mode: DecoyModeArg,
    /// Warm-pool size, when `--decoy-mode warm-pool`. Must match what you ran
    /// `warm --pool-size` with.
    #[arg(long, default_value_t = 32)]
    pool_size: u32,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let keypair = load_keypair(&cli.keypair)?;
    let program_id = Pubkey::from_str(&cli.program_id).context("bad program id")?;
    // The root secret: everything this tool can ever recover (all decoys/sinks/
    // pool-members, past and future, for this wallet) derives from this one
    // value. `Zeroizing` wipes it from process memory the moment it goes out of
    // scope at the end of `main` (incl. on early `?` returns), instead of
    // leaving it sitting in memory/swap for the life of the process. `Deref`s to
    // `[u8; 32]`, so `&master_seed` keeps working everywhere a `&[u8; 32]` is
    // expected below — no call-site signature changes needed.
    let master_seed: Zeroizing<[u8; 32]> = Zeroizing::new(derive_master_seed(&keypair));

    match &cli.cmd {
        Cmd::Plan(a) => {
            let (plan, _) = build_plan(&master_seed, a)?;
            print_plan(&plan, a);
        }
        Cmd::Send(a) => {
            let (plan, bundle_id) = build_plan(&master_seed, a)?;
            print_plan(&plan, a);
            let client = rpc(&cli.rpc);
            // Fail-closed check for `--decoy-mode warm-pool` (juiz-cego finding):
            // must happen here, at send time, not inside `build_plan`/`plan` --
            // see `ensure_warm_pool_slots_are_warmed`'s doc comment for why.
            ensure_warm_pool_slots_are_warmed(&client, &plan)?;
            let sig = send_bundle(&client, &keypair, program_id, &plan)?;
            println!("\n[ok] sent bundle {bundle_id}");
            println!("   signature: {sig}");
            println!("   explorer:  https://explorer.solana.com/tx/{sig}?cluster=devnet");
            record_bundle(&master_seed, &plan, a, &sig.to_string())?;
        }
        Cmd::Recover {
            bundle_id,
            k,
            disperse,
            decoy_mode,
            pool_size,
        } => {
            let client = rpc(&cli.rpc);
            let (decoy_mode, pool_size) =
                resolve_recover_mode(&master_seed, *bundle_id, *decoy_mode, *pool_size)?;
            recover(
                &client,
                &keypair,
                &master_seed,
                *bundle_id,
                *k,
                *disperse,
                RecoverSource {
                    decoy_mode,
                    pool_size,
                },
            )?;
        }
        Cmd::Inspect { bundle_id } => inspect(&master_seed, *bundle_id)?,
        Cmd::Warm { pool_size, rounds } => {
            let client = rpc(&cli.rpc);
            warm_pool(&client, &keypair, &master_seed, *pool_size, *rounds)?;
        }
    }
    Ok(())
}

/// Worst-case round-trip estimate for `warm_pool`'s sanity cap, isolated from
/// `RpcClient`/`Keypair` so it's directly unit-testable. `pool_size` and
/// `rounds` are user-controlled CLI args and can be pushed near `u32::MAX`;
/// `saturating_mul` (N-2) makes overflow saturate to `u64::MAX` -- "obviously
/// exceeds the cap" -- instead of wrapping around to a small value that would
/// silently bypass `warm_pool`'s `MAX_TOTAL_ROUND_TRIPS` sanity check.
fn worst_case_round_trips(pool_size: u32, rounds: u32) -> u64 {
    let per_slot_worst_case = (rounds as f64 * 3.0).ceil() as u64;
    (pool_size as u64).saturating_mul(per_slot_worst_case)
}

/// Build up real signature history on `pool_size` warm-pool slots: real
/// fund-in + sweep-back round trips per slot, each adding 2 signatures.
/// `rounds` is the user's budget/average, not a flat per-slot count — the
/// actual number of round trips per slot is jittered against real mainnet
/// calibration data by `warm_profile::target_rounds_for_slot`, so slots don't
/// all end up with the exact same, suspiciously uniform signature count.
/// Same construction as `harness/src/bin/collect_mainnet_profiles.rs`'s aged-
/// address methodology (a real transaction history, not synthesized) — the
/// difference is these slots are the actual addresses `DecoyMode::WarmPool`
/// draws from in production, not a measurement-only fixture.
fn warm_pool(
    client: &RpcClient,
    payer: &Keypair,
    master_seed: &[u8; 32],
    pool_size: u32,
    rounds: u32,
) -> Result<()> {
    const DUST_LAMPORTS: u64 = 5_000_000; // 0.005 SOL, comfortably above rent-exempt minimum
    const MAX_TOTAL_ROUND_TRIPS: u64 = 2_000; // sanity cap: ~pool_size * worst-case rounds, see below
                                              // Per-slot round-trip counts are no longer uniform (`warm_profile::target_rounds_for_slot`
                                              // jitters each slot's count against real mainnet calibration data, up to 3x `rounds`), so
                                              // this cap must budget for the worst case, not the flat `pool_size * rounds` that used to be
                                              // exact. This is now an upper-bound estimate, not the precise total round trips this run will
                                              // actually perform.
    let worst_case_round_trips = worst_case_round_trips(pool_size, rounds);
    if worst_case_round_trips > MAX_TOTAL_ROUND_TRIPS {
        return Err(anyhow!(
            "pool_size * rounds (worst-case per-slot jitter up to 3x) = {worst_case_round_trips} \
             exceeds the sanity cap of {MAX_TOTAL_ROUND_TRIPS} round-trip transactions (~{} SOL \
             touched in the worst case, {worst_case_round_trips} blocking RPC round-trips). \
             Run in smaller batches (lower --pool-size or --rounds per invocation) if you really need more.",
            lamports_to_sol(DUST_LAMPORTS.saturating_mul(worst_case_round_trips))
        ));
    }
    for slot in 0..pool_size {
        let kp = derive_pool_member_keypair(master_seed, slot);
        // Per-slot round-trip target, derived from real mainnet calibration data so
        // every slot doesn't end up with the exact same, suspiciously uniform
        // signature count (see `warm_profile` module doc).
        let slot_rounds = warm_profile::target_rounds_for_slot(master_seed, slot, rounds);
        for r in 0..slot_rounds {
            let bh = client.get_latest_blockhash()?;
            let fund_ix =
                system_instruction::transfer(&payer.pubkey(), &kp.pubkey(), DUST_LAMPORTS);
            let fund_tx =
                Transaction::new_signed_with_payer(&[fund_ix], Some(&payer.pubkey()), &[payer], bh);
            client
                .send_and_confirm_transaction_with_spinner(&fund_tx)
                .with_context(|| format!("fund pool slot {slot} round {r}"))?;

            let bal = client.get_balance(&kp.pubkey())?;
            let bh = client.get_latest_blockhash()?;
            let sweep_ix = system_instruction::transfer(&kp.pubkey(), &payer.pubkey(), bal);
            // `payer` (not `kp`) must be the fee-payer: `kp` only has exactly `bal`
            // lamports, and a transaction's fee is debited from the fee-payer
            // before the instruction runs — if `kp` paid its own fee here, moving
            // its full balance would always fail by exactly the fee amount. Same
            // fee-payer/signer split already used correctly in `recover()` below.
            let sweep_tx = Transaction::new_signed_with_payer(
                &[sweep_ix],
                Some(&payer.pubkey()),
                &[payer, &kp],
                bh,
            );
            client
                .send_and_confirm_transaction_with_spinner(&sweep_tx)
                .with_context(|| format!("sweep pool slot {slot} round {r}"))?;
        }

        // Give the slot a real SPL token account (wSOL's associated token
        // account — a real, on-chain-verifiable token holding, not merely a
        // signature-history number) so the token-holdings channel
        // (`harness/src/mainnet_channel.rs::eval_mainnet_token_holdings`) is
        // actually closed by this shipped mechanism, not just measured under
        // an idealized model. `payer` funds the (small) rent; the account
        // belongs to the pool slot, not `payer`. Idempotent: safe to re-run.
        let token_program: Pubkey = TOKEN_PROGRAM_ID.parse().expect("valid constant");
        let ata_program: Pubkey = ASSOCIATED_TOKEN_PROGRAM_ID.parse().expect("valid constant");
        let mint: Pubkey = NATIVE_MINT.parse().expect("valid constant");
        let create_ata_ix = create_ata_idempotent_ix(
            &payer.pubkey(),
            &kp.pubkey(),
            &mint,
            &token_program,
            &ata_program,
        );
        let bh = client.get_latest_blockhash()?;
        let ata_tx = Transaction::new_signed_with_payer(
            &[create_ata_ix],
            Some(&payer.pubkey()),
            &[payer],
            bh,
        );
        client
            .send_and_confirm_transaction_with_spinner(&ata_tx)
            .with_context(|| format!("create token account for pool slot {slot}"))?;

        println!(
            "  slot {slot}/{pool_size}: {} -> +{} real signatures this run ({slot_rounds} round-trip(s)), \
             1 token account ensured",
            kp.pubkey(),
            slot_rounds * 2
        );
    }
    println!(
        "\nwarmed {pool_size} pool slots (~{rounds} round-trip(s) each on average, 1 token account each). \
         Per-slot round-trip counts are jittered against real mainnet calibration data \
         (`cli/src/warm_profile.rs`) rather than all being exactly {rounds} — {rounds} is the budget \
         you set via --rounds, not the exact count applied to every slot; see the per-slot lines above \
         for what each slot actually got this run. \
         NOTE: funding-graph is NOT closed by this mechanism — every slot is funded by this same \
         wallet, so an attacker correlating bundles by fee-payer would see one funder behind every \
         warm-pool decoy. See THREAT_MODEL.md for the honest scope of what --decoy-mode warm-pool \
         does and does not defend.\n\
         Use --decoy-mode warm-pool --pool-size {pool_size} on `plan`/`send`."
    );
    Ok(())
}

fn build_plan(master_seed: &[u8; 32], a: &PlanArgs) -> Result<(BundlePlan, u64)> {
    let to = Pubkey::from_str(&a.to).context("bad --to pubkey")?;
    let lamports = sol_to_lamports(a.amount);
    if lamports == 0 {
        return Err(anyhow!("amount too small"));
    }
    let bundle_id = match a.bundle_id {
        Some(id) => id,
        None => next_bundle_id()?,
    };
    let plan = plan_bundle_with_mode(
        master_seed,
        bundle_id,
        to,
        lamports,
        a.k,
        DecoyConfig::default(),
        a.decoy_mode.to_sdk(a.pool_size),
    )
    .map_err(|e| anyhow!("plan failed: {e}"))?;
    Ok((plan, bundle_id))
}

fn print_plan(plan: &BundlePlan, a: &PlanArgs) {
    println!(
        "bundle {} — K={} (1 real + {} decoys)",
        plan.bundle_id,
        a.k,
        a.k - 1
    );
    println!("  real intent: {} SOL -> {}", a.amount, a.to);
    println!("  legs (as an observer sees them, real hidden):");
    for (i, leg) in plan.legs.iter().enumerate() {
        println!("    [{i:>2}] {:>14} lamports -> {}", leg.amount, leg.dest);
    }
    let decoy: u64 = plan
        .legs
        .iter()
        .filter(|l| !l.is_real)
        .map(|l| l.amount)
        .sum();
    println!(
        "  principal moved: {} SOL total; {} SOL parked in recoverable decoys",
        lamports_to_sol(plan.total_moved()),
        lamports_to_sol(decoy)
    );
}

fn send_bundle(
    client: &RpcClient,
    payer: &Keypair,
    program_id: Pubkey,
    plan: &BundlePlan,
) -> Result<solana_sdk::signature::Signature> {
    let ix = build_instruction(program_id, payer.pubkey(), plan);
    let bh = client.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[payer], bh);
    client
        .send_and_confirm_transaction_with_spinner(&tx)
        .context("send bundle")
}

/// Fail-closed check (juiz-cego finding, live-devnet round): `--decoy-mode
/// warm-pool` exists to give decoy destinations real on-chain history instead
/// of the zero-history a fresh keypair has (the destination-history channel,
/// THREAT_MODEL.md). Before this check, a plan/send with `--decoy-mode
/// warm-pool` against a pool that was never actually warmed (`supersonic
/// warm`) would silently use zero-history decoys anyway -- exactly the leak
/// warm-pool exists to close, with no warning.
///
/// **Design note: this runs at `send` time, not `plan` time.** `plan` is
/// intentionally a pure, offline operation today (see `build_plan` /
/// `plan_bundle_with_mode` -- neither touches `RpcClient`), and this check
/// needs one `getSignaturesForAddress` RPC round-trip per selected warm-pool
/// slot to confirm real history. Forcing `plan` onto the network just for
/// this one decoy mode would be a bigger behavior change than this bug
/// warrants, and would make `plan`'s offline guarantee mode-dependent instead
/// of universal. `send` already talks to RPC (`get_latest_blockhash`,
/// broadcasting the transaction), so the extra round-trips add no new
/// architectural surface there.
fn ensure_warm_pool_slots_are_warmed(client: &RpcClient, plan: &BundlePlan) -> Result<()> {
    let DecoyMode::WarmPool { pool_size } = plan.decoy_mode else {
        return Ok(()); // DecoyMode::Fresh never claims prior history -- nothing to check.
    };

    let slots: Vec<(u32, Pubkey)> = plan
        .legs
        .iter()
        .filter(|l| !l.is_real)
        .map(|l| {
            (
                l.decoy_index
                    .expect("decoy leg always carries a decoy_index"),
                l.dest,
            )
        })
        .collect();

    let mut warmed: HashSet<Pubkey> = HashSet::new();
    for (slot, pubkey) in &slots {
        let sigs = client
            .get_signatures_for_address_with_config(
                pubkey,
                GetConfirmedSignaturesForAddress2Config {
                    before: None,
                    until: None,
                    limit: Some(1),
                    commitment: Some(CommitmentConfig::confirmed()),
                },
            )
            .with_context(|| {
                format!("checking on-chain history for warm-pool slot {slot} ({pubkey})")
            })?;
        if !sigs.is_empty() {
            warmed.insert(*pubkey);
        }
    }
    check_warm_pool_slots_have_history(&slots, pool_size, &warmed)
}

/// Pure fail-closed decision, isolated from `RpcClient` so it's directly
/// unit-testable without a live/mocked RPC connection: given the warm-pool
/// decoy slots a plan would actually use and the subset of those pubkeys
/// known (by whatever means the caller checked) to have on-chain history,
/// fail loudly on the first slot that doesn't.
fn check_warm_pool_slots_have_history(
    slots: &[(u32, Pubkey)],
    pool_size: u32,
    warmed: &HashSet<Pubkey>,
) -> Result<()> {
    for (slot, pubkey) in slots {
        if !warmed.contains(pubkey) {
            return Err(anyhow!(
                "cannot use --decoy-mode warm-pool: pool slot {slot} ({pubkey}) has no on-chain \
                 history — run 'supersonic warm --pool-size {pool_size}' first"
            ));
        }
    }
    Ok(())
}

/// Figure out which decoy mode to reconstruct with: explicit CLI overrides win;
/// otherwise fall back to what was recorded locally at send time; otherwise
/// **fail loudly**. Silently assuming `Fresh` here would be actively wrong for
/// a bundle sent with `--decoy-mode warm-pool`: `recover()` would derive the
/// wrong decoy addresses, find them empty, and print "nothing to recover" --
/// indistinguishable from real success -- while the real decoys sit untouched
/// at the addresses this function never even looked at.
fn resolve_recover_mode(
    master_seed: &[u8; 32],
    bundle_id: u64,
    decoy_mode: Option<DecoyModeArg>,
    pool_size: Option<u32>,
) -> Result<(DecoyModeArg, u32)> {
    if let Some(m) = decoy_mode {
        return Ok((m, pool_size.unwrap_or(32)));
    }
    let s = with_store_lock_shared(load_store)?;
    match s
        .bundles
        .get(&bundle_id)
        .and_then(|enc| decrypt_record(master_seed, enc).ok())
    {
        Some(r) => Ok((r.decoy_mode, r.pool_size.max(1))),
        None => Err(anyhow!(
            "cannot recover bundle {bundle_id}: no local record for it in {} (missing, or it \
             failed to decrypt -- e.g. a different machine, a restored wallet, or a lost \
             ~/.supersonic directory). The decoy mode this bundle was sent with can't be \
             determined automatically, and guessing `fresh` would silently derive the wrong \
             decoy addresses if it was actually sent with `--decoy-mode warm-pool` -- you'd see \
             \"nothing to recover\" while your real decoys sit untouched elsewhere. Pass \
             --decoy-mode explicitly (and --pool-size too, if it was warm-pool) to recover.",
            store_path().display()
        )),
    }
}

/// Which decoy source a past bundle used, resolved from either an explicit
/// override or the local record (see `resolve_recover_mode`).
struct RecoverSource {
    decoy_mode: DecoyModeArg,
    pool_size: u32,
}

fn recover(
    client: &RpcClient,
    payer: &Keypair,
    master_seed: &[u8; 32],
    bundle_id: u64,
    k: usize,
    disperse: bool,
    source: RecoverSource,
) -> Result<()> {
    let n_decoys = k.saturating_sub(1);
    let decoy_kps: Vec<Keypair> = match source.decoy_mode {
        DecoyModeArg::Fresh => (0..n_decoys)
            .map(|i| derive_decoy_keypair(master_seed, bundle_id, i as u32))
            .collect(),
        DecoyModeArg::WarmPool => {
            select_pool_slots(master_seed, bundle_id, n_decoys, source.pool_size)
                .into_iter()
                .map(|slot| derive_pool_member_keypair(master_seed, slot))
                .collect()
        }
    };

    if disperse {
        return recover_dispersed(client, payer, master_seed, bundle_id, &decoy_kps);
    }

    println!("[warn] Consolidating all decoys into one wallet re-links them (THREAT_MODEL");
    println!("       §4.6). For stronger privacy use --disperse, or recover late/spread.\n");

    let mut instrs = Vec::new();
    let mut signers: Vec<&Keypair> = vec![payer];
    let mut swept = 0u64;
    for kp in &decoy_kps {
        let bal = client.get_balance(&kp.pubkey()).unwrap_or(0);
        if bal > 0 {
            instrs.push(system_instruction::transfer(
                &kp.pubkey(),
                &payer.pubkey(),
                bal,
            ));
            signers.push(kp);
            swept += bal;
        }
    }
    if instrs.is_empty() {
        println!("nothing to recover for bundle {bundle_id} (decoys already empty)");
        return Ok(());
    }
    let bh = client.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(&instrs, Some(&payer.pubkey()), &signers, bh);
    let n_signers = signers.len();
    // Signing is done; drop the borrows and the decoy keypairs themselves now instead
    // of at function end, so their secret material (zeroized on drop by ed25519-dalek's
    // own `SecretKey` impl, see SECURITY.md) is gone before the network round-trip below
    // rather than lingering in memory for its duration.
    drop(signers);
    drop(decoy_kps);
    let sig = client
        .send_and_confirm_transaction_with_spinner(&tx)
        .context("recover sweep")?;
    println!(
        "recovered {} SOL from {} decoys of bundle {bundle_id}\n  signature: {sig}",
        lamports_to_sol(swept),
        n_signers - 1
    );
    Ok(())
}

/// Dispersed recovery: sweep each decoy to its own distinct, seed-derived sink in a
/// **separate** transaction. An observer sees K-1 unrelated onward transfers to K-1
/// distinct addresses — no star into one wallet, no common sink to link them — which
/// removes the naive-consolidation tell (THREAT_MODEL §4.6). Funds stay recoverable
/// (sinks are derived from the master seed). This is the mitigation as code, not prose.
fn recover_dispersed(
    client: &RpcClient,
    payer: &Keypair,
    master_seed: &[u8; 32],
    bundle_id: u64,
    decoy_kps: &[Keypair],
) -> Result<()> {
    println!("[disperse] sweeping each decoy to its own sink in a separate tx (no star).\n");
    let mut swept = 0u64;
    let mut n = 0usize;
    for (i, kp) in decoy_kps.iter().enumerate() {
        let bal = client.get_balance(&kp.pubkey()).unwrap_or(0);
        if bal == 0 {
            continue;
        }
        let sink = derive_sink_keypair(master_seed, bundle_id, i as u32);
        let ix = system_instruction::transfer(&kp.pubkey(), &sink.pubkey(), bal);
        // Fresh blockhash per leg keeps the sweeps as independent transactions.
        let bh = client.get_latest_blockhash()?;
        let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[payer, kp], bh);
        let sig = client
            .send_and_confirm_transaction_with_spinner(&tx)
            .with_context(|| format!("disperse decoy {i}"))?;
        println!(
            "  decoy {i} -> sink {} ({} SOL)  {sig}",
            sink.pubkey(),
            lamports_to_sol(bal)
        );
        swept += bal;
        n += 1;
    }
    if n == 0 {
        println!("nothing to recover for bundle {bundle_id} (decoys already empty)");
    } else {
        println!(
            "\ndispersed {} SOL across {n} distinct sinks — no consolidation star",
            lamports_to_sol(swept)
        );
    }
    Ok(())
}

// ----- local record keeping -----
//
// Per-bundle records contain exactly what the rest of this tool exists to hide
// (`real_index`, the real destination) — so they're encrypted at rest, not just
// written as plain JSON. Key is derived from the wallet (same trust boundary as
// the recovery secret: whoever holds the keypair can already derive/recover
// everything anyway); nonce is random per record, stored alongside the
// ciphertext (never secret, must never repeat under the same key — random
// 96-bit is enough at this volume). `bundle_id`/`next_id` stay in cleartext as
// map keys — they're just counters, not sensitive on their own.

const KDF_LOCAL_STORE: &[u8] = b"supersonic-tx/local-store/v1";

#[derive(Serialize, Deserialize, Default)]
struct Store {
    next_id: u64,
    bundles: BTreeMap<u64, EncryptedRecord>,
}

#[derive(Serialize, Deserialize, Clone)]
struct EncryptedRecord {
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Record {
    bundle_id: u64,
    to: String,
    amount_sol: f64,
    k: usize,
    real_index: usize,
    signature: String,
    /// How this bundle's decoys were sourced — needed by `recover` to
    /// reconstruct the right addresses. Defaults to `Fresh` for records
    /// written before this field existed (`serde(default)`).
    #[serde(default = "default_decoy_mode")]
    decoy_mode: DecoyModeArg,
    #[serde(default)]
    pool_size: u32,
}

fn default_decoy_mode() -> DecoyModeArg {
    DecoyModeArg::Fresh
}

fn local_store_cipher(master_seed: &[u8; 32]) -> ChaCha20Poly1305 {
    let mut h = Sha256::new();
    h.update(KDF_LOCAL_STORE);
    h.update(master_seed);
    // Intermediate 32-byte cipher key derived from `master_seed` — a local,
    // owned buffer, so it's cheap and safe to wipe explicitly once the cipher
    // is built from it, instead of leaving it sitting in memory for the rest
    // of the process's life.
    let mut key: [u8; 32] = h.finalize().into();
    let cipher = ChaCha20Poly1305::new((&key).into());
    key.zeroize();
    cipher
}

fn encrypt_record(master_seed: &[u8; 32], record: &Record) -> Result<EncryptedRecord> {
    let plaintext = serde_json::to_vec(record)?;
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = local_store_cipher(master_seed)
        .encrypt(nonce, plaintext.as_ref())
        .map_err(|e| anyhow!("encrypt local record: {e}"))?;
    Ok(EncryptedRecord {
        nonce: nonce_bytes.to_vec(),
        ciphertext,
    })
}

fn decrypt_record(master_seed: &[u8; 32], enc: &EncryptedRecord) -> Result<Record> {
    let nonce = Nonce::from_slice(&enc.nonce);
    let plaintext = local_store_cipher(master_seed)
        .decrypt(nonce, enc.ciphertext.as_ref())
        .map_err(|e| anyhow!("decrypt local record (wrong wallet?): {e}"))?;
    Ok(serde_json::from_slice(&plaintext)?)
}

fn store_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".supersonic").join("bundles.json")
}

fn load_store() -> Store {
    let p = store_path();
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_store(s: &Store) -> Result<()> {
    let p = store_path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
        restrict_permissions(dir, 0o700)?;
    }
    std::fs::write(&p, serde_json::to_string_pretty(s)?)?;
    restrict_permissions(&p, 0o600)?;
    Ok(())
}

/// Best-effort on Unix (WSL/Linux/macOS, this project's actual deployment
/// target); a no-op on platforms without POSIX permission bits. Defense in
/// depth alongside encryption — encryption protects the file's *contents* if
/// it leaks (backup, forensic image, exfiltration); this narrows *who on this
/// machine* can read it at all.
#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}
#[cfg(not(unix))]
fn restrict_permissions(_path: &std::path::Path, _mode: u32) -> Result<()> {
    Ok(())
}

fn lock_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".supersonic").join(".lock")
}

/// Open (creating if needed) the `~/.supersonic/.lock` file used by both
/// `with_store_lock` and `with_store_lock_shared`. Shared setup so the two
/// lock flavors can't drift on directory creation / open options.
fn open_lock_file() -> Result<std::fs::File> {
    let p = lock_path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&p)
        .with_context(|| format!("open lock file {}", p.display()))
}

/// Hold an OS-level exclusive lock (`~/.supersonic/.lock`) across a
/// load -> modify -> save cycle on the local store. Without this, two
/// concurrent `supersonic send` processes on the same machine can both
/// `load_store()` the same `next_id`, both mint the same `bundle_id` (and,
/// since decoy derivation is seeded by `bundle_id`, byte-identical decoy
/// addresses -- a linkability leak), and one silently clobbers the other's
/// entry in `s.bundles` on save. The guard is RAII: it's released when it
/// drops, including on an early `?` return out of `f`, so a mid-critical-
/// section error can't wedge the store for later runs.
fn with_store_lock<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let file = open_lock_file()?;
    let mut rw = fd_lock::RwLock::new(file);
    let _guard = rw
        .write()
        .with_context(|| format!("acquire lock on {}", lock_path().display()))?;
    f()
}

/// Hold an OS-level *shared* lock (same `~/.supersonic/.lock` as
/// `with_store_lock`) across a read-only `load_store()`. Multiple readers can
/// hold this concurrently, but it still blocks against a concurrent
/// `with_store_lock` writer -- so a reader can no longer observe the store
/// file transiently truncated/partial mid-write by another process (N-1:
/// `resolve_recover_mode` and `inspect` previously called `load_store()`
/// with no lock at all).
fn with_store_lock_shared<T>(f: impl FnOnce() -> T) -> Result<T> {
    let file = open_lock_file()?;
    let rw = fd_lock::RwLock::new(file);
    let _guard = rw
        .read()
        .with_context(|| format!("acquire read lock on {}", lock_path().display()))?;
    Ok(f())
}

fn next_bundle_id() -> Result<u64> {
    with_store_lock(|| {
        let mut s = load_store();
        let id = s.next_id.max(1);
        s.next_id = id + 1;
        save_store(&s)?;
        Ok(id)
    })
}

fn record_bundle(master_seed: &[u8; 32], plan: &BundlePlan, a: &PlanArgs, sig: &str) -> Result<()> {
    with_store_lock(|| {
        let mut s = load_store();
        let record = Record {
            bundle_id: plan.bundle_id,
            to: a.to.clone(),
            amount_sol: a.amount,
            k: a.k,
            real_index: plan.real_index,
            signature: sig.to_string(),
            decoy_mode: a.decoy_mode,
            pool_size: a.pool_size,
        };
        s.bundles
            .insert(plan.bundle_id, encrypt_record(master_seed, &record)?);
        if plan.bundle_id >= s.next_id {
            s.next_id = plan.bundle_id + 1;
        }
        save_store(&s)
    })
}

fn format_record_line(r: &Record) -> String {
    format!(
        "bundle {} · K={} · {} SOL -> {} · real_index={} · {}",
        r.bundle_id, r.k, r.amount_sol, r.to, r.real_index, r.signature
    )
}

/// Builds the display lines for `inspect` with no `--bundle-id` filter (list
/// everything). Deliberately does not propagate a single record's decrypt
/// error with `?` -- one corrupted/undecryptable record (wrong wallet, bit
/// rot, truncated file, ...) must not hide every other record that decrypts
/// fine. Extracted from `inspect` so this behavior is unit-testable without
/// capturing stdout.
fn list_all_lines(master_seed: &[u8; 32], s: &Store) -> Vec<String> {
    if s.bundles.is_empty() {
        return vec!["no bundles recorded yet".to_string()];
    }
    s.bundles
        .iter()
        .map(|(bundle_id, enc)| match decrypt_record(master_seed, enc) {
            Ok(r) => format_record_line(&r),
            Err(e) => format!(
                "  [warn] bundle {bundle_id} record not decodable, skipping - could not decrypt (wrong wallet? corrupted record?): {e}"
            ),
        })
        .collect()
}

fn inspect(master_seed: &[u8; 32], bundle_id: Option<u64>) -> Result<()> {
    let s = with_store_lock_shared(load_store)?;
    match bundle_id {
        Some(id) => match s.bundles.get(&id) {
            Some(enc) => println!("{}", format_record_line(&decrypt_record(master_seed, enc)?)),
            None => println!("no local record for bundle {id}"),
        },
        None => {
            for line in list_all_lines(master_seed, &s) {
                println!("{line}");
            }
        }
    }
    Ok(())
}

// ----- helpers -----

fn rpc(url: &str) -> RpcClient {
    RpcClient::new_with_commitment(url.to_string(), CommitmentConfig::confirmed())
}

fn load_keypair(path: &Option<PathBuf>) -> Result<Keypair> {
    let p = path.clone().unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".config/solana/id.json")
    });
    read_keypair_file(&p).map_err(|e| anyhow!("read keypair {}: {e}", p.display()))
}

fn derive_master_seed(kp: &Keypair) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(MASTER_TAG);
    h.update(kp.to_bytes()); // 64-byte secret+pubkey; never leaves this machine
    h.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `load_store`/`save_store`/`next_bundle_id`/`with_store_lock` all resolve
    /// paths off the `HOME` env var, which is process-global -- so tests that
    /// point it at a scratch directory must not run concurrently with each
    /// other (cargo runs tests in threads within one process by default).
    static HOME_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Run `f` with `HOME` pointed at a fresh, empty scratch directory (so
    /// `~/.supersonic/*` in `f` never touches a real user's data), restoring
    /// the previous `HOME` and removing the scratch directory afterwards.
    fn with_temp_home<T>(f: impl FnOnce() -> T) -> T {
        let _env_guard = HOME_ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "supersonic-cli-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let prev_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", &dir);

        let result = f();

        match prev_home {
            Some(p) => std::env::set_var("HOME", p),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    fn sample_record() -> Record {
        Record {
            bundle_id: 7,
            to: "GhaNhctXJ5K1T2Ebe16X64D2S6a4qpTF3H4MNUkS5PUt".to_string(),
            amount_sol: 0.02,
            k: 8,
            real_index: 3,
            signature: "somesignature".to_string(),
            decoy_mode: DecoyModeArg::Fresh,
            pool_size: 0,
        }
    }

    /// The bug class this module exists to prevent: a local record must not be
    /// readable as plaintext JSON on disk, and must decrypt correctly for the
    /// wallet that wrote it.
    #[test]
    fn local_record_is_encrypted_and_round_trips() {
        let seed = [42u8; 32];
        let record = sample_record();
        let enc = encrypt_record(&seed, &record).unwrap();

        // The point of this test: the real_index/to that everything else in
        // this tool exists to hide must not appear as plaintext bytes in what
        // gets written to disk.
        assert!(!enc.ciphertext.windows(8).any(|w| w == b"real_ind"));
        assert!(!enc
            .ciphertext
            .windows(record.to.len())
            .any(|w| w == record.to.as_bytes()));

        let decrypted = decrypt_record(&seed, &enc).unwrap();
        assert_eq!(decrypted.bundle_id, record.bundle_id);
        assert_eq!(decrypted.to, record.to);
        assert_eq!(decrypted.real_index, record.real_index);
        assert_eq!(decrypted.signature, record.signature);
    }

    #[test]
    fn wrong_wallet_cannot_decrypt() {
        let enc = encrypt_record(&[1u8; 32], &sample_record()).unwrap();
        assert!(
            decrypt_record(&[2u8; 32], &enc).is_err(),
            "a different wallet's derived key must not decrypt another wallet's records"
        );
    }

    #[test]
    fn nonce_is_random_per_record_not_reused() {
        let seed = [9u8; 32];
        let a = encrypt_record(&seed, &sample_record()).unwrap();
        let b = encrypt_record(&seed, &sample_record()).unwrap();
        assert_ne!(
            a.nonce, b.nonce,
            "reused nonces under the same key break AEAD security"
        );
        assert_ne!(
            a.ciphertext, b.ciphertext,
            "identical plaintext + fresh nonce must not produce identical ciphertext"
        );
    }

    /// F-001 regression: if the local record is missing or undecryptable (lost
    /// `~/.supersonic`, different machine, wallet restored from seed) and the
    /// caller didn't pass `--decoy-mode` explicitly, `resolve_recover_mode`
    /// must fail loudly instead of silently assuming `Fresh` -- which would
    /// derive the wrong decoy addresses for a bundle actually sent with
    /// `--decoy-mode warm-pool`, and `recover()` would then report "nothing
    /// to recover" (indistinguishable from real success) while the real
    /// decoys sit untouched at addresses this path never even looked at.
    #[test]
    fn recover_mode_errors_when_record_missing_or_undecryptable_and_no_override() {
        with_temp_home(|| {
            let seed = [7u8; 32];

            // Case 1: no bundles.json at all.
            let missing = resolve_recover_mode(&seed, 42, None, None);
            assert!(
                missing.is_err(),
                "missing local record + no explicit --decoy-mode must error, not assume Fresh"
            );
            let msg = missing.unwrap_err().to_string();
            assert!(
                msg.contains("--decoy-mode"),
                "error should direct the user to pass --decoy-mode explicitly, got: {msg}"
            );

            // Case 2: bundles.json exists, but the entry for this bundle_id
            // was encrypted under a different wallet's key -- undecryptable
            // (e.g. corrupted, or recovered under the wrong keypair).
            let mut store = Store::default();
            let other_seed = [8u8; 32];
            store
                .bundles
                .insert(42, encrypt_record(&other_seed, &sample_record()).unwrap());
            save_store(&store).unwrap();

            let corrupted = resolve_recover_mode(&seed, 42, None, None);
            assert!(
                corrupted.is_err(),
                "undecryptable local record + no explicit --decoy-mode must error, not assume \
                 Fresh"
            );

            // Sanity: an explicit override must still work even with no
            // usable local record -- this path is not broken by the fix.
            let overridden =
                resolve_recover_mode(&seed, 42, Some(DecoyModeArg::WarmPool), Some(16));
            assert_eq!(overridden.unwrap(), (DecoyModeArg::WarmPool, 16));
        });
    }

    /// F-003 regression: `next_bundle_id()` must be safe under concurrent
    /// callers on the same machine. Before the fix, two racing calls could
    /// both read the same `next_id` and return the same bundle_id, which
    /// silently clobbers one bundle's local record with the other's and
    /// (since decoy derivation is seeded by `bundle_id`) produces
    /// byte-identical decoy addresses across the two bundles.
    #[test]
    fn next_bundle_id_unique_under_concurrent_callers() {
        with_temp_home(|| {
            use std::sync::Arc;
            use std::thread;

            const THREADS: usize = 8;
            const CALLS_PER_THREAD: usize = 25;

            let results: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
            let handles: Vec<_> = (0..THREADS)
                .map(|_| {
                    let results = Arc::clone(&results);
                    thread::spawn(move || {
                        let mut local = Vec::with_capacity(CALLS_PER_THREAD);
                        for _ in 0..CALLS_PER_THREAD {
                            local.push(next_bundle_id().expect("next_bundle_id"));
                        }
                        results.lock().unwrap().extend(local);
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap();
            }

            let mut ids = results.lock().unwrap().clone();
            let total = ids.len();
            assert_eq!(total, THREADS * CALLS_PER_THREAD);
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(
                ids.len(),
                total,
                "next_bundle_id() handed out a duplicate id under concurrent callers"
            );
        });
    }

    /// N-1 regression: `load_store()` reads must go through a lock too (a
    /// shared/read lock, so concurrent readers don't block each other), not
    /// just the writes in `next_bundle_id`/`record_bundle`. This doesn't
    /// simulate the race itself (that's what
    /// `next_bundle_id_unique_under_concurrent_callers` already covers for
    /// the write side) -- it confirms `with_store_lock_shared` actually
    /// acquires the shared lock on `~/.supersonic/.lock` and correctly
    /// propagates the closure's return value through.
    #[test]
    fn with_store_lock_shared_acquires_lock_and_returns_closure_value() {
        with_temp_home(|| {
            let value = with_store_lock_shared(|| 42u64).expect("acquire shared lock");
            assert_eq!(value, 42, "with_store_lock_shared must return f()'s value");

            // The lock file itself must exist afterwards (proves the lock
            // path was actually opened/locked, not skipped).
            assert!(
                lock_path().exists(),
                "with_store_lock_shared should have created the lock file at {}",
                lock_path().display()
            );

            // Multiple shared acquisitions must not deadlock each other --
            // that's the entire point of a *shared* (vs exclusive) lock.
            let a = with_store_lock_shared(|| 1u64).expect("first shared lock");
            let b = with_store_lock_shared(|| 2u64).expect("second shared lock");
            assert_eq!((a, b), (1, 2));
        });
    }

    /// N-2 regression: `worst_case_round_trips` must saturate to an
    /// obviously-over-the-cap value on overflow instead of wrapping around to
    /// a small number that would silently bypass `warm_pool`'s
    /// `MAX_TOTAL_ROUND_TRIPS` sanity check.
    #[test]
    fn worst_case_round_trips_saturates_instead_of_wrapping_on_overflow() {
        let result = worst_case_round_trips(u32::MAX, u32::MAX);
        assert_eq!(
            result,
            u64::MAX,
            "pool_size=u32::MAX * rounds=u32::MAX must saturate to u64::MAX on overflow, not \
             wrap around to a small value that could pass the sanity cap undetected"
        );
    }

    /// Non-overflowing inputs must still produce the exact expected worst
    /// case (3x jitter budget per round), so the overflow fix above didn't
    /// change normal-path behavior.
    #[test]
    fn worst_case_round_trips_exact_for_normal_inputs() {
        assert_eq!(worst_case_round_trips(10, 5), 10 * 15);
        assert_eq!(worst_case_round_trips(0, 100), 0);
        assert_eq!(worst_case_round_trips(100, 0), 0);
    }

    /// juiz-cego live-devnet finding: `inspect` with no `--bundle-id` (list
    /// everything) must not let a single corrupted/undecryptable record abort
    /// the whole listing -- every other record that decrypts fine must still
    /// show, with a warning (not a hard error) for the bad one.
    #[test]
    fn list_all_lines_skips_corrupted_record_instead_of_aborting() {
        let seed = [11u8; 32];
        let mut store = Store::default();

        // Good record, encrypted correctly under `seed`.
        let good = Record {
            bundle_id: 1,
            ..sample_record()
        };
        store
            .bundles
            .insert(1, encrypt_record(&seed, &good).unwrap());

        // Corrupted record: valid ciphertext under a *different* key, so
        // decrypting it under `seed` fails AEAD authentication -- simulates
        // "wrong wallet / bit rot / truncated file" without needing to hand
        // -roll invalid ChaCha20Poly1305 bytes.
        let other_seed = [12u8; 32];
        let bad = Record {
            bundle_id: 2,
            ..sample_record()
        };
        store
            .bundles
            .insert(2, encrypt_record(&other_seed, &bad).unwrap());

        let lines = list_all_lines(&seed, &store);

        assert_eq!(
            lines.len(),
            2,
            "one line per bundle expected, even though bundle 2 is undecryptable"
        );

        let good_line = lines
            .iter()
            .find(|l| l.starts_with("bundle 1"))
            .expect("good record (bundle 1) must still be listed");
        assert!(good_line.contains(&good.to));

        let warn_line = lines
            .iter()
            .find(|l| l.contains("bundle 2"))
            .expect("corrupted record (bundle 2) must produce a warning line, not vanish");
        assert!(
            warn_line.contains("[warn]") && warn_line.contains("skipping"),
            "expected a skip warning for the corrupted record, got: {warn_line}"
        );
        assert!(
            !warn_line.contains(&bad.to),
            "a record that failed to decrypt must never leak its plaintext fields"
        );
    }

    /// juiz-cego live-devnet finding (the "no fail-closed on a cold warm-pool"
    /// bug): the pure decision `check_warm_pool_slots_have_history` must fail
    /// loudly, naming the cold slot and the exact remediation command, when
    /// even one selected warm-pool slot lacks proven on-chain history --
    /// without needing a live/mocked RPC connection to exercise the logic.
    #[test]
    fn check_warm_pool_slots_have_history_fails_closed_on_a_single_cold_slot() {
        let warm_a = Pubkey::new_unique();
        let warm_b = Pubkey::new_unique();
        let cold_c = Pubkey::new_unique();
        let slots = vec![(0u32, warm_a), (1u32, warm_b), (5u32, cold_c)];

        let mut warmed = HashSet::new();
        warmed.insert(warm_a);
        warmed.insert(warm_b);
        // `cold_c` deliberately absent -- simulates a pool slot `select_pool_slots`
        // picked for this bundle that `supersonic warm` never touched.

        let err = check_warm_pool_slots_have_history(&slots, 32, &warmed)
            .expect_err("a slot with no proven history must fail closed, not proceed silently");
        let msg = err.to_string();
        assert!(
            msg.contains("--decoy-mode warm-pool"),
            "error should name the flag that triggered this check, got: {msg}"
        );
        assert!(
            msg.contains("pool slot 5"),
            "error should name the specific cold slot index, got: {msg}"
        );
        assert!(
            msg.contains(&cold_c.to_string()),
            "error should name the specific cold slot's pubkey, got: {msg}"
        );
        assert!(
            msg.contains("supersonic warm --pool-size 32"),
            "error should point at the exact remediation command, got: {msg}"
        );
    }

    /// Sanity companion to the above: when every selected slot has proven
    /// history, the check must pass silently instead of false-positiving.
    #[test]
    fn check_warm_pool_slots_have_history_passes_when_all_slots_are_warmed() {
        let a = Pubkey::new_unique();
        let b = Pubkey::new_unique();
        let slots = vec![(0u32, a), (3u32, b)];
        let mut warmed = HashSet::new();
        warmed.insert(a);
        warmed.insert(b);

        assert!(
            check_warm_pool_slots_have_history(&slots, 32, &warmed).is_ok(),
            "all selected slots warmed -- must not fail closed"
        );
    }

    /// `ensure_warm_pool_slots_are_warmed` must short-circuit to `Ok(())`
    /// without ever touching the network for `DecoyMode::Fresh` -- the fail-
    /// closed check exists only for `--decoy-mode warm-pool`, which never
    /// claims prior history in the first place. Uses a deliberately-invalid
    /// RPC URL: if this test hangs or errors on a connection attempt instead
    /// of returning immediately, the short-circuit is broken.
    #[test]
    fn ensure_warm_pool_slots_are_warmed_short_circuits_for_fresh_mode() {
        let seed = [3u8; 32];
        let plan = supersonic_sdk::plan_bundle(
            &seed,
            1,
            Pubkey::new_unique(),
            1_000_000_000,
            4,
            DecoyConfig::default(),
        )
        .unwrap();
        assert_eq!(plan.decoy_mode, DecoyMode::Fresh);

        // Never actually dialed for `Fresh` mode -- the function must return
        // before any RPC call is attempted.
        let client = RpcClient::new("http://127.0.0.1:1".to_string());
        assert!(ensure_warm_pool_slots_are_warmed(&client, &plan).is_ok());
    }
}
