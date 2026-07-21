//! Generalized "sample real mainnet feature → bootstrap-resample → report
//! advantage vs 1/K" measurement, shared by three channels: destination-history,
//! funding-graph, and token-holdings. Same shape as `destination.rs`'s
//! `eval_history_measured`, generalized over which numeric feature is scored, and
//! upgraded on one methodological point `destination.rs` doesn't have: evaluation
//! draws *only* from the `held_out` partition of `MainnetFixture` — the
//! `calibration` partition exists so a future fitting mechanism (an SDK-side
//! warming pool, say) has data to tune against that is disjoint from the data
//! used to *measure* it. Reusing one pool for both fitting and evaluation is a
//! known way to inflate an apparent closure that doesn't generalize; this keeps
//! the two roles separate from the start, at collection time, before either is
//! used (see `collect_mainnet_profiles.rs` and `mainnet_fixture.rs`).
//!
//! Each channel differs only in `score`: what numeric feature the attacker reads
//! off a destination. The attacker itself (`predict_by_history` from
//! `destination.rs`, reused here) is the same "pick the highest score" — honest
//! about being a simple, adversary-favorable baseline, not a tuned classifier;
//! the point of every channel here is the *feature*, not a fancier attacker.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;

use crate::destination::predict_by_history;
use crate::eval::Bundle;
use crate::mainnet_fixture::{MainnetFixture, MainnetProfile};

fn bootstrap_one<R: Rng>(
    rng: &mut R,
    pool: &[&MainnetProfile],
    score: impl Fn(&MainnetProfile) -> f64,
) -> f64 {
    let idx = rng.gen_range(0..pool.len());
    score(pool[idx])
}

/// Naive-vs-warm advantage for a single scored channel, evaluated only on the
/// `held_out` partition of `fixture`. `salt` decorrelates the RNG stream across
/// channels sharing the same `seed` (so K=2/4/8/16 draws for destination-history
/// don't reuse the exact same random path as funding-graph or token-holdings).
pub fn eval_channel<F: Fn(&MainnetProfile) -> f64>(
    bundles: &[Bundle],
    seed: u64,
    salt: u64,
    fixture: &MainnetFixture,
    score: F,
) -> (f64, f64) {
    let aged = fixture.aged_held_out();
    let fresh = fixture.fresh_held_out();
    if bundles.is_empty() || aged.is_empty() || fresh.is_empty() {
        return (0.0, 0.0);
    }
    let mut rng = ChaCha20Rng::seed_from_u64(seed ^ salt);
    let k = bundles[0].amounts.len();
    let (mut naive_hits, mut warm_hits) = (0usize, 0usize);

    for b in bundles {
        let kk = b.amounts.len();
        let mut naive = vec![0.0f64; kk];
        for (i, s) in naive.iter_mut().enumerate() {
            *s = if i == b.real_index {
                bootstrap_one(&mut rng, &aged, &score)
            } else {
                bootstrap_one(&mut rng, &fresh, &score)
            };
        }
        if predict_by_history(&naive) == b.real_index {
            naive_hits += 1;
        }
        let warm: Vec<f64> = (0..kk)
            .map(|_| bootstrap_one(&mut rng, &aged, &score))
            .collect();
        if predict_by_history(&warm) == b.real_index {
            warm_hits += 1;
        }
    }

    let n = bundles.len() as f64;
    let base = 1.0 / k as f64;
    (naive_hits as f64 / n - base, warm_hits as f64 / n - base)
}

/// Destination-history, on real mainnet data (n = aged.len() held-out, see
/// `PROOF.md`) — the mainnet-scale counterpart to `destination::eval_history_measured`'s
/// devnet fixture, with the honest held-out split described above.
pub fn eval_mainnet_destination_history(
    bundles: &[Bundle],
    seed: u64,
    fixture: &MainnetFixture,
) -> (f64, f64) {
    eval_channel(bundles, seed, 0x4D4E_5F44_4831, fixture, |p| {
        p.signature_count_lower_bound as f64
    })
}

/// Funding-graph: does this destination have a plausible external funder, vs a
/// decoy with none found. Scored from `distinct_funders_lower_bound`, which this
/// collector's single-page proxy makes effectively binary (see
/// `mainnet_fixture.rs`) — a coarser signal than a full funding-graph parse would
/// give, stated as such, not smoothed over.
pub fn eval_mainnet_funding_graph(
    bundles: &[Bundle],
    seed: u64,
    fixture: &MainnetFixture,
) -> (f64, f64) {
    eval_channel(bundles, seed, 0x4D4E_5F46_4732, fixture, |p| {
        p.distinct_funders_lower_bound as f64
    })
}

/// Token-holdings: does this destination already hold SPL/Token-2022 accounts,
/// vs a freshly-derived decoy holding none.
pub fn eval_mainnet_token_holdings(
    bundles: &[Bundle],
    seed: u64,
    fixture: &MainnetFixture,
) -> (f64, f64) {
    eval_channel(bundles, seed, 0x4D4E_5F54_4B33, fixture, |p| {
        p.token_account_count as f64
    })
}
