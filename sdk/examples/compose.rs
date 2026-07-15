//! Composability example: a *third-party tool* casting a bundle through
//! `supersonic-tx` using only the public SDK surface — no dependency on the router
//! program's source, no private internals. This is what an `account-cooker` (or any
//! other tool) does to "cast through" the router.
//!
//! Run: `cargo run -p supersonic-sdk --example compose`
//!
//! It builds a real, signable `execute_bundle` instruction offline (no RPC) and
//! prints its shape, proving the integration contract is the SDK's public API:
//! `plan_bundle` + `build_instruction` (+ `derive_decoy_keypair` / `derive_sink_keypair`
//! for recovery).

use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};
use std::str::FromStr;
use supersonic_sdk::{build_instruction, plan_bundle, DecoyConfig};

fn main() {
    // The deployed router program id (a third party only needs this + the SDK).
    let program_id =
        Pubkey::from_str("BCrR3JKi5EWhC5DuKYzV4EX7ogawoWaoKkhSqZYeYabn").unwrap();

    // The integrating tool's own wallet and its real intent.
    let user = Keypair::new();
    let real_payee = Keypair::new().pubkey();
    let master_seed = [42u8; 32]; // the tool's recovery secret
    let bundle_id = 7;
    let real_amount = 25_000_000; // 0.025 SOL
    let k = 8;

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

    println!("third-party tool composed a bundle through supersonic-tx:");
    println!("  program:      {}", ix.program_id);
    println!("  real intent:  {real_amount} lamports -> {real_payee}");
    println!("  real hidden at index {} of {k} legs", plan.real_index);
    println!("  instruction:  {} accounts, {} data bytes", ix.accounts.len(), ix.data.len());
    println!("  amounts (observer's view): {:?}", plan.amounts());
    println!("\nNo router source was touched — the SDK public API is the whole contract.");
}
