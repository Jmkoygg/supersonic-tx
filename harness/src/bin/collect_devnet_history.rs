//! Real devnet address-history collector — produces `harness/fixtures/devnet_history.json`,
//! the source data for `destination::eval_history_measured`.
//!
//! No account-cooker exists yet to integrate with directly, so this is a minimal
//! stand-in: it funds and transacts its own "aged" pool of addresses for real, and
//! confirms a separate "fresh" pool has zero history via the same RPC call an
//! attacker would use. Every number in the output fixture is a real RPC result, not
//! assumed.
//!
//! Usage:
//!   collect-devnet-history --collected-at 2026-07-19T20:00:00Z [--aged-count N]
//!     [--fresh-count M] [--min-tx A] [--max-tx B] [--rpc URL] [--out path] [--seed S]
//!
//! Funds aged addresses via a single real transfer each from the default Solana CLI
//! keypair, which must already hold devnet SOL (free via faucet; this collector never
//! calls the faucet itself, to avoid tripping its rate limit).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use serde::Serialize;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    signature::{read_keypair_file, Keypair},
    signer::Signer,
    system_instruction,
    transaction::Transaction,
};

#[derive(Serialize)]
struct AddressSample {
    pubkey: String,
    signature_count: u64,
}

#[derive(Serialize)]
struct Fixture {
    cluster: String,
    collected_at: String,
    methodology: String,
    aged: Vec<AddressSample>,
    fresh_verified_zero: Vec<AddressSample>,
}

struct Args {
    aged_count: usize,
    fresh_count: usize,
    min_tx: u32,
    max_tx: u32,
    rpc: String,
    out: PathBuf,
    seed: u64,
    collected_at: String,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        aged_count: 18,
        fresh_count: 10,
        min_tx: 2,
        max_tx: 25,
        rpc: "https://api.devnet.solana.com".to_string(),
        out: PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/devnet_history.json"
        )),
        seed: 1,
        collected_at: String::new(),
    };
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--aged-count" => {
                i += 1;
                a.aged_count = args[i].parse()?;
            }
            "--fresh-count" => {
                i += 1;
                a.fresh_count = args[i].parse()?;
            }
            "--min-tx" => {
                i += 1;
                a.min_tx = args[i].parse()?;
            }
            "--max-tx" => {
                i += 1;
                a.max_tx = args[i].parse()?;
            }
            "--rpc" => {
                i += 1;
                a.rpc = args[i].clone();
            }
            "--out" => {
                i += 1;
                a.out = PathBuf::from(&args[i]);
            }
            "--seed" => {
                i += 1;
                a.seed = args[i].parse()?;
            }
            "--collected-at" => {
                i += 1;
                a.collected_at = args[i].clone();
            }
            other => return Err(anyhow!("unknown arg: {other}")),
        }
        i += 1;
    }
    if a.collected_at.is_empty() {
        return Err(anyhow!(
            "--collected-at <ISO8601 UTC> is required, e.g. `date -u +%Y-%m-%dT%H:%M:%SZ`"
        ));
    }
    Ok(a)
}

const LAMPORTS_PER_TX: u64 = 6_000; // 5000 fee + 1000 transferred, rounded up
const FUND_MARGIN: u64 = 50_000; // safety margin above rent-exempt minimum

fn retry<T>(mut f: impl FnMut() -> Result<T>) -> Result<T> {
    let mut last_err = None;
    for attempt in 0..6u32 {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                eprintln!("  [retry {attempt}] {e}");
                std::thread::sleep(Duration::from_millis(500 * (attempt as u64 + 1)));
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap())
}

fn confirm_and_wait(client: &RpcClient, tx: &Transaction) -> Result<String> {
    let sig = retry(|| {
        client
            .send_and_confirm_transaction_with_spinner(tx)
            .map_err(|e| anyhow!("{e}"))
    })?;
    Ok(sig.to_string())
}

fn collect_aged(
    client: &RpcClient,
    funder: &Keypair,
    rng: &mut ChaCha20Rng,
    min_tx: u32,
    max_tx: u32,
    rent_exempt_min: u64,
) -> Result<AddressSample> {
    let kp = Keypair::new();
    let tx_count = rng.gen_range(min_tx..=max_tx);
    // Must stay above the rent-exempt minimum after every send, or the runtime
    // rejects the tx ("insufficient funds for rent") — a non-zero, sub-rent-exempt
    // balance is not allowed to exist even transiently.
    let fund_lamports = tx_count as u64 * LAMPORTS_PER_TX + rent_exempt_min + FUND_MARGIN;

    let bh = retry(|| client.get_latest_blockhash().map_err(|e| anyhow!("{e}")))?;
    let fund_ix = system_instruction::transfer(&funder.pubkey(), &kp.pubkey(), fund_lamports);
    let fund_tx =
        Transaction::new_signed_with_payer(&[fund_ix], Some(&funder.pubkey()), &[funder], bh);
    confirm_and_wait(client, &fund_tx).context("fund aged address")?;
    println!(
        "  funded {} with {fund_lamports} lamports for {tx_count} txs",
        kp.pubkey()
    );

    for i in 0..tx_count {
        let bh = retry(|| client.get_latest_blockhash().map_err(|e| anyhow!("{e}")))?;
        let ix = system_instruction::transfer(&kp.pubkey(), &funder.pubkey(), 1_000);
        let tx = Transaction::new_signed_with_payer(&[ix], Some(&kp.pubkey()), &[&kp], bh);
        confirm_and_wait(client, &tx)
            .with_context(|| format!("aged tx {i}/{tx_count} for {}", kp.pubkey()))?;
        std::thread::sleep(Duration::from_millis(150));
    }

    let sigs = retry(|| {
        client
            .get_signatures_for_address(&kp.pubkey())
            .map_err(|e| anyhow!("{e}"))
    })?;
    let real_count = sigs.len() as u64;
    println!(
        "  {} -> {real_count} real signatures (intended {tx_count} + 1 funding)",
        kp.pubkey()
    );
    Ok(AddressSample {
        pubkey: kp.pubkey().to_string(),
        signature_count: real_count,
    })
}

fn collect_fresh(client: &RpcClient) -> Result<AddressSample> {
    let kp = Keypair::new();
    let sigs = retry(|| {
        client
            .get_signatures_for_address(&kp.pubkey())
            .map_err(|e| anyhow!("{e}"))
    })?;
    let real_count = sigs.len() as u64;
    if real_count != 0 {
        eprintln!(
            "  [warn] fresh address {} unexpectedly has {real_count} signatures",
            kp.pubkey()
        );
    }
    println!(
        "  {} -> {real_count} signatures (fresh, never funded)",
        kp.pubkey()
    );
    Ok(AddressSample {
        pubkey: kp.pubkey().to_string(),
        signature_count: real_count,
    })
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let client = RpcClient::new_with_commitment(args.rpc.clone(), CommitmentConfig::confirmed());

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let funder_path = PathBuf::from(&home).join(".config/solana/id.json");
    let funder = read_keypair_file(&funder_path)
        .map_err(|e| anyhow!("read funder keypair {}: {e}", funder_path.display()))?;
    let funder_balance = client.get_balance(&funder.pubkey())?;
    let rent_exempt_min = client.get_minimum_balance_for_rent_exemption(0)?;
    println!(
        "funder {} balance: {} SOL (rent-exempt minimum for a 0-byte account: {} lamports)",
        funder.pubkey(),
        funder_balance as f64 / 1e9,
        rent_exempt_min
    );

    let mut rng = ChaCha20Rng::seed_from_u64(args.seed);

    println!(
        "\ncollecting {} aged addresses ({}..={} real txs each)...",
        args.aged_count, args.min_tx, args.max_tx
    );
    let mut aged = Vec::with_capacity(args.aged_count);
    for n in 0..args.aged_count {
        println!("[aged {}/{}]", n + 1, args.aged_count);
        aged.push(collect_aged(
            &client,
            &funder,
            &mut rng,
            args.min_tx,
            args.max_tx,
            rent_exempt_min,
        )?);
    }

    println!(
        "\ncollecting {} fresh (never-funded) addresses...",
        args.fresh_count
    );
    let mut fresh_verified_zero = Vec::with_capacity(args.fresh_count);
    for n in 0..args.fresh_count {
        println!("[fresh {}/{}]", n + 1, args.fresh_count);
        fresh_verified_zero.push(collect_fresh(&client)?);
    }

    let fixture = Fixture {
        cluster: "devnet".to_string(),
        collected_at: args.collected_at,
        methodology: "Self-collected against Solana devnet (no mature account-cooker exists yet \
            to integrate with directly): each 'aged' address is a freshly-generated keypair, \
            funded by a single real transfer from the project's deploy/dev wallet, then used as \
            its own fee-payer for a random number of real system-program transfers (1000 \
            lamports each) back to the deploy wallet. signature_count is the real length of \
            getSignaturesForAddress for that pubkey after those transactions confirmed (includes \
            the 1 funding transaction). fresh_verified_zero addresses are generated and never \
            funded or used; signature_count is the real (independently re-verifiable, expected \
            zero) result of the same RPC call against devnet. Every number here is a live RPC \
            result at collection time, not assumed or modeled."
            .to_string(),
        aged,
        fresh_verified_zero,
    };

    let json = serde_json::to_string_pretty(&fixture)?;
    std::fs::write(&args.out, &json).with_context(|| format!("write {}", args.out.display()))?;
    println!(
        "\nwrote fixture to {} ({} aged, {} fresh)",
        args.out.display(),
        fixture.aged.len(),
        fixture.fresh_verified_zero.len()
    );
    Ok(())
}
