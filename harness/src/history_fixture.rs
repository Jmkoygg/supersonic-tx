//! Real devnet address-history fixture, collected by
//! `cargo run -p supersonic-harness --bin collect-devnet-history --release`.
//!
//! Frozen (`include_str!`) into the binary at compile time — no network access at
//! harness run time, so `cargo test`/`cargo run` stay deterministic and CI-safe.
//! Re-running the collector overwrites this file with a fresh, honestly-labeled
//! sample; nothing here is synthesized.

use serde::Deserialize;

const RAW: &str = include_str!("../fixtures/devnet_history.json");

#[derive(Deserialize, Clone)]
pub struct AddressSample {
    pub pubkey: String,
    /// Real `getSignaturesForAddress` count for this pubkey at collection time.
    pub signature_count: u64,
}

#[derive(Deserialize, Clone)]
pub struct HistoryFixture {
    pub cluster: String,
    pub collected_at: String,
    pub methodology: String,
    /// Real, previously-active devnet addresses (funded + transacted by the
    /// collector), standing in for both "a real payee with prior activity" (naive
    /// regime) and "an account-cooker-warmed decoy" (pre-warmed regime) — the two
    /// roles a companion account-cooker would fill, applied to the same pool since
    /// no mature account-cooker exists yet to integrate with directly.
    pub aged: Vec<AddressSample>,
    /// Freshly-generated, never-funded, never-signed keypairs. `signature_count` is
    /// independently re-queried and expected to be 0 for every entry — the naive
    /// regime's zero-history decoy population.
    pub fresh_verified_zero: Vec<AddressSample>,
}

impl HistoryFixture {
    pub fn load() -> Self {
        serde_json::from_str(RAW).expect("harness/fixtures/devnet_history.json must be valid")
    }
}
