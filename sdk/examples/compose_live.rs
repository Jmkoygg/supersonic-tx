//! Composability example — LIVE: the same third-party integration shown in
//! `compose.rs`, but this one actually casts the bundle for real against devnet.
//!
//! `compose.rs` builds the `execute_bundle` instruction *offline* (no RPC) and only
//! prints its shape — free, but not proof that the SDK's public API can carry a real
//! transaction end to end. This example is the missing half: it funds a brand-new
//! keypair from the devnet faucet, plans a real K-leg bundle with `plan_bundle`,
//! builds the instruction with `build_instruction` (same public API, no access to
//! `cli`/`harness`/`programs` internals), signs it, and broadcasts it against the
//! deployed router. It prints the confirmed signature and an Explorer link so the
//! result can be checked independently by anyone.
//!
//! This is a **self-contained** proof: it depends on nothing but a devnet RPC
//! endpoint and the already-deployed program — not on any other contributor's work
//! or timing (contrast with the separate third-party proof in PR #3, which routes a
//! real devnet transaction through this same router from someone else's tool).
//!
//! Run: `cargo run -p supersonic-sdk --example compose_live --release`
//!
//! Costs a small amount of devnet SOL, funded via `request_airdrop` (devnet SOL has
//! no real value). Requires network access to a devnet RPC endpoint; the public
//! faucet is rate-limited per source, so a run can occasionally fail with "airdrop
//! limit reached" — that's an infra quota, not a bug in the bundle logic itself.

use std::str::FromStr;
use std::thread::sleep;
use std::time::Duration;

use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    native_token::lamports_to_sol,
    pubkey::Pubkey,
    signature::{Keypair, Signature, Signer},
    transaction::Transaction,
};
use supersonic_sdk::{build_instruction, plan_bundle, DecoyConfig};

const DEVNET_RPC: &str = "https://api.devnet.solana.com";

fn main() {
    // The deployed router program id (a third party only needs this + the SDK).
    let program_id = Pubkey::from_str("BCrR3JKi5EWhC5DuKYzV4EX7ogawoWaoKkhSqZYeYabn").unwrap();

    let client =
        RpcClient::new_with_commitment(DEVNET_RPC.to_string(), CommitmentConfig::confirmed());

    // A brand-new wallet for this run — no pre-funding, no dependency on any
    // existing key. Small K and a small real leg keep the whole run cheap.
    let user = Keypair::new();
    let real_payee = Keypair::new().pubkey();
    let master_seed = [7u8; 32];
    let bundle_id = 1;
    let real_amount = 15_000_000; // 0.015 SOL
    let k = 8;

    println!("compose_live: self-contained on-chain proof, no third party involved.");
    println!("  wallet: {}", user.pubkey());

    // Fund the fresh wallet from the devnet faucet — comfortably covers K legs
    // drawn around real_amount (see DecoyConfig::default's band) plus fees.
    let airdrop_lamports = 300_000_000; // 0.3 SOL
    println!(
        "  requesting {} SOL airdrop on devnet...",
        lamports_to_sol(airdrop_lamports)
    );
    let airdrop_sig = client
        .request_airdrop(&user.pubkey(), airdrop_lamports)
        .expect("devnet airdrop request failed (faucet may be rate-limited — retry later)");
    wait_for_confirmation(&client, &airdrop_sig);
    println!("  airdrop confirmed: {airdrop_sig}");

    // 1. Plan the intent-ambiguous bundle — public API, no router internals.
    let plan = plan_bundle(
        &master_seed,
        bundle_id,
        real_payee,
        real_amount,
        k,
        DecoyConfig::default(),
    )
    .expect("valid bundle");

    // 2. Build the signable instruction — public API.
    let ix = build_instruction(program_id, user.pubkey(), &plan);

    println!("  real intent:  {real_amount} lamports -> {real_payee}");
    println!("  real hidden at index {} of {k} legs", plan.real_index);
    println!(
        "  instruction:  {} accounts, {} data bytes",
        ix.accounts.len(),
        ix.data.len()
    );

    // 3. Sign and broadcast the real transaction against devnet.
    let bh = client.get_latest_blockhash().expect("get_latest_blockhash");
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&user.pubkey()), &[&user], bh);

    println!("  sending live transaction to devnet...");
    let sig = client
        .send_and_confirm_transaction_with_spinner(&tx)
        .expect("send_and_confirm_transaction_with_spinner");

    println!("\nconfirmed on devnet:");
    println!("  signature: {sig}");
    println!("  explorer:  https://explorer.solana.com/tx/{sig}?cluster=devnet");
    println!(
        "\nThis was a real transaction against the deployed router, built and sent using only"
    );
    println!("supersonic-sdk's public API — self-contained, no third party involved.");
}

/// Poll until the airdrop transaction is confirmed (devnet airdrops aren't
/// instant), so the balance is actually spendable before we plan the bundle.
fn wait_for_confirmation(client: &RpcClient, sig: &Signature) {
    for _ in 0..60 {
        if let Ok(true) = client.confirm_transaction(sig) {
            return;
        }
        sleep(Duration::from_millis(500));
    }
    panic!("airdrop did not confirm in time");
}
