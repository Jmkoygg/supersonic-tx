//! Warm-pool decoy destinations — the SDK-side mechanism that lets a decoy
//! destination carry real prior on-chain activity instead of being a
//! zero-history fresh keypair, closing (not just measuring) the
//! destination-history channel (`THREAT_MODEL.md`, `PROOF.md §3e`,
//! `harness/src/mainnet_channel.rs`).
//!
//! **What this deliberately does differently from a naive "reuse N warmed
//! addresses" pool.** An earlier design considered for this pool derived a
//! member from `(master_seed, slot)` alone and always used the same first
//! `K-1` slots as decoys for every bundle. That has a real, undocumented leak:
//! the same handful of addresses would then appear as a decoy leg across many
//! different bundles from the same signer, and an attacker who groups bundles
//! by fee-payer gains a trivial classifier — "this destination already
//! appeared in another bundle from this signer → decoy; appeared exactly
//! once → candidate real leg." Longitudinal decoy-reuse leaks are a known
//! failure mode in ring-signature-style privacy schemes (see e.g. Monero's
//! own history of ring-member reuse attacks). This module avoids it two ways:
//!
//! 1. **A pool larger than any single bundle's decoy count**, so a given
//!    bundle only ever touches a small, bundle-seeded *subset* of the pool,
//!    not the same fixed prefix every time.
//! 2. **A distinct, deterministically-seeded random subset per bundle**
//!    (`select_pool_slots`, a partial Fisher–Yates keyed on `bundle_id`), so
//!    which members appear together varies bundle to bundle. Any given member
//!    can still recur across bundles over time — that's inherent to "warmed"
//!    addresses being reused rather than minted fresh per bundle — but which
//!    *other* members it's grouped with, and how often it recurs relative to
//!    the pool size, is what an attacker would need to observe over many
//!    bundles to build confidence, not a single reused fixed set.
//!
//! **What this does not claim.** A pool member only carries plausible history
//! once it has actually been used — `warm_slots` (SDK) / `supersonic warm`
//! (CLI) does that with real, small, self-funded round-trip transactions,
//! same construction as `harness/src/bin/collect_mainnet_profiles.rs`'s
//! aged-address methodology, not synthesized. Before warming, a fresh pool
//! behaves identically to today's per-bundle fresh derivation — this is an
//! opt-in mechanism (`DecoyMode::WarmPool`), not a silent behavior change.

use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use sha2::{Digest, Sha256};
use solana_sdk::signature::{keypair_from_seed, Keypair};

const KDF_POOL: &[u8] = b"supersonic-tx/warm-pool/v1";
const KDF_POOL_SELECT: &[u8] = b"supersonic-tx/warm-pool-select/v1";

/// Derive the stable keypair for warm-pool slot `slot` (independent of
/// `bundle_id` — this is what lets it be reused/warmed across many bundles).
/// Deterministic in `master_seed`, so recovery needs only the seed and slot
/// index, same as `derive_decoy_keypair`.
pub fn derive_pool_member_keypair(master_seed: &[u8; 32], slot: u32) -> Keypair {
    let mut h = Sha256::new();
    h.update(KDF_POOL);
    h.update(master_seed);
    h.update(slot.to_le_bytes());
    let seed: [u8; 32] = h.finalize().into();
    keypair_from_seed(&seed).expect("32-byte seed is valid")
}

/// Deterministically select `n` **distinct** slot indices out of
/// `0..pool_size` for this specific `bundle_id`, via a bundle-seeded partial
/// Fisher–Yates shuffle. Two different `bundle_id`s over the same pool
/// produce different subsets (and typically different groupings) with high
/// probability whenever `pool_size` is meaningfully larger than `n`.
///
/// Panics if `n > pool_size` (a pool too small to serve this bundle's decoy
/// count — a configuration error, not a runtime condition to paper over).
pub fn select_pool_slots(
    master_seed: &[u8; 32],
    bundle_id: u64,
    n: usize,
    pool_size: u32,
) -> Vec<u32> {
    assert!(
        n <= pool_size as usize,
        "warm pool of size {pool_size} cannot serve {n} decoys — grow the pool or lower K"
    );
    let mut h = Sha256::new();
    h.update(KDF_POOL_SELECT);
    h.update(master_seed);
    h.update(bundle_id.to_le_bytes());
    let seed: [u8; 32] = h.finalize().into();
    let mut rng = ChaCha20Rng::from_seed(seed);

    // Partial Fisher–Yates: shuffle only as far as needed to pick n elements.
    let mut pool: Vec<u32> = (0..pool_size).collect();
    let len = pool.len();
    for i in 0..n.min(len) {
        let j = rng.gen_range(i..len);
        pool.swap(i, j);
    }
    pool.truncate(n);
    pool
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::signer::Signer;
    use std::collections::HashSet;

    const SEED: [u8; 32] = [11u8; 32];

    #[test]
    fn slot_derivation_is_deterministic_and_bundle_independent() {
        let a = derive_pool_member_keypair(&SEED, 5);
        let b = derive_pool_member_keypair(&SEED, 5);
        assert_eq!(a.pubkey(), b.pubkey());
    }

    #[test]
    fn distinct_slots_give_distinct_keys() {
        let a = derive_pool_member_keypair(&SEED, 0);
        let b = derive_pool_member_keypair(&SEED, 1);
        assert_ne!(a.pubkey(), b.pubkey());
    }

    #[test]
    fn selection_is_deterministic_in_bundle_id() {
        let a = select_pool_slots(&SEED, 42, 6, 32);
        let b = select_pool_slots(&SEED, 42, 6, 32);
        assert_eq!(a, b);
    }

    #[test]
    fn selection_has_no_duplicates_within_a_bundle() {
        for bundle_id in 0..50u64 {
            let slots = select_pool_slots(&SEED, bundle_id, 15, 32);
            let unique: HashSet<u32> = slots.iter().copied().collect();
            assert_eq!(
                unique.len(),
                slots.len(),
                "bundle {bundle_id} picked a duplicate slot"
            );
        }
    }

    /// The property this module exists to fix: across many bundles, the
    /// selected subset must *not* collapse onto the same fixed group every
    /// time (the flaw found in a competing implementation). We check this by
    /// counting, over many bundles, how often each pool slot is chosen, and
    /// asserting the distribution isn't concentrated on a small fixed prefix.
    #[test]
    fn selection_varies_across_bundles_not_a_fixed_prefix() {
        const POOL_SIZE: u32 = 32;
        const N: usize = 6;
        const BUNDLES: u64 = 500;

        let mut counts = vec![0u32; POOL_SIZE as usize];
        let mut distinct_subsets: HashSet<Vec<u32>> = HashSet::new();
        for bundle_id in 0..BUNDLES {
            let mut slots = select_pool_slots(&SEED, bundle_id, N, POOL_SIZE);
            for &s in &slots {
                counts[s as usize] += 1;
            }
            slots.sort_unstable();
            distinct_subsets.insert(slots);
        }

        // If this always picked the same first N slots (the flaw being fixed),
        // there would be exactly 1 distinct subset ever. Require meaningfully
        // more variety than that.
        assert!(
            distinct_subsets.len() > BUNDLES as usize / 2,
            "only {} distinct subsets across {BUNDLES} bundles — selection is too repetitive",
            distinct_subsets.len()
        );

        // Every slot should get picked a non-trivial share of the time — no
        // slot permanently idle, no slot permanently dominant. Expected count
        // per slot ~= BUNDLES * N / POOL_SIZE; allow a generous band since
        // this is a randomized property test, not an exact-uniformity check.
        let expected = BUNDLES as f64 * N as f64 / POOL_SIZE as f64;
        for (slot, &c) in counts.iter().enumerate() {
            assert!(
                (c as f64) > expected * 0.3,
                "slot {slot} picked only {c} times (expected ~{expected:.0}) — too idle"
            );
        }
    }
}
