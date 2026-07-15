//! A *learned* adversary — the attack a real copy-trader would actually run.
//!
//! Fixed heuristics (max/min/roundest/central/…) each probe one channel. A serious
//! adversary trains a model over all channels at once. This is a plain logistic
//! regression over per-leg features, trained on the train split and evaluated on the
//! held-out test split — so "advantage ≈ 0" means *even a trained model* can't beat
//! `1/K`, not just that our seven hand-picked heuristics can't.
//!
//! No ML crate: it's a few dozen lines of gradient descent. That's the point — if a
//! classifier this simple could separate real from decoy, the generator would be
//! broken.

use supersonic_sdk::amounts::trailing_zeros_base10;

use crate::eval::Bundle;

const N_FEATURES: usize = 13;

/// Per-leg features, all relative to the leg's own bundle (an observer sees one
/// bundle at a time). This is deliberately the *strong* adversary — the one a real
/// analyst would build — not a strawman: it covers the amount channel with
/// non-linear terms (z, z², |·|), the centrality channel (signed and squared
/// distance to the log-median), the roundness channel, position, and three
/// structural channels a naive suite misses:
///   * **isolation** — log-distance to the nearest other leg (a snapped decoy sits
///     on a grid; a non-snapped real leg can be unusually isolated or unusually
///     clustered);
///   * **order-statistic rank** — normalized rank within the bundle;
///   * **collision multiplicity** — how many legs share this leg's exact value.
/// Feature standardization happens in `train`.
fn features(amounts: &[u64], idx: usize) -> [f64; N_FEATURES] {
    let k = amounts.len().max(1);
    let logs: Vec<f64> = amounts.iter().map(|&a| (a.max(1) as f64).ln()).collect();
    let mean = logs.iter().sum::<f64>() / k as f64;
    let var = logs.iter().map(|&l| (l - mean).powi(2)).sum::<f64>() / k as f64;
    let std = var.sqrt().max(1e-9);
    let mut sorted = logs.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med = sorted[k / 2];
    let mean_round =
        amounts.iter().map(|&a| trailing_zeros_base10(a) as f64).sum::<f64>() / k as f64;
    let z = (logs[idx] - mean) / std;
    let dmed = logs[idx] - med;

    // Isolation: min log-distance to any other leg.
    let mut isolation = f64::INFINITY;
    for (j, &lj) in logs.iter().enumerate() {
        if j != idx {
            isolation = isolation.min((logs[idx] - lj).abs());
        }
    }
    if !isolation.is_finite() {
        isolation = 0.0;
    }
    // Order-statistic rank of this leg's amount within the bundle.
    let rank = logs.iter().filter(|&&l| l < logs[idx]).count() as f64 / (k as f64 - 1.0).max(1.0);
    // Collision multiplicity: how many legs share this exact value.
    let mult = amounts.iter().filter(|&&a| a == amounts[idx]).count() as f64;
    let rd = trailing_zeros_base10(amounts[idx]) as f64 - mean_round;

    [
        1.0,
        z,
        z * z,
        z.abs(),
        dmed,
        dmed * dmed,
        rd,
        rd * rd,
        idx as f64 / (k as f64 - 1.0).max(1.0),
        isolation,
        isolation * isolation,
        rank,
        mult,
    ]
}

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

/// Feature standardization stats (per feature, bias excluded), fit on train.
struct Standardizer {
    mean: [f64; N_FEATURES],
    std: [f64; N_FEATURES],
}

impl Standardizer {
    fn fit(rows: &[[f64; N_FEATURES]]) -> Self {
        let n = rows.len().max(1) as f64;
        let mut mean = [0.0; N_FEATURES];
        let mut std = [1.0; N_FEATURES];
        for x in rows {
            for j in 0..N_FEATURES {
                mean[j] += x[j];
            }
        }
        for j in 0..N_FEATURES {
            mean[j] /= n;
        }
        let mut var = [0.0; N_FEATURES];
        for x in rows {
            for j in 0..N_FEATURES {
                var[j] += (x[j] - mean[j]).powi(2);
            }
        }
        for j in 1..N_FEATURES {
            std[j] = (var[j] / n).sqrt().max(1e-9);
        }
        // Keep the bias feature at 1.0 (do not center/scale it).
        mean[0] = 0.0;
        std[0] = 1.0;
        Standardizer { mean, std }
    }

    fn apply(&self, x: &[f64; N_FEATURES]) -> [f64; N_FEATURES] {
        let mut out = [0.0; N_FEATURES];
        for j in 0..N_FEATURES {
            out[j] = (x[j] - self.mean[j]) / self.std[j];
        }
        out[0] = 1.0;
        out
    }
}

/// Train standardized logistic regression on all legs (label 1 = real).
fn train(rows: &[([f64; N_FEATURES], f64)], epochs: usize, lr: f64) -> [f64; N_FEATURES] {
    let mut w = [0.0f64; N_FEATURES];
    if rows.is_empty() {
        return w;
    }
    for _ in 0..epochs {
        let mut grad = [0.0f64; N_FEATURES];
        for (x, y) in rows {
            let z: f64 = (0..N_FEATURES).map(|j| w[j] * x[j]).sum();
            let err = sigmoid(z) - y;
            for j in 0..N_FEATURES {
                grad[j] += err * x[j];
            }
        }
        let scale = lr / rows.len() as f64;
        for j in 0..N_FEATURES {
            w[j] -= scale * grad[j];
        }
    }
    w
}

fn predict(w: &[f64; N_FEATURES], std: &Standardizer, amounts: &[u64]) -> usize {
    let mut best = 0usize;
    let mut best_s = f64::NEG_INFINITY;
    for i in 0..amounts.len() {
        let x = std.apply(&features(amounts, i));
        let s: f64 = (0..N_FEATURES).map(|j| w[j] * x[j]).sum();
        if s > best_s {
            best_s = s;
            best = i;
        }
    }
    best
}

fn accuracy(w: &[f64; N_FEATURES], std: &Standardizer, bundles: &[Bundle]) -> f64 {
    let hits = bundles
        .iter()
        .filter(|b| predict(w, std, &b.amounts) == b.real_index)
        .count();
    hits as f64 / bundles.len() as f64
}

/// Train on `train_set`, return `(train_accuracy, test_accuracy)`. Features are
/// standardized with stats fit on train only.
pub fn train_and_eval(train_set: &[Bundle], test_set: &[Bundle]) -> (f64, f64) {
    let mut raw: Vec<[f64; N_FEATURES]> = Vec::new();
    for b in train_set {
        for i in 0..b.amounts.len() {
            raw.push(features(&b.amounts, i));
        }
    }
    let std = Standardizer::fit(&raw);

    let mut rows: Vec<([f64; N_FEATURES], f64)> = Vec::with_capacity(raw.len());
    for b in train_set {
        for i in 0..b.amounts.len() {
            let label = if i == b.real_index { 1.0 } else { 0.0 };
            rows.push((std.apply(&features(&b.amounts, i)), label));
        }
    }

    let w = train(&rows, 3000, 0.5);
    (accuracy(&w, &std, train_set), accuracy(&w, &std, test_set))
}
