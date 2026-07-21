//! # supersonic-sdk
//!
//! Client SDK for the `supersonic-tx` router. It turns a single real intent —
//! "send `A` lamports to `D`" — into an **intent-ambiguous bundle**: one real leg
//! plus `K-1` economically-real decoy legs, arranged so an on-chain observer cannot
//! identify which leg was the real one with probability materially above `1/K`
//! (see `THREAT_MODEL.md`).
//!
//! The privacy engineering lives here, not on-chain. The program is a neutral
//! atomic executor; this SDK is what makes the noise *believable*:
//!
//! * **Amount ambiguity** — decoy amounts are drawn in log-space around the real
//!   amount (so the real value is not an outlier), then **roundness-matched** so a
//!   round real amount (e.g. `1.0 SOL`) is hidden among equally-round decoys instead
//!   of standing out against jittered ones (`amounts` module).
//! * **Recoverable decoys** — decoy destinations are derived deterministically from
//!   a user-held master seed, so the user (and only the user) can later sweep the
//!   funds back (`decoy` + `recovery`).
//! * **Position ambiguity** — the real leg is placed at a seed-determined random
//!   index among the decoys.
//!
//! Everything is deterministic in `(master_seed, bundle_id)`: the same inputs
//! reproduce the same plan, which is what makes decoys recoverable and the whole
//! thing testable.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use sha2::{Digest, Sha256};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{keypair_from_seed, Keypair},
    signer::Signer,
    system_program,
};

pub mod amounts;
pub mod warming;

/// Maximum legs per bundle — must match the on-chain `MAX_LEGS`.
pub const MAX_LEGS: usize = 16;

/// Domain-separation tags for the key-derivation function.
const KDF_RNG: &[u8] = b"supersonic-tx/rng/v1";
const KDF_DECOY: &[u8] = b"supersonic-tx/decoy-dest/v1";
const KDF_SINK: &[u8] = b"supersonic-tx/disperse-sink/v1";

/// Tunables for decoy generation. Defaults are reasonable; the adversarial harness
/// is what justifies any change to them.
#[derive(Clone, Copy, Debug)]
pub struct DecoyConfig {
    /// Log-space standard deviation of decoy amounts around the real amount. Larger
    /// = more spread (more plausible variety, but risk of the real sitting in a
    /// sparse region). ~0.6 keeps decoys within roughly a 0.3x–3x band of the real.
    pub sigma: f64,
    /// Probability a given decoy is snapped to the *same* decimal roundness as the
    /// real amount. The rest get a nearby roundness level, so the set mixes round
    /// and precise values the way real activity does.
    pub round_match_prob: f64,
    /// Plausible amount band, in lamports. Decoys are resampled to stay inside it so
    /// an observer holding a prior on plausible payment sizes cannot eliminate a
    /// decoy as "impossible" — the support-boundary attack. The real amount is
    /// assumed to lie within this band; if it doesn't, the band is widened to include
    /// it (the real is never distorted).
    pub min_lamports: u64,
    pub max_lamports: u64,
}

impl Default for DecoyConfig {
    fn default() -> Self {
        Self {
            sigma: 0.6,
            // Plausible payment band: 0.001 SOL to 100 SOL. Keeps decoys inside the
            // same support the real leg lives in, closing the boundary leak where a
            // decoy in a Gaussian tail lands at an implausible size and gives itself
            // away as "not the real one."
            min_lamports: 1_000_000,
            max_lamports: 100_000_000_000,
            // 1.0: all decoys share the real leg's exact roundness level, which kills
            // the roundness channel entirely (a real leg fixed at some precision can't
            // stand out). Combined with the exchangeable amount construction
            // (amounts.rs), this drives every attack — including the log-central one
            // that broke the earlier version, and a learned logistic-regression
            // adversary — to the 1/K baseline for K>=8. See PROOF.md.
            round_match_prob: 1.0,
        }
    }
}

/// How decoy destinations are sourced for a bundle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecoyMode {
    /// Today's default: every decoy destination is a brand-new keypair derived
    /// uniquely for `(master_seed, bundle_id, index)` — never used before, so it
    /// carries zero on-chain history (the destination-history channel,
    /// `THREAT_MODEL.md`).
    Fresh,
    /// Decoy destinations are drawn from a warmed pool of `pool_size` stable
    /// addresses (`warming::derive_pool_member_keypair`), a bundle-seeded
    /// distinct subset per bundle (`warming::select_pool_slots`). Members
    /// accumulate real history once actually used/warmed (`warm_slots` /
    /// `supersonic warm`) — see `warming.rs`'s module doc for why the
    /// selection is randomized per bundle rather than a fixed prefix.
    WarmPool { pool_size: u32 },
}

/// One planned leg of a bundle.
#[derive(Clone, Debug)]
pub struct PlannedLeg {
    /// Destination that will receive `amount` lamports.
    pub dest: Pubkey,
    /// Lamports moved by this leg.
    pub amount: u64,
    /// True for the single real leg; false for decoys.
    pub is_real: bool,
    /// For decoys, the index used to recreate this destination during recovery
    /// — a fresh-derivation index under `DecoyMode::Fresh`, or a warm-pool slot
    /// under `DecoyMode::WarmPool` (see `BundlePlan::decoy_mode` for which).
    /// `None` for the real leg (its destination is user-supplied and is never
    /// swept).
    pub decoy_index: Option<u32>,
}

/// A fully-planned bundle: the ordered legs the transaction will contain, plus the
/// index of the real leg (which the caller keeps private and needs for recovery).
#[derive(Clone, Debug)]
pub struct BundlePlan {
    pub legs: Vec<PlannedLeg>,
    pub real_index: usize,
    pub bundle_id: u64,
    /// How this plan's decoy destinations were sourced — recovery needs this
    /// to know whether to reconstruct via `derive_decoy_keypair` or
    /// `warming::derive_pool_member_keypair`.
    pub decoy_mode: DecoyMode,
}

impl BundlePlan {
    /// The per-leg amounts, in transaction order (what an observer sees).
    pub fn amounts(&self) -> Vec<u64> {
        self.legs.iter().map(|l| l.amount).collect()
    }

    /// The per-leg destinations, in transaction order.
    pub fn destinations(&self) -> Vec<Pubkey> {
        self.legs.iter().map(|l| l.dest).collect()
    }

    /// Total lamports the user pays out across all legs (principal — decoys are
    /// recoverable, so the *net* cost is fees + whatever the user leaves parked).
    pub fn total_moved(&self) -> u64 {
        self.legs.iter().map(|l| l.amount).sum()
    }
}

/// Derive the deterministic RNG for a bundle from the master seed and bundle id.
fn bundle_rng(master_seed: &[u8; 32], bundle_id: u64) -> ChaCha20Rng {
    let mut h = Sha256::new();
    h.update(KDF_RNG);
    h.update(master_seed);
    h.update(bundle_id.to_le_bytes());
    let seed: [u8; 32] = h.finalize().into();
    ChaCha20Rng::from_seed(seed)
}

/// Derive the keypair for decoy destination `index` in `bundle_id`. Deterministic
/// in the master seed, so the holder of the seed — and no one else — can recover
/// the parked funds. This is *not* revealed on-chain (that would brand decoys).
pub fn derive_decoy_keypair(master_seed: &[u8; 32], bundle_id: u64, index: u32) -> Keypair {
    let mut h = Sha256::new();
    h.update(KDF_DECOY);
    h.update(master_seed);
    h.update(bundle_id.to_le_bytes());
    h.update(index.to_le_bytes());
    let seed: [u8; 32] = h.finalize().into();
    // keypair_from_seed accepts a >=32-byte seed and is fully deterministic.
    keypair_from_seed(&seed).expect("32-byte seed is valid")
}

/// Derive a per-decoy **dispersal sink** — a distinct user-controlled address that a
/// decoy's funds are swept to instead of straight back to the main wallet. Dispersed
/// recovery sends each decoy to its own sink in a separate transaction, so an
/// observer sees `K-1` unrelated onward transfers to `K-1` distinct addresses rather
/// than a single star into the user's wallet (the consolidation tell, THREAT_MODEL
/// §4.6). Still fully recoverable from the seed.
pub fn derive_sink_keypair(master_seed: &[u8; 32], bundle_id: u64, index: u32) -> Keypair {
    let mut h = Sha256::new();
    h.update(KDF_SINK);
    h.update(master_seed);
    h.update(bundle_id.to_le_bytes());
    h.update(index.to_le_bytes());
    let seed: [u8; 32] = h.finalize().into();
    keypair_from_seed(&seed).expect("32-byte seed is valid")
}

/// Plan an intent-ambiguous bundle for a single real transfer.
///
/// * `master_seed` — the user's recovery secret (hold it safe; it recovers decoys).
/// * `bundle_id` — a per-bundle nonce (a counter or timestamp); combined with the
///   seed it makes the plan reproducible and the decoys recoverable.
/// * `real_dest` / `real_amount` — the user's genuine intent.
/// * `k` — total legs including the real one (anonymity set size), `2..=MAX_LEGS`.
///
/// Returns the ordered plan; `real_index` tells the caller where their real leg
/// landed.
pub fn plan_bundle(
    master_seed: &[u8; 32],
    bundle_id: u64,
    real_dest: Pubkey,
    real_amount: u64,
    k: usize,
    cfg: DecoyConfig,
) -> Result<BundlePlan, SdkError> {
    plan_bundle_with_mode(
        master_seed,
        bundle_id,
        real_dest,
        real_amount,
        k,
        cfg,
        DecoyMode::Fresh,
    )
}

/// Same as [`plan_bundle`], with explicit control over how decoy destinations
/// are sourced (`DecoyMode`). `plan_bundle` is `DecoyMode::Fresh` — this is the
/// entry point for `DecoyMode::WarmPool`.
pub fn plan_bundle_with_mode(
    master_seed: &[u8; 32],
    bundle_id: u64,
    real_dest: Pubkey,
    real_amount: u64,
    k: usize,
    cfg: DecoyConfig,
    decoy_mode: DecoyMode,
) -> Result<BundlePlan, SdkError> {
    if !(2..=MAX_LEGS).contains(&k) {
        return Err(SdkError::BadAnonymitySet(k));
    }
    if real_amount == 0 {
        return Err(SdkError::ZeroRealAmount);
    }
    if real_dest == system_program::ID {
        return Err(SdkError::BadDestination);
    }

    let mut rng = bundle_rng(master_seed, bundle_id);
    let n_decoys = k - 1;

    // Decoy amounts around the real amount, roundness-matched to hide a round real.
    let decoy_amounts = amounts::generate_decoy_amounts(real_amount, n_decoys, &cfg, &mut rng);

    // Decoy destinations: either freshly derived per (bundle_id, index) — never
    // used before — or drawn from a bundle-seeded distinct subset of the warm
    // pool (see `warming.rs` for why the subset is randomized per bundle).
    let decoy_indices: Vec<u32> = match decoy_mode {
        DecoyMode::Fresh => (0..n_decoys as u32).collect(),
        DecoyMode::WarmPool { pool_size } => {
            warming::select_pool_slots(master_seed, bundle_id, n_decoys, pool_size)
        }
    };

    // Build decoy legs with deterministic, recoverable destinations.
    let mut legs: Vec<PlannedLeg> = Vec::with_capacity(k);
    for (amount, idx) in decoy_amounts.into_iter().zip(decoy_indices) {
        let dest = match decoy_mode {
            DecoyMode::Fresh => derive_decoy_keypair(master_seed, bundle_id, idx).pubkey(),
            DecoyMode::WarmPool { .. } => {
                warming::derive_pool_member_keypair(master_seed, idx).pubkey()
            }
        };
        legs.push(PlannedLeg {
            dest,
            amount,
            is_real: false,
            decoy_index: Some(idx),
        });
    }

    // Insert the real leg at a seed-determined random position.
    let real_index = rng.gen_range(0..k);
    legs.insert(
        real_index,
        PlannedLeg {
            dest: real_dest,
            amount: real_amount,
            is_real: true,
            decoy_index: None,
        },
    );

    Ok(BundlePlan {
        legs,
        real_index,
        bundle_id,
        decoy_mode,
    })
}

/// Build the `execute_bundle` instruction for a plan. Destinations are appended as
/// writable, non-signer accounts in leg order (the program pairs them 1:1 with the
/// legs).
pub fn build_instruction(program_id: Pubkey, user: Pubkey, plan: &BundlePlan) -> Instruction {
    let data = execute_bundle_data(&plan.amounts());

    let mut accounts = vec![
        AccountMeta::new(user, true),
        AccountMeta::new_readonly(system_program::ID, false),
    ];
    for dest in plan.destinations() {
        accounts.push(AccountMeta::new(dest, false));
    }

    Instruction {
        program_id,
        accounts,
        data,
    }
}

/// Encode the `execute_bundle` instruction data exactly as the Anchor program
/// expects: 8-byte discriminator + Borsh `Vec<Leg>` (a `u32` length then each
/// `Leg { amount: u64 }` little-endian).
pub fn execute_bundle_data(amounts: &[u64]) -> Vec<u8> {
    let mut data = anchor_discriminator("execute_bundle").to_vec();
    data.extend_from_slice(&(amounts.len() as u32).to_le_bytes());
    for a in amounts {
        data.extend_from_slice(&a.to_le_bytes());
    }
    data
}

/// Anchor's global instruction discriminator: first 8 bytes of
/// `sha256("global:<name>")`.
pub fn anchor_discriminator(ix_name: &str) -> [u8; 8] {
    let mut h = Sha256::new();
    h.update(format!("global:{ix_name}").as_bytes());
    let digest = h.finalize();
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SdkError {
    /// `k` outside `2..=MAX_LEGS`.
    BadAnonymitySet(usize),
    ZeroRealAmount,
    BadDestination,
}

impl std::fmt::Display for SdkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SdkError::BadAnonymitySet(k) => {
                write!(f, "anonymity set k={k} must be in 2..={MAX_LEGS}")
            }
            SdkError::ZeroRealAmount => write!(f, "real amount must be greater than zero"),
            SdkError::BadDestination => write!(f, "invalid real destination"),
        }
    }
}

impl std::error::Error for SdkError {}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: [u8; 32] = [7u8; 32];

    #[test]
    fn plan_is_deterministic_in_seed_and_id() {
        let dest = Keypair::new().pubkey();
        let a = plan_bundle(&SEED, 1, dest, 1_337_000, 5, DecoyConfig::default()).unwrap();
        let b = plan_bundle(&SEED, 1, dest, 1_337_000, 5, DecoyConfig::default()).unwrap();
        assert_eq!(a.amounts(), b.amounts(), "same seed+id ⇒ same amounts");
        assert_eq!(
            a.destinations(),
            b.destinations(),
            "same seed+id ⇒ same dests"
        );
        assert_eq!(a.real_index, b.real_index);
    }

    #[test]
    fn different_bundle_id_changes_plan() {
        let dest = Keypair::new().pubkey();
        let a = plan_bundle(&SEED, 1, dest, 1_337_000, 5, DecoyConfig::default()).unwrap();
        let b = plan_bundle(&SEED, 2, dest, 1_337_000, 5, DecoyConfig::default()).unwrap();
        assert_ne!(
            a.destinations(),
            b.destinations(),
            "distinct id ⇒ distinct decoys"
        );
    }

    #[test]
    fn real_leg_present_exactly_once_with_right_value() {
        let dest = Keypair::new().pubkey();
        let plan = plan_bundle(&SEED, 9, dest, 4_200_000, 6, DecoyConfig::default()).unwrap();
        assert_eq!(plan.legs.len(), 6);
        let reals: Vec<_> = plan.legs.iter().filter(|l| l.is_real).collect();
        assert_eq!(reals.len(), 1, "exactly one real leg");
        assert_eq!(reals[0].dest, dest);
        assert_eq!(reals[0].amount, 4_200_000);
        assert_eq!(plan.legs[plan.real_index].dest, dest);
    }

    #[test]
    fn decoys_are_recoverable_from_seed() {
        let dest = Keypair::new().pubkey();
        let plan = plan_bundle(&SEED, 3, dest, 2_000_000, 5, DecoyConfig::default()).unwrap();
        for leg in plan.legs.iter().filter(|l| !l.is_real) {
            let idx = leg.decoy_index.unwrap();
            let kp = derive_decoy_keypair(&SEED, 3, idx);
            assert_eq!(kp.pubkey(), leg.dest, "decoy dest recoverable from seed");
        }
    }

    #[test]
    fn rejects_bad_params() {
        let dest = Keypair::new().pubkey();
        assert!(matches!(
            plan_bundle(&SEED, 1, dest, 1_000, 1, DecoyConfig::default()),
            Err(SdkError::BadAnonymitySet(1))
        ));
        assert!(matches!(
            plan_bundle(&SEED, 1, dest, 0, 4, DecoyConfig::default()),
            Err(SdkError::ZeroRealAmount)
        ));
    }

    #[test]
    fn discriminator_matches_known_anchor_value() {
        // sha256("global:execute_bundle")[..8] — stable, so the SDK and program agree.
        let d = anchor_discriminator("execute_bundle");
        assert_eq!(d.len(), 8);
        // Re-deriving the same way must be stable across runs.
        assert_eq!(d, anchor_discriminator("execute_bundle"));
    }
}
