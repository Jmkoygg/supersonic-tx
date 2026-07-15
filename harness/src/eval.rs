//! Evaluation: generate many bundles with the real SDK, then measure how well the
//! adversary can pick the real leg.
//!
//! Method (kept honest on purpose):
//! * Real amounts are sampled from a broad, realistic distribution — log-uniform
//!   from 0.001 to 100 SOL, with ~40% snapped to human-round values — *independent*
//!   of how decoys are generated. We are not feeding the SDK a strawman.
//! * We generate `2N` bundles and split train/test. The adversary picks the single
//!   best classifier on the **train** half and we report its advantage on the
//!   held-out **test** half. This stops the adversary from cherry-picking a
//!   classifier that only looked good on noise.
//! * `advantage = accuracy − 1/K`. Zero means the adversary does no better than
//!   guessing; that is the win condition.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use serde::Serialize;
use solana_sdk::signature::{Keypair, Signer};
use supersonic_sdk::{amounts, plan_bundle, DecoyConfig};

use crate::classifiers::Classifier;

/// What an observer sees for one bundle, plus the ground-truth real index.
pub struct Bundle {
    pub amounts: Vec<u64>,
    pub real_index: usize,
}

#[derive(Serialize, Clone)]
pub struct ClassifierResult {
    pub name: String,
    pub test_accuracy: f64,
    pub test_advantage: f64,
}

#[derive(Serialize, Clone)]
pub struct KResult {
    pub k: usize,
    pub n_train: usize,
    pub n_test: usize,
    pub baseline: f64,
    pub per_classifier: Vec<ClassifierResult>,
    /// The classifier the adversary selected on train, and its honest test
    /// advantage — the headline number for this K.
    pub best_classifier: String,
    pub adversary_test_advantage: f64,
    /// Naive-consolidation worst case: if decoys are swept back immediately, the
    /// adversary identifies them and the real leg is whatever is left.
    pub naive_consolidation_advantage: f64,
}

/// Sample a realistic "real intent" amount in lamports.
fn sample_real_amount<R: Rng>(rng: &mut R) -> u64 {
    let ln_min = 1_000_000f64.ln(); // 0.001 SOL
    let ln_max = 100_000_000_000f64.ln(); // 100 SOL
    let x = rng.gen_range(ln_min..ln_max).exp();
    let mut v = x.round() as u64;
    if rng.gen_bool(0.4) {
        // Humans often send round amounts; snap to 0.001–1 SOL granularity.
        let level = rng.gen_range(6..=9);
        v = amounts::snap_to_roundness(v, level);
    }
    v.max(1)
}

/// Generate `count` bundles of anonymity set `k` with the real SDK.
fn generate_bundles(k: usize, count: usize, cfg: DecoyConfig, rng: &mut ChaCha20Rng) -> Vec<Bundle> {
    let mut master_seed = [0u8; 32];
    rng.fill(&mut master_seed);
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let real_amount = sample_real_amount(rng);
        let real_dest = Keypair::new().pubkey();
        let plan = plan_bundle(&master_seed, i as u64, real_dest, real_amount, k, cfg)
            .expect("valid bundle params");
        out.push(Bundle {
            amounts: plan.amounts(),
            real_index: plan.real_index,
        });
    }
    out
}

fn advantage(classifier: Classifier, bundles: &[Bundle], k: usize) -> (f64, f64) {
    let hits = bundles
        .iter()
        .filter(|b| classifier.predict(&b.amounts) == b.real_index)
        .count();
    let acc = hits as f64 / bundles.len() as f64;
    (acc, acc - 1.0 / k as f64)
}

/// Run the full evaluation for one anonymity-set size `k`.
pub fn eval_k(k: usize, n: usize, cfg: DecoyConfig, seed: u64) -> KResult {
    let mut rng = ChaCha20Rng::seed_from_u64(seed ^ (k as u64).wrapping_mul(0x9E3779B97F4A7C15));
    let train = generate_bundles(k, n, cfg, &mut rng);
    let test = generate_bundles(k, n, cfg, &mut rng);

    // Every candidate reports (name, train advantage, test advantage). The adversary
    // selects the best on TRAIN; we report its TEST advantage (no peeking at test).
    let mut per_classifier = Vec::new();
    let mut candidates: Vec<(String, f64, f64)> = Vec::new();
    let base = 1.0 / k as f64;

    for &c in Classifier::all() {
        let (_train_acc, train_adv) = advantage(c, &train, k);
        let (test_acc, test_adv) = advantage(c, &test, k);
        per_classifier.push(ClassifierResult {
            name: c.name().to_string(),
            test_accuracy: test_acc,
            test_advantage: test_adv,
        });
        candidates.push((c.name().to_string(), train_adv, test_adv));
    }

    // The learned logistic-regression adversary competes on equal footing — this is
    // the attack a real copy-trader would run over all channels at once.
    let (learn_train_acc, learn_test_acc) = crate::learned::train_and_eval(&train, &test);
    per_classifier.push(ClassifierResult {
        name: "learned_logreg".to_string(),
        test_accuracy: learn_test_acc,
        test_advantage: learn_test_acc - base,
    });
    candidates.push((
        "learned_logreg".to_string(),
        learn_train_acc - base,
        learn_test_acc - base,
    ));

    let best = candidates
        .iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
        .unwrap();
    let best_classifier = best.0.clone();
    let best_test_adv = best.2;

    // Naive consolidation: sweeping decoys back identifies all K-1 of them, so the
    // real leg is fully exposed. accuracy = 1.0, advantage = 1 - 1/k.
    let naive_consolidation_advantage = 1.0 - 1.0 / k as f64;

    KResult {
        k,
        n_train: n,
        n_test: n,
        baseline: base,
        per_classifier,
        best_classifier,
        adversary_test_advantage: best_test_adv,
        naive_consolidation_advantage,
    }
}
