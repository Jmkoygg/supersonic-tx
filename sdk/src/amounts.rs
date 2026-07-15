//! Decoy **amount** generation — the part of the SDK that defeats the amount-based
//! and round-number classifiers in the threat model (§4.3).
//!
//! Two ideas do the work:
//!
//! 1. **Log-space clustering.** Payment sizes are roughly log-distributed, and a
//!    naive `rand()` decoy set makes the real amount an outlier (smallest, largest,
//!    or oddly-placed). We instead draw decoys from a log-normal centered on the
//!    real amount, so the real value sits *inside the bulk* of the set rather than
//!    at an edge.
//!
//! 2. **Roundness matching.** Real payments are often round (`1.0 SOL`,
//!    `0.05 SOL`). If decoys are jittered to precise values while the real one is
//!    round, the round leg is trivially the real one. So we measure the decimal
//!    roundness of the real amount and snap a realistic fraction of decoys to the
//!    *same* roundness, mixing in nearby levels for the rest.
//!
//! Both are deterministic given the RNG the caller passes (seeded from the master
//! seed), which keeps the whole plan reproducible.

use rand::Rng;

use crate::DecoyConfig;

/// Generate `n` decoy amounts so that the real amount is statistically
/// **exchangeable** with them, then roundness-match them to the real.
///
/// ## Why exchangeability (the fix for the log-centrality leak)
///
/// A naive generator draws decoys from a log-normal centered on `ln(real)`. That
/// makes the real leg the *most central* value in log-space, and an adversary that
/// simply picks "the value closest to the median" identifies it far above `1/K`.
/// (This is the mirror of the outlier attack, and it is decisive — see the harness'
/// `log_median_central` classifier.)
///
/// The correct construction treats `real` as if it were itself one draw from the
/// bundle's log-normal `LN(mu, sigma)`. We draw the real leg's own z-score
/// `z_real ~ N(0,1)` and back out `mu = ln(real) - sigma*z_real`; the decoys are
/// then fresh i.i.d. draws from that *same* `LN(mu, sigma)`. Because the real leg's
/// z-score came from the same `N(0,1)` as the decoys', the real value is just one of
/// `K` i.i.d. samples — no amount- or position-based classifier can beat `1/K` on
/// the amount channel, by construction rather than by tuning.
pub fn generate_decoy_amounts<R: Rng>(
    real: u64,
    n: usize,
    cfg: &DecoyConfig,
    rng: &mut R,
) -> Vec<u64> {
    if n == 0 {
        return Vec::new();
    }
    let real_round = trailing_zeros_base10(real);

    // Draw the real leg's own z-score, then derive the bundle center so that `real`
    // is a genuine LN(mu, sigma) sample with that z-score. Bound z to ±3.5 (both for
    // real and decoys) so tails can't blow the user's balance; the same bound applies
    // to every leg, so it introduces no directional tell.
    const Z_CLAMP: f64 = 3.5;
    let z_real = standard_normal(rng).clamp(-Z_CLAMP, Z_CLAMP);
    let mu = (real.max(1) as f64).ln() - cfg.sigma * z_real;

    // Plausible band, widened to always include the real leg (the real is never
    // distorted). Decoys are rejection-sampled to land inside it, so a decoy in a
    // Gaussian tail can't fall to an implausible size and reveal itself as "not the
    // real one" (the support-boundary attack).
    let lo = (cfg.min_lamports.min(real).max(1)) as f64;
    let hi = (cfg.max_lamports.max(real)) as f64;

    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        // Rejection-sample z until the amount is inside the band. Truncation keeps the
        // log-normal smooth rather than piling decoys up at the edge.
        let mut raw;
        let mut tries = 0;
        loop {
            let z = standard_normal(rng).clamp(-Z_CLAMP, Z_CLAMP);
            raw = (mu + cfg.sigma * z).exp();
            if (raw >= lo && raw <= hi) || tries >= 64 {
                break;
            }
            tries += 1;
        }
        let mut v = (raw.clamp(lo, hi).round() as u64).max(1);

        // round_match_prob == 0 disables roundness matching entirely: decoys are raw
        // exchangeable draws (no grid snapping). Kept as an experimental/measurement
        // path — it removes the snap-vs-exact asymmetry but re-exposes a round real.
        if cfg.round_match_prob <= 0.0 {
            out.push(v);
            continue;
        }

        // Roundness matching: real's roundness is fixed and observable, so decoys'
        // roundness levels are drawn symmetrically around the real's level (mode =
        // real_round), making the real's roundness a typical draw rather than a tell.
        let level = if rng.gen_bool(cfg.round_match_prob) {
            real_round
        } else {
            let lo = real_round.saturating_sub(1);
            let hi = (real_round + 1).min(MAX_ROUND_LEVEL);
            rng.gen_range(lo..=hi)
        };
        v = snap_to_roundness(v, level);
        // Snapping can nudge a value across the band edge; keep it inside.
        v = ((v as f64).clamp(lo, hi).round() as u64).max(1);
        out.push(v);
    }
    out
}

/// Cap on how "round" we ever snap (10^9 lamports = 1 SOL); beyond this, snapping
/// would erase all variety.
const MAX_ROUND_LEVEL: u32 = 9;

/// Number of trailing zeros of `x` in base 10, capped at [`MAX_ROUND_LEVEL`].
/// `1_000_000 -> 6`, `1_337_000 -> 3`, `42 -> 0`, `0 -> 0`.
pub fn trailing_zeros_base10(x: u64) -> u32 {
    if x == 0 {
        return 0;
    }
    let mut n = 0;
    let mut v = x;
    while v % 10 == 0 && n < MAX_ROUND_LEVEL {
        v /= 10;
        n += 1;
    }
    n
}

/// Round `v` to have **exactly** `level` trailing zeros in base 10 (never more),
/// never returning zero (a zero-value leg is rejected on-chain and is a trivial
/// tell).
///
/// The "exactly, never more" part matters: if a decoy snapped to `level` happened
/// to land on a multiple of `10^(level+1)` it would read as *rounder* than a real
/// leg fixed at exactly `level` trailing zeros, letting the real stand out as the
/// least-round leg. Forcing exactly `level` keeps matched decoys' roundness
/// identical to the real's.
pub fn snap_to_roundness(v: u64, level: u32) -> u64 {
    let m = 10u64.saturating_pow(level);
    if m <= 1 {
        return v.max(1);
    }
    let mut snapped = ((v + m / 2) / m) * m;
    if snapped == 0 {
        snapped = m;
    }
    // Break any extra trailing zero so the result has exactly `level` of them.
    if level < MAX_ROUND_LEVEL {
        let m10 = m.saturating_mul(10);
        if snapped % m10 == 0 {
            snapped += m;
        }
    }
    snapped
}

/// A standard normal sample via the Box–Muller transform.
fn standard_normal<R: Rng>(rng: &mut R) -> f64 {
    // Guard u1 away from 0 so ln() is finite.
    let u1: f64 = rng.gen_range(f64::MIN_POSITIVE..1.0);
    let u2: f64 = rng.gen_range(0.0..1.0);
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;

    fn rng() -> ChaCha20Rng {
        ChaCha20Rng::from_seed([13u8; 32])
    }

    #[test]
    fn trailing_zeros_are_correct() {
        assert_eq!(trailing_zeros_base10(1_000_000), 6);
        assert_eq!(trailing_zeros_base10(1_337_000), 3);
        assert_eq!(trailing_zeros_base10(42), 0);
        assert_eq!(trailing_zeros_base10(0), 0);
        assert_eq!(trailing_zeros_base10(5_000_000_000), MAX_ROUND_LEVEL); // capped
    }

    #[test]
    fn snap_never_returns_zero() {
        assert_eq!(snap_to_roundness(3, 9), 1_000_000_000); // rounds up off zero-ish
        assert_eq!(snap_to_roundness(0, 3), 1_000);
        assert_eq!(snap_to_roundness(1_499, 3), 1_000);
        assert_eq!(snap_to_roundness(1_500, 3), 2_000);
    }

    #[test]
    fn generates_requested_count_all_positive() {
        let cfg = DecoyConfig::default();
        let v = generate_decoy_amounts(1_337_000, 7, &cfg, &mut rng());
        assert_eq!(v.len(), 7);
        assert!(v.iter().all(|&a| a > 0));
    }

    #[test]
    fn real_is_exchangeable_not_systematically_extreme() {
        // Exchangeability means the real leg is the global min OR max of the bundle
        // at ~2/K — the same rate as any single leg — not systematically central
        // (the old centrality bug) nor systematically extreme. For K=8 that's 0.25.
        let cfg = DecoyConfig::default();
        let real = 1_337_000u64;
        let k = 8usize;
        let trials = 4000;
        let mut extreme = 0;
        for i in 0..trials {
            let mut r = ChaCha20Rng::seed_from_u64(i);
            let decoys = generate_decoy_amounts(real, k - 1, &cfg, &mut r);
            let min = *decoys.iter().min().unwrap();
            let max = *decoys.iter().max().unwrap();
            if real <= min || real >= max {
                extreme += 1;
            }
        }
        let rate = extreme as f64 / trials as f64;
        let expected = 2.0 / k as f64; // 0.25
        assert!(
            (rate - expected).abs() < 0.06,
            "real should be the extreme at ~{expected:.2} (exchangeable), got {rate:.3}"
        );
    }

    #[test]
    fn real_is_not_the_most_central_above_baseline() {
        // Regression test for the log-centrality leak: with the exchangeable
        // construction, the real leg must be the value closest to the log-median at
        // only ~1/K frequency, not systematically. (Before the fix, the real was the
        // most-central value far above 1/K, which broke the headline claim.)
        let cfg = DecoyConfig::default();
        let real = 1_337_000u64;
        let k = 8usize; // 1 real + 7 decoys
        let trials = 4000;
        let mut real_is_central = 0;
        for i in 0..trials {
            let mut r = ChaCha20Rng::seed_from_u64(1000 + i);
            let decoys = generate_decoy_amounts(real, k - 1, &cfg, &mut r);
            let mut logs: Vec<f64> = decoys.iter().map(|&a| (a as f64).ln()).collect();
            logs.push((real as f64).ln());
            let med = {
                let mut s = logs.clone();
                s.sort_by(|a, b| a.partial_cmp(b).unwrap());
                s[s.len() / 2]
            };
            // Is the real leg (last pushed) the closest to the median?
            let real_dist = (logs[k - 1] - med).abs();
            let most_central = logs.iter().all(|&l| (l - med).abs() >= real_dist - 1e-9);
            if most_central {
                real_is_central += 1;
            }
        }
        let rate = real_is_central as f64 / trials as f64;
        let baseline = 1.0 / k as f64; // 0.125
        // Allow slack for roundness perturbation + ties, but it must be near 1/K,
        // nowhere near the pre-fix leak (which pushed this well above baseline).
        assert!(
            rate < baseline + 0.05,
            "real is most-central at rate {rate:.3}, baseline {baseline:.3} — centrality leak regressed"
        );
    }

    #[test]
    fn round_real_is_hidden_among_round_decoys() {
        // If the real is round (1.0 SOL), a good chunk of decoys must share that
        // roundness, or the round leg is a giveaway.
        let cfg = DecoyConfig::default();
        let real = 1_000_000_000u64; // 1 SOL, very round
        let decoys = generate_decoy_amounts(real, 10, &cfg, &mut rng());
        let round_like = decoys
            .iter()
            .filter(|&&a| trailing_zeros_base10(a) >= 6)
            .count();
        assert!(
            round_like >= 3,
            "a round real needs round company, got {round_like}/10 round-ish decoys"
        );
    }
}
