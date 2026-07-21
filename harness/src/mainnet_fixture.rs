//! Real mainnet payee-profile fixture, collected by
//! `cargo run -p supersonic-harness --bin collect-mainnet-profiles --release`.
//!
//! Frozen (`include_str!`) into the binary at compile time — no network access at
//! harness run time, so `cargo test`/`cargo run` stay deterministic and CI-safe.
//! Re-running the collector overwrites this file with a fresh, honestly-labeled
//! sample; nothing here is synthesized. See `collect_mainnet_profiles.rs`'s module
//! doc for the full collection methodology and its stated limits.

use serde::Deserialize;

const RAW: &str = include_str!("../fixtures/mainnet_profiles.json");

#[derive(Deserialize, Clone)]
pub struct MainnetProfile {
    pub pubkey: String,
    /// Real `getSignaturesForAddress` count, capped at the collector's page limit
    /// (a lower bound, not exhaustive, when `signature_count_exact` is false).
    pub signature_count_lower_bound: u64,
    #[allow(dead_code)]
    pub signature_count_exact: bool,
    #[allow(dead_code)]
    pub earliest_funder_proxy: Option<String>,
    /// Funder-diversity lower bound (see the collector's module doc: a coarse
    /// proxy, effectively 0/1 in this collector's single-page implementation, not
    /// an exhaustive count).
    pub distinct_funders_lower_bound: u64,
    /// SPL Token + Token-2022 account count for this address.
    pub token_account_count: u64,
    /// "calibration" or "held_out", assigned at collection time before any
    /// measurement. Evaluation below uses `held_out` only — `calibration` is
    /// reserved for whatever mechanism (SDK-side warming pool, etc.) *fits*
    /// something to the data, keeping that separate from what *evaluates* it.
    pub split: String,
}

#[derive(Deserialize, Clone)]
pub struct MainnetFixture {
    pub cluster: String,
    pub collected_at: String,
    pub methodology: String,
    /// Real, passively-observed mainnet addresses with prior activity.
    pub aged: Vec<MainnetProfile>,
    /// Freshly-generated, never-used keypairs — independently re-verified to have
    /// zero signatures and zero token accounts at collection time.
    pub fresh_verified_zero: Vec<MainnetProfile>,
}

impl MainnetFixture {
    pub fn load() -> Self {
        serde_json::from_str(RAW).expect("harness/fixtures/mainnet_profiles.json must be valid")
    }

    pub fn aged_held_out(&self) -> Vec<&MainnetProfile> {
        self.aged.iter().filter(|p| p.split == "held_out").collect()
    }

    pub fn fresh_held_out(&self) -> Vec<&MainnetProfile> {
        self.fresh_verified_zero
            .iter()
            .filter(|p| p.split == "held_out")
            .collect()
    }
}
