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
    build_instruction, derive_decoy_keypair, derive_sink_keypair, plan_bundle, BundlePlan,
    DecoyConfig,
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
    },
    /// Show locally-recorded bundles.
    Inspect {
        #[arg(long)]
        bundle_id: Option<u64>,
    },
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
        } => {
            let client = rpc(&cli.rpc);
            recover(&client, &keypair, &master_seed, *bundle_id, *k, *disperse)?;
        }
        Cmd::Inspect { bundle_id } => inspect(*bundle_id)?,
    }
    Ok(())
}

fn build_plan(master_seed: &[u8; 32], a: &PlanArgs) -> Result<(BundlePlan, u64)> {
    let to = Pubkey::from_str(&a.to).context("bad --to pubkey")?;
    let lamports = sol_to_lamports(a.amount);
    if lamports == 0 {
        return Err(anyhow!("amount too small"));
    }
    let bundle_id = a.bundle_id.unwrap_or_else(next_bundle_id);
    let plan = plan_bundle(
        master_seed,
        bundle_id,
        to,
        lamports,
        a.k,
        DecoyConfig::default(),
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

fn recover(
    client: &RpcClient,
    payer: &Keypair,
    master_seed: &[u8; 32],
    bundle_id: u64,
    k: usize,
    disperse: bool,
) -> Result<()> {
    let n_decoys = k.saturating_sub(1);
    let decoy_kps: Vec<Keypair> = (0..n_decoys)
        .map(|i| derive_decoy_keypair(master_seed, bundle_id, i as u32))
        .collect();

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
