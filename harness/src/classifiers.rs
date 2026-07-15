//! Adversary classifiers — each is a concrete attack from `THREAT_MODEL.md §4`,
//! operating only on what an on-chain observer sees for a bundle: the ordered list
//! of leg amounts (and their decimal roundness / position). Each returns its guess
//! of which leg index is the user's real intent.
//!
//! Destinations are deliberately *not* a feature here: in the harness every
//! destination (real and decoy) is a fresh key, so the destination channel is held
//! constant to isolate the amount/position channel the SDK actually controls. The
//! destination-history channel is a separate, acknowledged attack (defended
//! operationally by pre-warming decoy addresses / a companion account-cooker), not
//! something this harness claims to measure.

use supersonic_sdk::amounts::trailing_zeros_base10;

/// A predicted real-leg index.
pub type Prediction = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Classifier {
    /// "The real payment is the big one."
    MaxAmount,
    /// "The real payment is the small one (decoys are inflated)."
    MinAmount,
    /// "The real amount is the roundest (1.0 SOL etc.)."
    Roundest,
    /// "The real amount is the odd, precise one; decoys are round."
    LeastRound,
    /// "The real amount is the outlier from the pack" (in log space).
    LogMedianOutlier,
    /// Fixed-position guesses — catch any ordering leak.
    FirstPosition,
    LastPosition,
}

impl Classifier {
    pub fn all() -> &'static [Classifier] {
        &[
            Classifier::MaxAmount,
            Classifier::MinAmount,
            Classifier::Roundest,
            Classifier::LeastRound,
            Classifier::LogMedianOutlier,
            Classifier::FirstPosition,
            Classifier::LastPosition,
        ]
    }

    pub fn name(&self) -> &'static str {
        match self {
            Classifier::MaxAmount => "max_amount",
            Classifier::MinAmount => "min_amount",
            Classifier::Roundest => "roundest",
            Classifier::LeastRound => "least_round",
            Classifier::LogMedianOutlier => "log_median_outlier",
            Classifier::FirstPosition => "first_position",
            Classifier::LastPosition => "last_position",
        }
    }

    /// Predict the real-leg index from the observable amounts. Ties resolve to the
    /// lowest index; since the real leg is placed at a uniformly-random position,
    /// tie-breaking does not bias toward or away from it.
    pub fn predict(&self, amounts: &[u64]) -> Prediction {
        debug_assert!(!amounts.is_empty());
        match self {
            Classifier::MaxAmount => argmax_by(amounts, |&a| a as f64),
            Classifier::MinAmount => argmax_by(amounts, |&a| -(a as f64)),
            Classifier::Roundest => argmax_by(amounts, |&a| trailing_zeros_base10(a) as f64),
            Classifier::LeastRound => argmax_by(amounts, |&a| -(trailing_zeros_base10(a) as f64)),
            Classifier::LogMedianOutlier => {
                let logs: Vec<f64> = amounts.iter().map(|&a| (a.max(1) as f64).ln()).collect();
                let med = median(&logs);
                argmax_by(&logs, |&l| (l - med).abs())
            }
            Classifier::FirstPosition => 0,
            Classifier::LastPosition => amounts.len() - 1,
        }
    }
}

/// Index of the maximum of `key(item)`, lowest index on ties.
fn argmax_by<T, F: Fn(&T) -> f64>(xs: &[T], key: F) -> usize {
    let mut best = 0usize;
    let mut best_v = f64::NEG_INFINITY;
    for (i, x) in xs.iter().enumerate() {
        let v = key(x);
        if v > best_v {
            best_v = v;
            best = i;
        }
    }
    best
}

fn median(xs: &[f64]) -> f64 {
    let mut s = xs.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_and_min_pick_the_edges() {
        let a = [10u64, 5, 99, 7];
        assert_eq!(Classifier::MaxAmount.predict(&a), 2);
        assert_eq!(Classifier::MinAmount.predict(&a), 1);
    }

    #[test]
    fn roundest_picks_the_round_one() {
        let a = [1_337u64, 1_000_000, 42_042]; // middle is roundest
        assert_eq!(Classifier::Roundest.predict(&a), 1);
    }

    #[test]
    fn position_classifiers_are_fixed() {
        let a = [1u64, 2, 3, 4];
        assert_eq!(Classifier::FirstPosition.predict(&a), 0);
        assert_eq!(Classifier::LastPosition.predict(&a), 3);
    }
}
