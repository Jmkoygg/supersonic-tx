//! Destination-history channel — quantified with vs. without a companion
//! `account-cooker`, both as a synthetic model (below) and as a real measurement
//! against devnet (`eval_history_measured`, see `history_fixture.rs`).
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
//! destinations alive so they accumulate their own plausible history. Two regimes,
//! measure how identifiable the real leg is from the history channel:
//!
//! * **naive** — decoys are fresh (zero history), the real payee has history. The
//!   attacker wins almost completely.
//! * **pre-warmed** — an account-cooker has given decoy destinations plausible history
//!   too, drawn from the same active-address distribution. The channel closes.
//!
//! `eval_history_modeled` draws both regimes from a synthetic log-normal — kept for
//! regression comparison. `eval_history_measured` draws the same two regimes from
//! `HistoryFixture` — a real, third-party-verifiable devnet sample (see
//! `harness/fixtures/devnet_history.json` and its `collected_at`/pubkeys), via
//! bootstrap resampling (sampling with replacement from the real observed values,
//! not fitting/assuming a distribution shape). No account-cooker exists yet to
//! integrate with directly (external dependency, out of this project's control), so
//! the same self-collected `aged` pool stands in for both "a real payee with prior
//! activity" and "an account-cooker-warmed decoy" — the two roles it would fill.
//!
//! **What the pre-warmed/measured result does and does not establish.** In the
//! pre-warmed regime every leg draws from the *same* pool, so exchangeability — and
//! therefore an advantage of ~0 — follows from the sampling construction itself, not
//! from a property discovered in the devnet data; that part would hold for any i.i.d.
//! pool, real or synthetic. What the real fixture adds is *provenance*: the specific
//! counts (5–25 real confirmed transactions per address, independently re-queryable)
//! are genuine, not invented, so the claim "the number itself isn't fabricated" is
//! checkable. It does **not** validate that a real account-cooker's warming pattern
//! (funding graph shape, timing, who pays for it) is itself indistinguishable from
//! organic activity — `signature_count` is a single scalar, and a real account-cooker
//! integration remains a stated next step, not something this measurement covers.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;

use crate::eval::Bundle;
use crate::history_fixture::HistoryFixture;

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
/// bundles (uses only each bundle's `K` and `real_index`) — synthetic model, kept as
/// a regression reference. See `eval_history_measured` for the real-data version.
pub fn eval_history_modeled(bundles: &[Bundle], seed: u64) -> (f64, f64) {
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

/// Bootstrap-sample one `signature_count` from a real fixture pool (sampling with
/// replacement — legitimate for estimating the attacker's advantage under the
/// pool's real empirical distribution without assuming its shape).
fn bootstrap_one<R: Rng>(rng: &mut R, pool: &[crate::history_fixture::AddressSample]) -> f64 {
    let idx = rng.gen_range(0..pool.len());
    pool[idx].signature_count as f64
}

/// Same two regimes as `eval_history_modeled`, but every score is a bootstrap draw
/// from `fixture` — real `getSignaturesForAddress` counts collected against devnet,
/// not a synthetic distribution. `fixture.fresh_verified_zero` backs the naive
/// regime's decoys (confirmed-empty, not merely assumed empty); `fixture.aged` backs
/// both the naive regime's real payee and the pre-warmed regime's decoys+payee.
pub fn eval_history_measured(
    bundles: &[Bundle],
    seed: u64,
    fixture: &HistoryFixture,
) -> (f64, f64) {
    if bundles.is_empty() || fixture.aged.is_empty() || fixture.fresh_verified_zero.is_empty() {
        return (0.0, 0.0);
    }
    let mut rng = ChaCha20Rng::seed_from_u64(seed ^ 0x6D_EA5_0117);
    let k = bundles[0].amounts.len();
    let (mut naive_hits, mut warm_hits) = (0usize, 0usize);

    for b in bundles {
        let kk = b.amounts.len();
        // Naive: real payee's score is a real "aged" sample; decoys are real
        // confirmed-zero fresh samples.
        let mut naive = vec![0.0f64; kk];
        for (i, s) in naive.iter_mut().enumerate() {
            *s = if i == b.real_index {
                bootstrap_one(&mut rng, &fixture.aged)
            } else {
                bootstrap_one(&mut rng, &fixture.fresh_verified_zero)
            };
        }
        if predict_by_history(&naive) == b.real_index {
            naive_hits += 1;
        }
        // Pre-warmed: every destination's score is a real "aged" sample.
        let warm: Vec<f64> = (0..kk)
            .map(|_| bootstrap_one(&mut rng, &fixture.aged))
            .collect();
        if predict_by_history(&warm) == b.real_index {
            warm_hits += 1;
        }
    }

    let n = bundles.len() as f64;
    let base = 1.0 / k as f64;
    (naive_hits as f64 / n - base, warm_hits as f64 / n - base)
}
