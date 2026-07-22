//! `supersonic` — CLI for casting intent-ambiguous transfer bundles and recovering
//! decoy funds.
//!
//! The master recovery secret is derived from your wallet key, so no extra secret
//! to manage: the same wallet that sends a bundle can always recover its decoys.
//! Per-bundle metadata is stored locally under `~/.supersonic/bundles.json` so you
//! can inspect and audit what you sent.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    native_token::{lamports_to_sol, sol_to_lamports},
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair},
    signer::Signer,
    system_instruction,
    transaction::Transaction,
};
use supersonic_sdk::{
    build_instruction, derive_decoy_keypair, derive_sink_keypair, plan_bundle_with_mode,
    warming::{derive_pool_member_keypair, select_pool_slots},
    BundlePlan, DecoyConfig, DecoyMode,
};

/// The deployed router program id (matches `declare_id!` in the program).
const DEFAULT_PROGRAM_ID: &str = "BCrR3JKi5EWhC5DuKYzV4EX7ogawoWaoKkhSqZYeYabn";
const DEFAULT_RPC: &str = "https://api.devnet.solana.com";
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
    let master_seed = derive_master_seed(&keypair);

    match &cli.cmd {
        Cmd::Plan(a) => {
            let (plan, _) = build_plan(&master_seed, a)?;
            print_plan(&plan, a);
        }
        Cmd::Send(a) => {
            let (plan, bundle_id) = build_plan(&master_seed, a)?;
            print_plan(&plan, a);
            let client = rpc(&cli.rpc);
            let sig = send_bundle(&client, &keypair, program_id, &plan)?;
            println!("\n[ok] sent bundle {bundle_id}");
            println!("   signature: {sig}");
            println!("   explorer:  https://explorer.solana.com/tx/{sig}?cluster=devnet");
            record_bundle(&plan, a, &sig.to_string())?;
        }
        Cmd::Recover {
            bundle_id,
            k,
            disperse,
            decoy_mode,
            pool_size,
        } => {
            let client = rpc(&cli.rpc);
            let (decoy_mode, pool_size) = resolve_recover_mode(*bundle_id, *decoy_mode, *pool_size);
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
        Cmd::Inspect { bundle_id } => inspect(*bundle_id)?,
        Cmd::Warm { pool_size, rounds } => {
            let client = rpc(&cli.rpc);
            warm_pool(&client, &keypair, &master_seed, *pool_size, *rounds)?;
        }
    }
    Ok(())
}

/// Build up real signature history on `pool_size` warm-pool slots: `rounds`
/// real fund-in + sweep-back round trips per slot, each adding 2 signatures.
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
    const MAX_TOTAL_ROUND_TRIPS: u64 = 2_000; // sanity cap: ~pool_size * rounds, see below
    let total_round_trips = pool_size as u64 * rounds as u64;
    if total_round_trips > MAX_TOTAL_ROUND_TRIPS {
        return Err(anyhow!(
            "pool_size * rounds = {total_round_trips} exceeds the sanity cap of {MAX_TOTAL_ROUND_TRIPS} \
             round-trip transactions (~{} SOL touched, {total_round_trips} blocking RPC round-trips). \
             Run in smaller batches (lower --pool-size or --rounds per invocation) if you really need more.",
            lamports_to_sol(DUST_LAMPORTS * total_round_trips)
        ));
    }
    for slot in 0..pool_size {
        let kp = derive_pool_member_keypair(master_seed, slot);
        for r in 0..rounds {
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
        println!(
            "  slot {slot}/{pool_size}: {} -> +{} real signatures this run",
            kp.pubkey(),
            rounds * 2
        );
    }
    println!("\nwarmed {pool_size} pool slots ({rounds} round-trip(s) each). Use --decoy-mode warm-pool --pool-size {pool_size} on `plan`/`send`.");
    Ok(())
}

fn build_plan(master_seed: &[u8; 32], a: &PlanArgs) -> Result<(BundlePlan, u64)> {
    let to = Pubkey::from_str(&a.to).context("bad --to pubkey")?;
    let lamports = sol_to_lamports(a.amount);
    if lamports == 0 {
        return Err(anyhow!("amount too small"));
    }
    let bundle_id = a.bundle_id.unwrap_or_else(next_bundle_id);
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

/// Figure out which decoy mode to reconstruct with: explicit CLI overrides win;
/// otherwise fall back to what was recorded locally at send time; otherwise
/// `Fresh` (today's default, and what every record predating this field used).
fn resolve_recover_mode(
    bundle_id: u64,
    decoy_mode: Option<DecoyModeArg>,
    pool_size: Option<u32>,
) -> (DecoyModeArg, u32) {
    if let Some(m) = decoy_mode {
        return (m, pool_size.unwrap_or(32));
    }
    let s = load_store();
    match s.bundles.get(&bundle_id) {
        Some(r) => (r.decoy_mode, r.pool_size.max(1)),
        None => (DecoyModeArg::Fresh, 0),
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
    let sig = client
        .send_and_confirm_transaction_with_spinner(&tx)
        .context("recover sweep")?;
    println!(
        "recovered {} SOL from {} decoys of bundle {bundle_id}\n  signature: {sig}",
        lamports_to_sol(swept),
        signers.len() - 1
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

#[derive(Serialize, Deserialize, Default)]
struct Store {
    next_id: u64,
    bundles: BTreeMap<u64, Record>,
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
    }
    std::fs::write(&p, serde_json::to_string_pretty(s)?)?;
    Ok(())
}

fn next_bundle_id() -> u64 {
    let mut s = load_store();
    let id = s.next_id.max(1);
    s.next_id = id + 1;
    let _ = save_store(&s);
    id
}

fn record_bundle(plan: &BundlePlan, a: &PlanArgs, sig: &str) -> Result<()> {
    let mut s = load_store();
    s.bundles.insert(
        plan.bundle_id,
        Record {
            bundle_id: plan.bundle_id,
            to: a.to.clone(),
            amount_sol: a.amount,
            k: a.k,
            real_index: plan.real_index,
            signature: sig.to_string(),
            decoy_mode: a.decoy_mode,
            pool_size: a.pool_size,
        },
    );
    if plan.bundle_id >= s.next_id {
        s.next_id = plan.bundle_id + 1;
    }
    save_store(&s)
}

fn inspect(bundle_id: Option<u64>) -> Result<()> {
    let s = load_store();
    let show = |r: &Record| {
        println!(
            "bundle {} · K={} · {} SOL -> {} · real_index={} · {}",
            r.bundle_id, r.k, r.amount_sol, r.to, r.real_index, r.signature
        );
    };
    match bundle_id {
        Some(id) => match s.bundles.get(&id) {
            Some(r) => show(r),
            None => println!("no local record for bundle {id}"),
        },
        None => {
            if s.bundles.is_empty() {
                println!("no bundles recorded yet");
            }
            for r in s.bundles.values() {
                show(r);
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
