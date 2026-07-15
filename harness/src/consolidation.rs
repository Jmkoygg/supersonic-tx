//! Consolidation-linkage attack — measured, not assumed.
//!
//! The amount channel (the rest of the harness) asks "which leg is real *within one
//! bundle*." This module asks the recovery-phase question: once the user sweeps the
//! decoy funds back, can an observer of the **onward transfers** re-identify the
//! decoys and thus the real leg?
//!
//! We model the recovery graph and run the grouping attack the `THREAT_MODEL §4.6`
//! describes, for both recovery modes, and derive the advantage (we do **not**
//! hardcode `1 - 1/K`):
//!
//! * **Naive consolidation** (`recover` without `--disperse`): every decoy is swept
//!   to the *same* wallet. The adversary groups bundle destinations by shared onward
//!   recipient; the `K-1` decoys form one group, the real leg is the lone ungrouped
//!   destination — identified deterministically.
//! * **Dispersed** (`recover --disperse`): each decoy is swept to its *own* distinct
//!   sink in a separate transaction. No two destinations share an onward recipient,
//!   so the grouping attack forms no group and yields nothing beyond a uniform guess.

use crate::eval::Bundle;

/// Onward-recipient id for each leg of a bundle under a recovery mode.
/// `shared == true` → naive (all decoys share recipient 0); else dispersed (unique).
fn recipient_ids(k: usize, real_index: usize, shared: bool) -> Vec<u64> {
    let mut ids = vec![0u64; k];
    let mut next = 1u64;
    for (i, id) in ids.iter_mut().enumerate() {
        if i == real_index {
            // The real leg's payee is its own address — always unique.
            *id = next;
            next += 1;
        } else if shared {
            // Naive consolidation: every decoy forwards to the one wallet (id 0).
            *id = 0;
        } else {
            // Dispersed: each decoy forwards to a distinct sink.
            *id = next;
            next += 1;
        }
    }
    ids
}

/// Run the grouping attack over `bundles` and return the adversary's advantage over
/// `1/K`. The attacker never sees `real_index`; it derives its guess from group
/// sizes alone.
pub fn linkage_advantage(bundles: &[Bundle], shared: bool) -> f64 {
    if bundles.is_empty() {
        return 0.0;
    }
    let k = bundles[0].amounts.len();
    let mut hits = 0.0f64;

    for b in bundles {
        let ids = recipient_ids(k, b.real_index, shared);
        // Group size of each leg = how many legs share its onward recipient.
        let group_size: Vec<usize> = (0..k)
            .map(|i| ids.iter().filter(|&&x| x == ids[i]).count())
            .collect();
        // Legs in a group of >=2 are flagged as decoys (they co-forward). The real
        // leg is sought among the unflagged (group size 1) legs.
        let unflagged: Vec<usize> = (0..k).filter(|&i| group_size[i] == 1).collect();
        if unflagged.len() == 1 {
            // Deterministically identified.
            if unflagged[0] == b.real_index {
                hits += 1.0;
            }
        } else {
            // Ambiguous: uniform guess among the unflagged (or all, if none).
            let pool = if unflagged.is_empty() { k } else { unflagged.len() };
            hits += 1.0 / pool as f64;
        }
    }

    hits / bundles.len() as f64 - 1.0 / k as f64
}
