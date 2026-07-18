//! Destination-history channel — modeled, and quantified with vs. without a
//! companion `account-cooker`.
//!
//! The amount channel (the rest of the harness) is what `supersonic-tx` itself
//! controls. But an observer has a second channel the tool does *not* close on its
//! own: **the on-chain history of each destination.** A real payee is usually an
//! address with prior activity; a freshly-derived decoy sink has none. An attacker
//! who just asks "which of these K destinations already existed on-chain?" identifies
//! the real leg almost for free — this is, honestly, the strongest attack against the
//! tool used in isolation, and the README/threat-model say so.
//!
//! This module makes that concrete and shows the mitigation as a *number*, not a
//! promise. The companion `account-cooker` in this same bounty keeps decoy
//! destinations alive so they accumulate their own plausible history. We model two
//! regimes and measure how identifiable the real leg is from the history channel:
//!
//! * **naive** — decoys are fresh (zero history), the real payee has history. The
//!   attacker wins almost completely.
//! * **pre-warmed** — an account-cooker has given decoy destinations plausible history
//!   too, drawn from the same active-address distribution. The channel closes.
//!
//! **This is MODELED, not measured on real chain data.** History scores are synthetic
//! draws from a plausible active-address distribution. The load-bearing result is the
//! *relative* one — how much pre-warming closes the channel — not an absolute number.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;

use crate::eval::Bundle;

/// Standard normal via Box–Muller (no distributions crate).
fn standard_normal<R: Rng>(rng: &mut R) -> f64 {
    let u1: f64 = rng.gen_range(f64::MIN_POSITIVE..1.0);
    let u2: f64 = rng.gen_range(0.0..1.0);
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

/// A plausible prior-activity score for an *active* address (heavy-tailed): a
/// log-normal with median ~e^3.4 ≈ 30 prior interactions and a long tail.
fn active_history<R: Rng>(rng: &mut R) -> f64 {
    (3.4 + 1.2 * standard_normal(rng)).exp()
}

/// Attacker over the history channel: pick the destination with the most prior
/// activity (ties resolve to the lowest index — unbiased, since the real leg's
/// position is uniform).
fn predict_by_history(scores: &[f64]) -> usize {
    let mut best = 0usize;
    let mut best_s = f64::NEG_INFINITY;
    for (i, &s) in scores.iter().enumerate() {
        if s > best_s {
            best_s = s;
            best = i;
        }
    }
    best
}

/// Advantage of the history-channel attacker under both regimes, for the given
/// bundles (uses only each bundle's `K` and `real_index`).
pub fn eval_history(bundles: &[Bundle], seed: u64) -> (f64, f64) {
    if bundles.is_empty() {
        return (0.0, 0.0);
    }
    let mut rng = ChaCha20Rng::seed_from_u64(seed ^ 0xDE57_0117);
    let k = bundles[0].amounts.len();
    let (mut naive_hits, mut warm_hits) = (0usize, 0usize);

    for b in bundles {
        let kk = b.amounts.len();
        // Naive: real has history, decoys are fresh (0).
        let mut naive = vec![0.0f64; kk];
        naive[b.real_index] = active_history(&mut rng);
        if predict_by_history(&naive) == b.real_index {
            naive_hits += 1;
        }
        // Pre-warmed: every destination has history from the same distribution, so the
        // real leg is exchangeable on this channel.
        let warm: Vec<f64> = (0..kk).map(|_| active_history(&mut rng)).collect();
        if predict_by_history(&warm) == b.real_index {
            warm_hits += 1;
        }
    }

    let n = bundles.len() as f64;
    let base = 1.0 / k as f64;
    (naive_hits as f64 / n - base, warm_hits as f64 / n - base)
}
