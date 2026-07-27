//! Cross-bundle sub-funder learning channel — an escalating counterpart to
//! `mainnet_channel::eval_mainnet_funding_graph_shipped_mechanism_subfunder_pool`.
//! That function measures a single-bundle attacker with no memory across
//! bundles, and finds the per-slot sub-funder mitigation collapses its
//! advantage to `0.0`. This module measures a stronger, realistic attacker:
//! one who has observed every *prior* bundle this signer cast from the same
//! warm pool.
//!
//! Why that attacker is real: `derive_subfunder_keypair(master_seed, slot)`
//! is a function of `(master_seed, slot)` only, not `bundle_id` — the exact
//! same sub-funder recurs every time `select_pool_slots` draws that slot
//! again for a later bundle from the same signer. An observer who has seen
//! every leg's funder across N earlier bundles has learned the sub-funder
//! identity of every distinct pool slot drawn so far. In bundle N+1, any leg
//! whose funder is already in that learned set is (with overwhelming
//! probability) a decoy — the real leg's funder is a real, distinct,
//! essentially-never-repeating historical address, not a member of the small,
//! closed sub-funder set. Once the learned set covers most of `pool_size`'s
//! distinct sub-funders, this attacker approaches the same ~100% accuracy the
//! single-bundle same-wallet attacker had *before* the per-slot mitigation —
//! just delayed until enough bundles accumulate instead of present from
//! bundle 1.
//!
//! This is exactly the residual `THREAT_MODEL.md §6`'s "does not defend"
//! clause already names in general terms ("a global adversary correlating
//! across many bundles over time... fee-payer behavior across many bundles
//! from the same signer — no measured advantage number exists for this
//! channel"). This module is that missing number.

use std::collections::HashSet;

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use solana_sdk::pubkey::Pubkey as SdkPubkey;
use solana_sdk::signer::Signer;
use supersonic_sdk::warming::{derive_subfunder_keypair, select_pool_slots};

/// Per-bundle-index advantage (accuracy - 1/K) of the cross-bundle learning
/// attacker described above, averaged over `trials` independent simulated
/// signers (each with its own random `master_seed`), for `num_bundles`
/// bundles per signer. `k` and `pool_size` match the real shipped mechanism's
/// parameters (`sdk/src/warming.rs::select_pool_slots`,
/// `derive_subfunder_keypair`) — this is not a synthetic model, it calls the
/// exact same SDK functions `cli/src/main.rs::warm_pool`/`send` use.
pub fn eval_cross_bundle_subfunder_learning(
    pool_size: u32,
    k: usize,
    num_bundles: usize,
    trials: usize,
    seed: u64,
) -> Vec<f64> {
    let mut hits = vec![0u64; num_bundles];
    let mut rng = ChaCha20Rng::seed_from_u64(seed);

    for trial in 0..trials {
        let mut master_seed = [0u8; 32];
        rng.fill(&mut master_seed);
        let mut real_index_rng =
            ChaCha20Rng::seed_from_u64(seed ^ (trial as u64).wrapping_mul(0x9E3779B97F4A7C15));

        let mut seen: HashSet<SdkPubkey> = HashSet::new();
        for bundle_id in 0..num_bundles as u64 {
            let decoy_slots = select_pool_slots(&master_seed, bundle_id, k - 1, pool_size);
            let decoy_funders: Vec<SdkPubkey> = decoy_slots
                .iter()
                .map(|&slot| derive_subfunder_keypair(&master_seed, slot).pubkey())
                .collect();
            // The real leg's funder is a real, distinct historical address --
            // modeled here as a fresh random pubkey, which is a safe stand-in
            // specifically because collision with the closed sub-funder set
            // is astronomically unlikely (2^-256), not because real funder
            // identity doesn't matter elsewhere (it does, see
            // mainnet_channel.rs's mainnet-fixture-based channels).
            let real_funder =
                SdkPubkey::new_from_array(std::array::from_fn(|_| real_index_rng.gen::<u8>()));
            let real_index = real_index_rng.gen_range(0..k);

            let mut funders: Vec<SdkPubkey> = Vec::with_capacity(k);
            let mut di = 0;
            for i in 0..k {
                if i == real_index {
                    funders.push(real_funder);
                } else {
                    funders.push(decoy_funders[di]);
                    di += 1;
                }
            }

            // Predict: the leg(s) whose funder the attacker has NOT seen in
            // any prior bundle from this signer. Tie-break to the lowest
            // such index; if every funder has already been seen, fall back
            // to lowest index overall (same unbiased convention as
            // `predict_by_shared_funder`/`predict_by_history` elsewhere in
            // this harness).
            let guess = funders.iter().position(|f| !seen.contains(f)).unwrap_or(0);
            if guess == real_index {
                hits[bundle_id as usize] += 1;
            }

            // Learn from this bundle for all future ones -- the attacker
            // observes every leg's funder, not just the ones they guessed
            // correctly.
            for f in &funders {
                seen.insert(*f);
            }
        }
    }

    let base = 1.0 / k as f64;
    hits.iter()
        .map(|&h| h as f64 / trials as f64 - base)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advantage_is_deterministic_for_a_fixed_seed() {
        let a = eval_cross_bundle_subfunder_learning(32, 8, 20, 200, 7);
        let b = eval_cross_bundle_subfunder_learning(32, 8, 20, 200, 7);
        assert_eq!(a, b);
    }

    #[test]
    fn first_bundle_has_no_advantage() {
        // Nothing has been learned yet before bundle 0 -- the attacker's
        // "not seen" set is everything, so their prediction is no better
        // than the 1/K baseline (in expectation; asserted with a wide
        // tolerance since this is empirical, not the exact-zero identity
        // `mainnet_channel.rs`'s subfunder_pool test proves).
        let advantage = eval_cross_bundle_subfunder_learning(32, 8, 1, 5000, 11);
        assert!(
            advantage[0].abs() < 0.02,
            "expected ~0 advantage on the very first bundle, got {}",
            advantage[0]
        );
    }

    #[test]
    fn advantage_increases_as_more_bundles_are_observed() {
        let advantage = eval_cross_bundle_subfunder_learning(32, 8, 60, 500, 13);
        // Not asserting monotonicity at every single step (this is a noisy
        // empirical curve), just that it trends up substantially: the last
        // bundle's advantage should be well above the first's.
        assert!(
            advantage[59] > advantage[0] + 0.2,
            "expected the cross-bundle learning advantage to climb substantially by bundle 60: \
             bundle 0 = {}, bundle 59 = {}",
            advantage[0],
            advantage[59]
        );
    }

    #[test]
    fn empty_run_returns_empty() {
        let advantage = eval_cross_bundle_subfunder_learning(32, 8, 0, 10, 1);
        assert!(advantage.is_empty());
    }
}
