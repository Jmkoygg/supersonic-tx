//! Live functional proof that the deployed Pinocchio router
//! (`bench/pinocchio-router`) isn't just an on-chain-present binary — it
//! actually executes a real multi-destination bundle correctly, on devnet,
//! against the real deployed program id.
//!
//! `harness/tests/pinocchio_invariants.rs` proves the 8 invariants via
//! Mollusk (an offline, in-process SVM) using an arbitrary placeholder
//! program id, since Mollusk doesn't need a real deployment. This binary
//! proves the same instruction encoding works against the actual deployed
//! `.so`, over real devnet RPC, with real funded accounts and a real
//! confirmed transaction — the gap between "proven correct in isolation" and
//! "works as deployed" that a size/rent-only benchmark leaves open.
//!
//! Usage:
//!   verify-pinocchio-devnet-deploy --program-id <PUBKEY> --keypair <PATH> [--rpc URL]
//!
//! Instruction encoding matches `bench/pinocchio-router/src/lib.rs` exactly
//! (see `harness/tests/pinocchio_invariants.rs`'s doc comment): manual, no
//! Borsh/discriminator — `[count: u8][count × u64 LE amounts]`, accounts
//! `[payer (signer, writable), dest_0, .., dest_{count-1} (writable)]`.

use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    instruction::{AccountMeta, Instruction},
    native_token::lamports_to_sol,
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair},
    signer::Signer,
    transaction::Transaction,
};

struct Args {
    rpc: String,
    program_id: String,
    keypair: String,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        rpc: "https://api.devnet.solana.com".to_string(),
        program_id: String::new(),
        keypair: String::new(),
    };
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--rpc" => {
                i += 1;
                a.rpc = args[i].clone();
            }
            "--program-id" => {
                i += 1;
                a.program_id = args[i].clone();
            }
            "--keypair" => {
                i += 1;
                a.keypair = args[i].clone();
            }
            other => return Err(anyhow!("unknown arg: {other}")),
        }
        i += 1;
    }
    if a.program_id.is_empty() {
        return Err(anyhow!("--program-id <PUBKEY> is required"));
    }
    if a.keypair.is_empty() {
        return Err(anyhow!("--keypair <PATH> is required"));
    }
    Ok(a)
}

fn encode(amounts: &[u64]) -> Vec<u8> {
    let mut data = vec![amounts.len() as u8];
    for a in amounts {
        data.extend_from_slice(&a.to_le_bytes());
    }
    data
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let program_id = Pubkey::from_str(&args.program_id).context("bad --program-id")?;
    let payer = read_keypair_file(&args.keypair).map_err(|e| anyhow!("read --keypair: {e}"))?;
    let client = RpcClient::new_with_commitment(args.rpc.clone(), CommitmentConfig::confirmed());

    let before = client.get_balance(&payer.pubkey())?;
    println!(
        "payer {} balance before: {} SOL",
        payer.pubkey(),
        lamports_to_sol(before)
    );

    // Two fresh, never-used destinations, verified below to start at zero.
    let dest_a = Keypair::new().pubkey();
    let dest_b = Keypair::new().pubkey();
    for d in [&dest_a, &dest_b] {
        let bal = client.get_balance(d)?;
        if bal != 0 {
            return Err(anyhow!(
                "expected fresh destination {d} to start at 0 lamports, got {bal}"
            ));
        }
    }

    let amounts = [1_000_000u64, 2_000_000u64]; // 0.001 / 0.002 SOL, comfortably above rent-exempt minimums
    let data = encode(&amounts);
    let accounts = vec![
        AccountMeta::new(payer.pubkey(), true),
        AccountMeta::new(dest_a, false),
        AccountMeta::new(dest_b, false),
        // The program's own logic never reads this account (`Transfer::invoke()`
        // only takes `from`/`to`), but the runtime needs the CPI target present
        // among this instruction's accounts to resolve it -- same requirement
        // `harness/tests/pinocchio_invariants.rs::bundle()` documents and relies
        // on; omitting it fails with "insufficient account keys for instruction"
        // (confirmed empirically against this exact deployed program).
        AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
    ];
    let ix = Instruction {
        program_id,
        accounts,
        data,
    };

    let bh = client.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer], bh);
    let sig = client
        .send_and_confirm_transaction_with_spinner(&tx)
        .context("send ExecuteBundle-equivalent instruction to the deployed Pinocchio program")?;

    // Independently re-verify the effect via fresh RPC reads, not just "the
    // send call didn't error" -- the same discipline every other live proof
    // in this repo holds itself to.
    std::thread::sleep(Duration::from_millis(500));
    let bal_a = client.get_balance(&dest_a)?;
    let bal_b = client.get_balance(&dest_b)?;
    if bal_a != amounts[0] {
        return Err(anyhow!(
            "dest_a expected {} lamports, observed {bal_a}",
            amounts[0]
        ));
    }
    if bal_b != amounts[1] {
        return Err(anyhow!(
            "dest_b expected {} lamports, observed {bal_b}",
            amounts[1]
        ));
    }
    let after = client.get_balance(&payer.pubkey())?;

    println!("transaction confirmed: {sig}");
    println!(
        "dest_a {dest_a} -> {} lamports (expected {})",
        bal_a, amounts[0]
    );
    println!(
        "dest_b {dest_b} -> {} lamports (expected {})",
        bal_b, amounts[1]
    );
    println!(
        "payer balance after: {} SOL (spent {} SOL: {} transferred + fee)",
        lamports_to_sol(after),
        lamports_to_sol(before.saturating_sub(after)),
        lamports_to_sol(amounts.iter().sum())
    );
    println!(
        "\nThis is a real, atomic, 2-destination multi-transfer executed by the deployed \
        Pinocchio program ({program_id}) on {} -- independently reproducible with the same \
        RPC calls by anyone, not asserted from a Mollusk-only proof.",
        args.rpc
    );
    Ok(())
}
