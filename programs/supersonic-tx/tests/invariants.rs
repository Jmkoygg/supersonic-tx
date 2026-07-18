//! Invariant tests for the supersonic-tx router (Phase 3).
//!
//! Each test maps to an invariant from THREAT_MODEL.md §5. They run in-process
//! against the compiled program via LiteSVM (Rust, not TypeScript — the bounty is
//! Rust end-to-end). Build the program first: `anchor build`.
//!
//! Phase 3 model: each leg moves `amount` lamports from the signer to a paired
//! destination in `remaining_accounts`. Legs are economically real (value truly
//! moves — it survives a balance-delta filter) and non-round-trip (funds stay at
//! the destination). Decoy destinations are user-controlled and recoverable; the
//! program does not and cannot tell them from the real payee.

use anchor_lang::{InstructionData, ToAccountMetas};
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_program,
    transaction::Transaction,
};
use supersonic_tx::{Leg, MAX_LEGS};

const SO_PATH: &str = "../../target/deploy/supersonic_tx.so";
const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

/// Boot a LiteSVM instance with the router loaded and a funded user.
fn setup() -> (LiteSVM, Keypair) {
    let mut svm = LiteSVM::new();
    svm.add_program_from_file(supersonic_tx::ID, SO_PATH)
        .expect("load program .so — run `anchor build` first");

    let user = Keypair::new();
    svm.airdrop(&user.pubkey(), 10 * LAMPORTS_PER_SOL).unwrap();
    (svm, user)
}

/// Build (and try to send) an `execute_bundle` tx. `dests` are the per-leg
/// destination accounts, appended as writable, non-signer remaining_accounts in
/// leg order.
fn send_bundle(
    svm: &mut LiteSVM,
    user: &Keypair,
    legs: Vec<Leg>,
    dests: &[Pubkey],
) -> Result<(), litesvm::types::FailedTransactionMetadata> {
    let data = supersonic_tx::instruction::ExecuteBundle { legs }.data();
    let mut accounts = supersonic_tx::accounts::ExecuteBundle {
        user: user.pubkey(),
        system_program: system_program::ID,
    }
    .to_account_metas(None);
    for d in dests {
        accounts.push(AccountMeta::new(*d, false));
    }
    let ix = Instruction {
        program_id: supersonic_tx::ID,
        accounts,
        data,
    };
    let blockhash = svm.latest_blockhash();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&user.pubkey()), &[user], blockhash);
    svm.send_transaction(tx).map(|_| ())
}

/// n fresh destination pubkeys (stand-ins for user-controlled decoy wallets and
/// the one real payee).
fn dests(n: usize) -> Vec<Pubkey> {
    (0..n).map(|_| Keypair::new().pubkey()).collect()
}

/// Happy path: a single-leg bundle succeeds and the destination actually receives
/// the lamports (economic reality — this is not a no-op).
#[test]
fn single_leg_succeeds_and_moves_value() {
    let (mut svm, user) = setup();
    let d = dests(1);
    let res = send_bundle(
        &mut svm,
        &user,
        vec![Leg {
            amount: LAMPORTS_PER_SOL,
        }],
        &d,
    );
    assert!(res.is_ok(), "single-leg bundle should succeed: {res:?}");
    assert_eq!(
        svm.get_balance(&d[0]).unwrap_or(0),
        LAMPORTS_PER_SOL,
        "destination must actually receive the lamports (economic reality)"
    );
}

/// Happy path: a realistic bundle (1 real leg + several decoys, all identical in
/// shape) executes atomically and every destination is funded. This is the shape
/// the SDK produces.
#[test]
fn multi_leg_bundle_succeeds_and_distributes() {
    let (mut svm, user) = setup();
    let d = dests(4);
    let legs = vec![
        Leg {
            amount: 250_000_000,
        }, // decoy
        Leg { amount: 1_337_000 }, // "real" — indistinguishable to the program
        Leg { amount: 42_000_000 }, // decoy
        Leg { amount: 90_000_000 }, // decoy
    ];
    let amounts: Vec<u64> = legs.iter().map(|l| l.amount).collect();
    let start = svm.get_balance(&user.pubkey()).unwrap();
    let res = send_bundle(&mut svm, &user, legs, &d);
    assert!(res.is_ok(), "multi-leg bundle should succeed: {res:?}");

    // Every destination received exactly its paired amount.
    for (dest, amt) in d.iter().zip(amounts.iter()) {
        assert_eq!(svm.get_balance(dest).unwrap_or(0), *amt, "each dest funded");
    }
    // The user paid out principal + fee (this is real value movement, by design).
    let moved: u64 = amounts.iter().sum();
    let end = svm.get_balance(&user.pubkey()).unwrap();
    assert!(
        start - end >= moved,
        "user paid at least the moved principal"
    );
    assert!(start - end < moved + 100_000, "…plus only a tx fee");
}

/// I4 / EmptyBundle: an empty bundle is rejected (fail-closed).
#[test]
fn empty_bundle_rejected() {
    let (mut svm, user) = setup();
    let res = send_bundle(&mut svm, &user, vec![], &[]);
    assert!(res.is_err(), "empty bundle must be rejected");
}

/// TooManyLegs: a bundle above MAX_LEGS is rejected.
#[test]
fn too_many_legs_rejected() {
    let (mut svm, user) = setup();
    let legs = vec![Leg { amount: 1_000 }; MAX_LEGS + 1];
    let d = dests(MAX_LEGS + 1);
    let res = send_bundle(&mut svm, &user, legs, &d);
    assert!(res.is_err(), "bundle over MAX_LEGS must be rejected");
}

/// Structural uniformity: legs and destinations must be 1:1. A mismatch is
/// rejected (fail-closed) — the program will not guess a pairing.
#[test]
fn account_count_mismatch_rejected() {
    let (mut svm, user) = setup();
    let legs = vec![Leg { amount: 1_000 }, Leg { amount: 2_000 }];
    let d = dests(1); // one destination for two legs
    let res = send_bundle(&mut svm, &user, legs, &d);
    assert!(
        res.is_err(),
        "leg/destination count mismatch must be rejected"
    );
}

/// ZeroAmount: a leg that moves nothing is rejected — a zero-value leg would be a
/// trivially-filterable decoy, so the program refuses it.
#[test]
fn zero_amount_leg_rejected() {
    let (mut svm, user) = setup();
    let d = dests(1);
    let res = send_bundle(&mut svm, &user, vec![Leg { amount: 0 }], &d);
    assert!(res.is_err(), "zero-amount leg must be rejected");
}

/// SelfDestination: a leg paying the signer itself is an economically pointless
/// tell and is rejected.
#[test]
fn self_destination_rejected() {
    let (mut svm, user) = setup();
    let res = send_bundle(
        &mut svm,
        &user,
        vec![Leg { amount: 1_000_000 }],
        &[user.pubkey()],
    );
    assert!(res.is_err(), "self-destination leg must be rejected");
}

/// I3/I4 fail-closed atomicity: if any leg cannot pay, the WHOLE bundle reverts
/// and no destination is funded — the real leg is never exposed without its
/// decoys.
#[test]
fn insufficient_funds_reverts_whole_bundle() {
    let (mut svm, user) = setup();
    let d = dests(3);
    // First two legs are payable; the third exceeds the balance. All-or-nothing:
    // the first two must NOT land.
    let legs = vec![
        Leg { amount: 1_000_000 },
        Leg { amount: 1_000_000 },
        Leg {
            amount: 1_000 * LAMPORTS_PER_SOL,
        }, // more than airdropped
    ];
    let res = send_bundle(&mut svm, &user, legs, &d);
    assert!(res.is_err(), "over-balance leg must fail the bundle");
    for dest in &d {
        assert_eq!(
            svm.get_balance(dest).unwrap_or(0),
            0,
            "no destination may be funded when the bundle reverts (atomicity)"
        );
    }
}
