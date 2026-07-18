//! Property-based tests for the bundle generator (proptest).
//!
//! The unit tests in `lib.rs`/`amounts.rs` check specific examples. These check the
//! load-bearing invariants for *arbitrary* inputs — any master seed, any bundle id,
//! any real amount across five orders of magnitude, any anonymity set 2..=16. If the
//! generator can violate one of these for *some* input, proptest shrinks to the
//! minimal counterexample. For a privacy tool whose whole pitch is "the real leg is
//! exactly one exchangeable sample," these are exactly the guarantees that must hold
//! universally, not just on the examples we happened to pick.

use proptest::prelude::*;
use solana_sdk::{pubkey::Pubkey, signer::Signer};
use supersonic_sdk::{derive_decoy_keypair, plan_bundle, DecoyConfig};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// For any valid input, the plan is well-formed: exactly K legs, exactly one real
    /// leg carrying the exact intent, and every amount strictly positive.
    #[test]
    fn plan_is_well_formed(
        seed in any::<[u8; 32]>(),
        bundle_id in any::<u64>(),
        real_amount in 1u64..=1_000_000_000_000,
        k in 2usize..=16,
        dest_bytes in any::<[u8; 32]>(),
    ) {
        let real_dest = Pubkey::new_from_array(dest_bytes);
        let plan = plan_bundle(&seed, bundle_id, real_dest, real_amount, k, DecoyConfig::default())
            .expect("valid params must plan");

        prop_assert_eq!(plan.legs.len(), k);

        let reals: Vec<_> = plan.legs.iter().filter(|l| l.is_real).collect();
        prop_assert_eq!(reals.len(), 1, "exactly one real leg");
        prop_assert_eq!(reals[0].dest, real_dest);
        prop_assert_eq!(reals[0].amount, real_amount, "real amount is never distorted");
        prop_assert_eq!(plan.real_index, plan.legs.iter().position(|l| l.is_real).unwrap());

        prop_assert!(plan.legs.iter().all(|l| l.amount > 0), "no zero-value leg");
    }

    /// Every decoy destination is recoverable from the master seed, and the decoy
    /// indices are exactly 0..K-1 with no gaps or repeats. This is the invariant the
    /// whole recovery path depends on — lose it and a user's parked funds are gone.
    #[test]
    fn decoys_are_fully_recoverable(
        seed in any::<[u8; 32]>(),
        bundle_id in any::<u64>(),
        real_amount in 1u64..=1_000_000_000_000,
        k in 2usize..=16,
        dest_bytes in any::<[u8; 32]>(),
    ) {
        let real_dest = Pubkey::new_from_array(dest_bytes);
        let plan = plan_bundle(&seed, bundle_id, real_dest, real_amount, k, DecoyConfig::default())
            .unwrap();

        let mut seen = vec![false; k - 1];
        for leg in plan.legs.iter().filter(|l| !l.is_real) {
            let idx = leg.decoy_index.expect("decoy has a derivation index") as usize;
            prop_assert!(idx < k - 1, "decoy index in range");
            prop_assert!(!seen[idx], "no repeated decoy index");
            seen[idx] = true;
            let kp = derive_decoy_keypair(&seed, bundle_id, idx as u32);
            prop_assert_eq!(kp.pubkey(), leg.dest, "decoy dest recoverable from seed");
        }
        prop_assert!(seen.iter().all(|&s| s), "every decoy index 0..k-1 used exactly once");
    }

    /// Every decoy amount stays inside the plausible band (widened to include the real
    /// amount). This is the support-boundary guarantee: no decoy lands at an
    /// implausible size that would give itself away.
    #[test]
    fn decoys_stay_in_plausible_band(
        seed in any::<[u8; 32]>(),
        bundle_id in any::<u64>(),
        real_amount in 1u64..=1_000_000_000_000,
        k in 2usize..=16,
        dest_bytes in any::<[u8; 32]>(),
    ) {
        let real_dest = Pubkey::new_from_array(dest_bytes);
        let cfg = DecoyConfig::default();
        let plan = plan_bundle(&seed, bundle_id, real_dest, real_amount, k, cfg).unwrap();

        let lo = cfg.min_lamports.min(real_amount).max(1);
        let hi = cfg.max_lamports.max(real_amount);
        for leg in plan.legs.iter().filter(|l| !l.is_real) {
            prop_assert!(leg.amount >= lo && leg.amount <= hi,
                "decoy {} outside band [{}, {}]", leg.amount, lo, hi);
        }
    }

    /// The plan is a pure function of (master_seed, bundle_id, intent, k): identical
    /// inputs produce byte-identical bundles. This is what makes decoys recoverable
    /// and the whole system reproducible.
    #[test]
    fn plan_is_deterministic(
        seed in any::<[u8; 32]>(),
        bundle_id in any::<u64>(),
        real_amount in 1u64..=1_000_000_000_000,
        k in 2usize..=16,
        dest_bytes in any::<[u8; 32]>(),
    ) {
        let real_dest = Pubkey::new_from_array(dest_bytes);
        let a = plan_bundle(&seed, bundle_id, real_dest, real_amount, k, DecoyConfig::default()).unwrap();
        let b = plan_bundle(&seed, bundle_id, real_dest, real_amount, k, DecoyConfig::default()).unwrap();
        prop_assert_eq!(a.amounts(), b.amounts());
        prop_assert_eq!(a.destinations(), b.destinations());
        prop_assert_eq!(a.real_index, b.real_index);
    }
}
