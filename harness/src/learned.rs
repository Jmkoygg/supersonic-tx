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

const N_FEATURES: usize = 5;

/// Per-leg features, all relative to the leg's own bundle (an observer sees one
/// bundle at a time): bias, log-amount z-score, |distance to log-median|, roundness
/// delta from the bundle mean, and normalized position.
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

    [
        1.0,
        (logs[idx] - mean) / std,
        (logs[idx] - med).abs(),
        trailing_zeros_base10(amounts[idx]) as f64 - mean_round,
        idx as f64 / (k as f64 - 1.0).max(1.0),
    ]
}

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

/// Train logistic regression on all legs of the train bundles (label 1 = real).
fn train(bundles: &[Bundle], epochs: usize, lr: f64) -> [f64; N_FEATURES] {
    let mut w = [0.0f64; N_FEATURES];
    // Collect the training rows once.
    let mut rows: Vec<([f64; N_FEATURES], f64)> = Vec::new();
    for b in bundles {
        for i in 0..b.amounts.len() {
            let label = if i == b.real_index { 1.0 } else { 0.0 };
            rows.push((features(&b.amounts, i), label));
        }
    }
    if rows.is_empty() {
        return w;
    }
    for _ in 0..epochs {
        let mut grad = [0.0f64; N_FEATURES];
        for (x, y) in &rows {
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

fn score(w: &[f64; N_FEATURES], amounts: &[u64], idx: usize) -> f64 {
    let x = features(amounts, idx);
    (0..N_FEATURES).map(|j| w[j] * x[j]).sum()
}

/// Predict the real leg of a bundle as the highest-scoring leg.
fn predict(w: &[f64; N_FEATURES], amounts: &[u64]) -> usize {
    let mut best = 0usize;
    let mut best_s = f64::NEG_INFINITY;
    for i in 0..amounts.len() {
        let s = score(w, amounts, i);
        if s > best_s {
            best_s = s;
            best = i;
        }
    }
    best
}

fn accuracy(w: &[f64; N_FEATURES], bundles: &[Bundle]) -> f64 {
    let hits = bundles
        .iter()
        .filter(|b| predict(w, &b.amounts) == b.real_index)
        .count();
    hits as f64 / bundles.len() as f64
}

/// Train on `train`, return `(train_accuracy, test_accuracy)`.
pub fn train_and_eval(train_set: &[Bundle], test_set: &[Bundle]) -> (f64, f64) {
    let w = train(train_set, 300, 0.5);
    (accuracy(&w, train_set), accuracy(&w, test_set))
}
