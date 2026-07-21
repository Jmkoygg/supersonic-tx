//! A *nonlinear* adversary — an extremely-randomized decision-tree ensemble.
//!
//! The logistic regression in `learned.rs` is linear (plus hand-added interaction
//! terms). A skeptical analyst wouldn't stop there — they'd throw a tree ensemble
//! (random forest / gradient boosting) at the problem, which captures arbitrary
//! nonlinear interactions with no manual feature engineering. This is that adversary.
//!
//! It reuses the **exact same 23 features** as `learned.rs`, so the comparison is
//! honest: same information, a strictly more powerful model. If the linear model and
//! this ensemble both land at ~the same small advantage, that is strong evidence the
//! residual is a real property of the generator, not an artifact of using too weak a
//! classifier. If the ensemble beats the linear model materially, our headline number
//! was understated — better we find that than a judge.
//!
//! Implementation is an Extra-Trees ensemble (bagged trees with randomized split
//! thresholds), from scratch — no ML crate — so every number is reproducible and
//! auditable. Deterministic in its seed.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;

use crate::eval::Bundle;
use crate::learned::{features, N_FEATURES};

const N_TREES: usize = 24;
const MAX_DEPTH: usize = 9;
const MIN_LEAF: usize = 24;
const THRESHOLDS_PER_FEATURE: usize = 24;

struct Row {
    x: [f64; N_FEATURES],
    y: f64, // 1.0 real, 0.0 decoy
}

enum Node {
    Leaf(f64), // P(real)
    Split {
        feat: usize,
        thr: f64,
        left: Box<Node>,
        right: Box<Node>,
    },
}

fn rows_of(bundles: &[Bundle]) -> Vec<Row> {
    let mut rows = Vec::new();
    for b in bundles {
        for i in 0..b.amounts.len() {
            rows.push(Row {
                x: features(&b.amounts, i),
                y: if i == b.real_index { 1.0 } else { 0.0 },
            });
        }
    }
    rows
}

fn mean_label(rows: &[&Row]) -> f64 {
    if rows.is_empty() {
        return 0.0;
    }
    rows.iter().map(|r| r.y).sum::<f64>() / rows.len() as f64
}

/// Gini impurity of a label set summarized by (count, positives).
fn gini(n: usize, pos: f64) -> f64 {
    if n == 0 {
        return 0.0;
    }
    let p = pos / n as f64;
    1.0 - p * p - (1.0 - p) * (1.0 - p)
}

fn build_tree(rows: &[&Row], depth: usize, rng: &mut ChaCha20Rng) -> Node {
    let n = rows.len();
    let pos: f64 = rows.iter().map(|r| r.y).sum();
    // Stop: pure, too small, or too deep.
    if depth >= MAX_DEPTH || n <= MIN_LEAF || pos == 0.0 || pos == n as f64 {
        return Node::Leaf(mean_label(rows));
    }

    // Try sqrt(features) random features; for each, random thresholds; keep best split.
    let n_try = ((N_FEATURES as f64).sqrt().ceil() as usize).max(1);
    let mut best: Option<(usize, f64, f64)> = None; // (feat, thr, weighted_gini)
    for _ in 0..n_try {
        let feat = rng.gen_range(0..N_FEATURES);
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for r in rows {
            lo = lo.min(r.x[feat]);
            hi = hi.max(r.x[feat]);
        }
        if hi.partial_cmp(&lo) != Some(std::cmp::Ordering::Greater) {
            continue; // constant feature in this node (or NaN, treated the same way)
        }
        for _ in 0..THRESHOLDS_PER_FEATURE {
            let thr = rng.gen_range(lo..hi);
            let (mut ln, mut lp, mut rn, mut rp) = (0usize, 0.0f64, 0usize, 0.0f64);
            for r in rows {
                if r.x[feat] <= thr {
                    ln += 1;
                    lp += r.y;
                } else {
                    rn += 1;
                    rp += r.y;
                }
            }
            if ln == 0 || rn == 0 {
                continue;
            }
            let wg = (ln as f64 * gini(ln, lp) + rn as f64 * gini(rn, rp)) / n as f64;
            if best.is_none_or(|(_, _, bg)| wg < bg) {
                best = Some((feat, thr, wg));
            }
        }
    }

    let (feat, thr, _) = match best {
        Some(b) => b,
        None => return Node::Leaf(mean_label(rows)),
    };
    let (mut left, mut right): (Vec<&Row>, Vec<&Row>) = (Vec::new(), Vec::new());
    for &r in rows {
        if r.x[feat] <= thr {
            left.push(r);
        } else {
            right.push(r);
        }
    }
    Node::Split {
        feat,
        thr,
        left: Box::new(build_tree(&left, depth + 1, rng)),
        right: Box::new(build_tree(&right, depth + 1, rng)),
    }
}

fn predict_tree(node: &Node, x: &[f64; N_FEATURES]) -> f64 {
    match node {
        Node::Leaf(p) => *p,
        Node::Split {
            feat,
            thr,
            left,
            right,
        } => {
            if x[*feat] <= *thr {
                predict_tree(left, x)
            } else {
                predict_tree(right, x)
            }
        }
    }
}

struct Forest {
    trees: Vec<Node>,
}

impl Forest {
    fn train(rows: &[Row], seed: u64) -> Self {
        let mut rng = ChaCha20Rng::seed_from_u64(seed);
        let mut trees = Vec::with_capacity(N_TREES);
        for _ in 0..N_TREES {
            // Bootstrap sample (with replacement), same size as the training set.
            let sample: Vec<&Row> = (0..rows.len())
                .map(|_| &rows[rng.gen_range(0..rows.len())])
                .collect();
            trees.push(build_tree(&sample, 0, &mut rng));
        }
        Forest { trees }
    }

    fn prob(&self, x: &[f64; N_FEATURES]) -> f64 {
        self.trees.iter().map(|t| predict_tree(t, x)).sum::<f64>() / self.trees.len() as f64
    }

    fn predict_bundle(&self, amounts: &[u64]) -> usize {
        let mut best = 0usize;
        let mut best_p = f64::NEG_INFINITY;
        for i in 0..amounts.len() {
            let p = self.prob(&features(amounts, i));
            if p > best_p {
                best_p = p;
                best = i;
            }
        }
        best
    }

    fn accuracy(&self, bundles: &[Bundle]) -> f64 {
        let hits = bundles
            .iter()
            .filter(|b| self.predict_bundle(&b.amounts) == b.real_index)
            .count();
        hits as f64 / bundles.len() as f64
    }
}

/// Train the ensemble on `train_set`, return `(train_accuracy, test_accuracy)`.
pub fn train_and_eval(train_set: &[Bundle], test_set: &[Bundle], seed: u64) -> (f64, f64) {
    let rows = rows_of(train_set);
    let forest = Forest::train(&rows, seed);
    (forest.accuracy(train_set), forest.accuracy(test_set))
}
