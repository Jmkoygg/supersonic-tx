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

/// Generate `n` decoy amounts around `real`, roundness-matched to it.
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
    let mu = (real.max(1) as f64).ln();

    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        // Standard normal via Box–Muller, so we don't pull in a distributions crate.
        let z = standard_normal(rng);
        let raw = (mu + cfg.sigma * z).exp();
        // Clamp to a sane band: never zero, and cap runaway tails at 100x the real
        // so a decoy can't dwarf the whole bundle and blow the user's balance.
        let raw = raw.clamp(1.0, real.max(1) as f64 * 100.0);
        let mut v = raw.round() as u64;
        v = v.max(1);

        // Roundness matching: most decoys share the real's roundness; the rest get a
        // *symmetrically* nearby level so the set isn't suspiciously uniform, without
        // biasing decoys to be systematically rounder (or less round) than the real.
        // The earlier asymmetric range let a precise real stand out as "least round"
        // and leak at small K; a symmetric ±1 window removes that directional tell.
        let level = if rng.gen_bool(cfg.round_match_prob) {
            real_round
        } else {
            let lo = real_round.saturating_sub(1);
            let hi = (real_round + 1).min(MAX_ROUND_LEVEL);
            rng.gen_range(lo..=hi)
        };
        out.push(snap_to_roundness(v, level));
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

/// Round `v` to the nearest multiple of `10^level`, never returning zero (a
/// zero-value leg is rejected on-chain and is a trivial tell).
pub fn snap_to_roundness(v: u64, level: u32) -> u64 {
    let m = 10u64.saturating_pow(level);
    if m <= 1 {
        return v.max(1);
    }
    let snapped = ((v + m / 2) / m) * m;
    if snapped == 0 {
        m
    } else {
        snapped
    }
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
    fn real_amount_is_not_an_outlier() {
        // The real should land inside the decoy range, not be the unique min/max —
        // that positional tell is exactly what log-space clustering removes.
        let cfg = DecoyConfig::default();
        let real = 1_337_000u64;
        let mut inside = 0;
        let trials = 200;
        for i in 0..trials {
            let mut r = ChaCha20Rng::seed_from_u64(i);
            let decoys = generate_decoy_amounts(real, 7, &cfg, &mut r);
            let min = *decoys.iter().min().unwrap();
            let max = *decoys.iter().max().unwrap();
            if real >= min && real <= max {
                inside += 1;
            }
        }
        // Not a guarantee every time, but it must be the overwhelming norm.
        assert!(
            inside as f64 / trials as f64 > 0.8,
            "real should sit within the decoy range most of the time, got {inside}/{trials}"
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
