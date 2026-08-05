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
//!
//! `eval_mainnet_funding_graph` above is an idealized ceiling for the
//! funding-graph channel — it does not model what the shipped `warm_pool`
//! mechanism actually produces (every decoy slot funded by the same wallet).
//! `eval_mainnet_funding_graph_shipped_mechanism`, below, measures that real
//! mechanism directly with its own attacker (`predict_by_shared_funder`), and is
//! kept separate from `eval_channel`'s generic bootstrap shape because the
//! shipped mechanism isn't a bootstrap-resampled numeric feature — it's a
//! same-wallet-vs-real-funder identity comparison.
//!
//! **`eval_channel`'s warm regime is construction-modeled, not selector-run —
//! stated plainly, not left implied.** The warm column for destination-history
//! and token-holdings draws every leg (real and decoy) i.i.d. from the same
//! `aged` pool each bundle; it does not route decoy selection through the
//! actual shipped `supersonic_sdk::warming::select_pool_slots` /
//! `derive_pool_member_keypair`. Wiring it through the real selector was
//! attempted and reverted: giving each pool slot a fixed persistent feature
//! value (as a real warmed slot would have) and letting `select_pool_slots`
//! decide which bundles draw it either (a) made the reported advantage swing
//! wildly and seed-dependent when the fixed value was a single bootstrap draw
//! (an outlier slot dominates `predict_by_history`'s max-pick whenever
//! selected — measured: -0.15 to +0.02 across seeds 1/2/3/42 at K=2 for
//! token-holdings, nothing like this project's usual "holds across 4
//! independent seeds" bar), or (b) introduced a systematic bias — not noise,
//! a consistent, wrong-shaped result — when the fixed value was smoothed by
//! averaging multiple draws: decoys clustering tightly around the pool mean
//! while the real leg stays a single high-variance draw breaks the symmetric
//! K-way comparison `predict_by_history` assumes, measured to make accuracy
//! roughly constant across K instead of scaling with `1/K` (e.g. destination-
//! history "warm" advantage climbing to +0.6 at K=16, a large *reported* leak
//! that was a measurement artifact, not a real property of the mechanism).
//! Getting this right needs either running the K-1 decoys AND the real leg
//! through the same persistence discipline (the real leg is a genuine one-off
//! payee, so that doesn't apply), or averaging the whole measurement over many
//! independent pool realizations (`cross_bundle.rs`'s `trials` pattern) rather
//! than one fixed `pool_features` draw per harness run — real, but more work
//! than fits safely under this fix. Tracked as an open item rather than
//! shipped half-verified; see `SECURITY.md`.

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
/// give, stated as such, not smoothed over. In the current fixture it is in fact
/// constant (`1`) across every `aged` entry — zero variance, not just low
/// resolution — so this channel's numbers below are a modeled ceiling from a
/// non-discriminating proxy, not output from a working classifier.
///
/// **This is an IDEALIZED CEILING, not the shipped mechanism's real residual.** It
/// answers "what would an attacker see if the pool had genuinely diverse funders" —
/// the `warm_pool` mechanism `supersonic-tx` actually ships does not do that (every
/// slot is funded by the same wallet). For the real, honest residual of the
/// mechanism as shipped, see `eval_mainnet_funding_graph_shipped_mechanism` below.
pub fn eval_mainnet_funding_graph(
    bundles: &[Bundle],
    seed: u64,
    fixture: &MainnetFixture,
) -> (f64, f64) {
    eval_channel(bundles, seed, 0x4D4E_5F46_4732, fixture, |p| {
        p.distinct_funders_lower_bound as f64
    })
}

/// Deploy/dev wallet used as fee-payer for every `warm_pool` slot
/// (`cli/src/main.rs::warm_pool`, `payer.pubkey()` funds each slot's fund-in
/// transfer) — see `PROOF.md`/`SECURITY.md` for provenance and independent
/// verification. This is not a stand-in constant; it is the literal wallet the
/// shipped CLI uses today.
pub const WARM_POOL_FUNDER_PUBKEY: &str = "25NhgSgz97LKxPbD8usUcnH7AcUbuV6P2Qmc3Ayhbaee";

/// Same-funder-clustering attacker: given the K legs' disclosed funders, pick the
/// index whose funder value is the *least* common among the K (i.e. "the one that
/// differs from the majority"). Ties (e.g. every funder distinct, or no unique
/// minimum) resolve to the lowest index — the same unbiased tie-break convention
/// `predict_by_history` uses elsewhere in this harness, since the real leg's
/// position is uniformly random and shouldn't be exploitable on its own.
pub(crate) fn predict_by_shared_funder(funders: &[&str]) -> usize {
    let mut best = 0usize;
    let mut best_count = usize::MAX;
    for (i, f) in funders.iter().enumerate() {
        let count = funders.iter().filter(|&&g| g == *f).count();
        if count < best_count {
            best_count = count;
            best = i;
        }
    }
    best
}

/// The REAL, honestly-measured residual of the SHIPPED `warm_pool` mechanism
/// against the funding-graph channel — as opposed to `eval_mainnet_funding_graph`
/// above, which is an idealized ceiling from a non-discriminating proxy.
///
/// Models the attack precisely as the shipped mechanism actually produces it: for
/// each synthetic K-leg bundle, one index is the real leg and is assigned a real
/// historical funder drawn from `fixture`'s `held_out` aged pool's
/// `earliest_funder_proxy` (a real mainnet payee's real prior funder). The other
/// K-1 legs are decoys as `--decoy-mode warm-pool` actually produces them: every
/// one funded by the literal same wallet, `WARM_POOL_FUNDER_PUBKEY`
/// (`cli/src/main.rs::warm_pool`'s `payer.pubkey()`). The attacker
/// (`predict_by_shared_funder`) needs no numeric proxy or trained classifier — it
/// just asks "which of these K funders differs from the rest", something anyone
/// with `getSignaturesForAddress`/`getTransaction` access on the K legs can do
/// today for free. This is a single regime (there is no "naive" vs "warm" split
/// here — this *is* what shipped `warm-pool` bundles look like on this channel
/// today), so it returns one advantage value, not a pair.
///
/// Returns `0.0` (no measurement) if `bundles` is empty or the `held_out` aged
/// pool has no entries with a known `earliest_funder_proxy` — mirrors
/// `eval_channel`'s empty-pool convention above rather than panicking.
pub fn eval_mainnet_funding_graph_shipped_mechanism(
    bundles: &[Bundle],
    seed: u64,
    fixture: &MainnetFixture,
) -> f64 {
    let aged = fixture.aged_held_out();
    let funder_pool: Vec<&str> = aged
        .iter()
        .filter_map(|p| p.earliest_funder_proxy.as_deref())
        .collect();
    if bundles.is_empty() || funder_pool.is_empty() {
        return 0.0;
    }
    let mut rng = ChaCha20Rng::seed_from_u64(seed ^ 0x4D4E_5F46_4753);
    let k = bundles[0].amounts.len();
    let mut hits = 0usize;

    for b in bundles {
        let kk = b.amounts.len();
        let real_funder = funder_pool[rng.gen_range(0..funder_pool.len())];
        let mut funders: Vec<&str> = vec![WARM_POOL_FUNDER_PUBKEY; kk];
        funders[b.real_index] = real_funder;
        if predict_by_shared_funder(&funders) == b.real_index {
            hits += 1;
        }
    }

    let n = bundles.len() as f64;
    let base = 1.0 / k as f64;
    hits as f64 / n - base
}

/// The residual of the shipped `warm_pool` mechanism against the
/// funding-graph channel **after** the per-slot sub-funder mitigation
/// (`cli/src/main.rs::warm_pool`, `sdk/src/warming.rs::derive_subfunder_keypair`):
/// each decoy leg is now funded by a distinct wallet — one dedicated
/// sub-funder per pool slot, never shared with any other slot — instead of
/// the single shared `WARM_POOL_FUNDER_PUBKEY` that
/// `eval_mainnet_funding_graph_shipped_mechanism` (above) models. Modeled here
/// with a distinct synthetic funder id per decoy leg (values don't matter,
/// only that no two legs in the same bundle share one) plus one real,
/// historical funder for the real leg, run through the same
/// `predict_by_shared_funder` attacker used above.
///
/// Because every leg's funder is now unique, `predict_by_shared_funder` finds
/// no majority to pick the odd leg out against — every count is `1`, so its
/// tie-break (lowest index) always resolves to leg `0`, independent of which
/// leg is actually real. With `real_index` uniform over `0..K` (guaranteed
/// upstream by however `bundles` was generated), that makes the attacker's
/// hit rate exactly `1/K` — i.e. the measured advantage is `0.0`: this
/// specific, shipped, same-hop attacker is fully neutralized, not just
/// reduced.
///
/// **What this does not model:** an attacker willing to trace one hop further
/// back (each sub-funder's own funder) still finds the same wallet (`payer`)
/// behind every slot — that stronger, multi-hop adversary is out of scope
/// here; see THREAT_MODEL.md §6 for the full, honest statement of what this
/// mitigation does and does not defend against.
pub fn eval_mainnet_funding_graph_shipped_mechanism_subfunder_pool(
    bundles: &[Bundle],
    seed: u64,
    fixture: &MainnetFixture,
) -> f64 {
    let aged = fixture.aged_held_out();
    let funder_pool: Vec<&str> = aged
        .iter()
        .filter_map(|p| p.earliest_funder_proxy.as_deref())
        .collect();
    if bundles.is_empty() || funder_pool.is_empty() {
        return 0.0;
    }
    let mut rng = ChaCha20Rng::seed_from_u64(seed ^ 0x4D4E_5F46_4753_5342);
    let k = bundles[0].amounts.len();
    let mut hits = 0usize;

    for (bundle_idx, b) in bundles.iter().enumerate() {
        let kk = b.amounts.len();
        let real_funder = funder_pool[rng.gen_range(0..funder_pool.len())];
        let decoy_ids: Vec<String> = (0..kk)
            .map(|leg| format!("subfunder-{bundle_idx}-{leg}"))
            .collect();
        let mut funders: Vec<&str> = decoy_ids.iter().map(|s| s.as_str()).collect();
        funders[b.real_index] = real_funder;
        if predict_by_shared_funder(&funders) == b.real_index {
            hits += 1;
        }
    }

    let n = bundles.len() as f64;
    let base = 1.0 / k as f64;
    hits as f64 / n - base
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

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::{pubkey::Pubkey, signer::Signer};
    use std::collections::HashSet;
    use supersonic_sdk::warming::{derive_pool_member_keypair, select_pool_slots};

    fn synthetic_bundles(k: usize, count: usize) -> Vec<Bundle> {
        let mut rng = ChaCha20Rng::seed_from_u64(0xB00D_1E00);
        (0..count)
            .map(|_| Bundle {
                amounts: vec![1u64; k],
                real_index: rng.gen_range(0..k),
            })
            .collect()
    }

    #[test]
    fn predict_by_shared_funder_picks_the_odd_one_out() {
        let funders = ["A", "A", "A", "B"];
        assert_eq!(predict_by_shared_funder(&funders), 3);
    }

    #[test]
    fn predict_by_shared_funder_ties_resolve_to_lowest_index() {
        // Every funder distinct: every count is 1, so the minimum-count tie
        // resolves to index 0, mirroring `predict_by_history`'s convention.
        let funders = ["A", "B", "C", "D"];
        assert_eq!(predict_by_shared_funder(&funders), 0);
    }

    /// Deterministic with a fixed seed, and the result is a sane advantage value
    /// (bounded in `[-1, 1]`, since it's `accuracy - 1/K` and accuracy is in
    /// `[0, 1]`) — not pinned to an exact number, since it's sensitive to the
    /// fixture's held-out sample, per the task's own instructions.
    #[test]
    fn eval_mainnet_funding_graph_shipped_mechanism_is_deterministic_and_bounded() {
        let fixture = MainnetFixture::load();
        for &k in &[2usize, 4, 8, 16] {
            let bundles = synthetic_bundles(k, 500);
            let a = eval_mainnet_funding_graph_shipped_mechanism(&bundles, 1, &fixture);
            let b = eval_mainnet_funding_graph_shipped_mechanism(&bundles, 1, &fixture);
            assert_eq!(a, b, "same seed must reproduce the same measurement");
            assert!(
                (-1.0..=1.0).contains(&a),
                "advantage {a} out of the expected [-1, 1] range for K={k}"
            );
        }
    }

    #[test]
    fn eval_mainnet_funding_graph_shipped_mechanism_empty_bundles_is_zero() {
        let fixture = MainnetFixture::load();
        assert_eq!(
            eval_mainnet_funding_graph_shipped_mechanism(&[], 1, &fixture),
            0.0
        );
    }

    /// The point of the sub-funder mitigation: since `predict_by_shared_funder`
    /// always guesses leg 0 once every funder is unique (see the function's
    /// doc comment), its hit rate is exactly `1/K` if and only if `real_index`
    /// is *exactly* uniform over the sample, not merely uniform in
    /// expectation — a randomly-drawn sample (like `synthetic_bundles`) would
    /// only converge to that, with sampling noise on any finite n. So this
    /// test builds `real_index` deterministically cycling `0..K` (`k` copies
    /// of each value) instead of drawing it at random, to get an exact
    /// uniform distribution and therefore an exact `0.0` residual — a
    /// mathematical identity here, not a statistical approximation.
    #[test]
    fn eval_mainnet_funding_graph_shipped_mechanism_subfunder_pool_is_zero_residual() {
        let fixture = MainnetFixture::load();
        for &k in &[2usize, 4, 8, 16] {
            let bundles: Vec<Bundle> = (0..k * 20)
                .map(|i| Bundle {
                    amounts: vec![1u64; k],
                    real_index: i % k,
                })
                .collect();
            let advantage =
                eval_mainnet_funding_graph_shipped_mechanism_subfunder_pool(&bundles, 1, &fixture);
            assert_eq!(
                advantage, 0.0,
                "expected exactly zero residual for K={k}, got {advantage}"
            );
        }
    }

    #[test]
    fn eval_mainnet_funding_graph_shipped_mechanism_subfunder_pool_empty_bundles_is_zero() {
        let fixture = MainnetFixture::load();
        assert_eq!(
            eval_mainnet_funding_graph_shipped_mechanism_subfunder_pool(&[], 1, &fixture),
            0.0
        );
    }

    /// Coverage checkpoint for the B4 gap named in this module's doc comment:
    /// `eval_channel`'s warm column does NOT route decoy selection through the
    /// real shipped selector (`select_pool_slots` / `derive_pool_member_keypair`)
    /// — that wiring was attempted and reverted (module doc has the full
    /// account, including the specific numbers that ruled it out). This test
    /// does not close that gap and must not be read as if it did; it's a canary
    /// living in THIS crate (not just `sdk/tests/properties.rs`, which proves an
    /// adjacent but distinct SDK-level guarantee) proving the real selector is
    /// invokable and well-formed from the harness at exactly the configuration
    /// `eval_channel` would need if it were wired through it: `POOL_SIZE`
    /// matching the CLI default, decoy counts spanning every `K` this harness
    /// reports (2/4/8/16), across many `bundle_id`s. If a future attempt rewires
    /// `eval_channel` through the selector, this test's assertions (right decoy
    /// count, no duplicate slot per bundle, every slot resolves to a real,
    /// distinct keypair) are exactly what that wiring needs to hold.
    #[test]
    fn warm_pool_selector_is_invokable_and_well_formed_at_every_reported_k() {
        const POOL_SIZE: u32 = 32;
        let seed = [9u8; 32];
        for &k in &[2usize, 4, 8, 16] {
            let n_decoys = k - 1;
            for bundle_id in 0..20u64 {
                let slots = select_pool_slots(&seed, bundle_id, n_decoys, POOL_SIZE);
                assert_eq!(
                    slots.len(),
                    n_decoys,
                    "K={k} bundle {bundle_id}: expected {n_decoys} decoy slots, got {}",
                    slots.len()
                );
                let unique: HashSet<u32> = slots.iter().copied().collect();
                assert_eq!(
                    unique.len(),
                    slots.len(),
                    "K={k} bundle {bundle_id}: duplicate slot within one bundle"
                );
                let mut seen_keys: HashSet<Pubkey> = HashSet::new();
                for &slot in &slots {
                    // The exact call eval_channel's warm regime would need to
                    // make per decoy leg if/when it's wired through the selector.
                    let kp = derive_pool_member_keypair(&seed, slot);
                    assert_ne!(
                        kp.pubkey(),
                        Pubkey::default(),
                        "K={k} slot {slot}: degenerate keypair"
                    );
                    assert!(
                        seen_keys.insert(kp.pubkey()),
                        "K={k} bundle {bundle_id} slot {slot}: pool member pubkey collided \
                         with another slot's — selector's distinctness guarantee broken"
                    );
                }
            }
        }
    }
}
