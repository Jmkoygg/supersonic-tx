//! Invariant tests for the Pinocchio reimplementation of the router core
//! (`bench/pinocchio-router`), via Mollusk — the same 8 invariants the Anchor
//! program proves in `programs/supersonic-tx/tests/invariants.rs`
//! (`THREAT_MODEL.md §5`), run against the Pinocchio `.so` instead.
//!
//! This is what makes the Pinocchio side of BENCHMARK.md more than a size
//! comparison: both implementations are proven to uphold the identical set of
//! invariants, not just measured for binary size. The Pinocchio program is not
//! deployed anywhere and does not replace the live Anchor deployment — it is
//! offered as a minimal-attack-surface *option*, tested to the same bar.
//! Mirrors the Anchor tests' own style of asserting fail-closed behavior
//! generically (`res.is_err()`), not pinned to a specific error code.
//!
//! Instruction encoding (manual, no Borsh/discriminator — see `bench/pinocchio-router/src/lib.rs`):
//!   data:     `[count: u8][count × u64 LE amounts]`
//!   accounts: `[payer (signer, writable), dest_0, .., dest_{count-1} (writable)]`
//!             — no system-program account slot in the instruction's own account
//!             list; `Transfer::invoke()` only needs `from`/`to` per leg (verified
//!             against pinocchio-system 0.6.1's source), though the System
//!             Program's account must still be loaded into Mollusk's broader
//!             account set for the CPI target to resolve.

use mollusk_svm::{result::Check, Mollusk};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

const SBF_OUT_DIR: &str = "../bench/pinocchio-router/target/deploy";
const PROGRAM_NAME: &str = "supersonic_tx_pinocchio";
// Arbitrary 32-byte program label for Mollusk's registry — this program isn't
// deployed anywhere; native/Pinocchio programs (unlike Anchor's `declare_id!`)
// don't embed or check an on-chain identity, so any distinct id works here.
const PROGRAM_ID: Pubkey = Pubkey::new_from_array([0x50; 32]);
const LAMPORTS_PER_SOL: u64 = 1_000_000_000;
const MAX_LEGS: usize = 16;

fn mollusk() -> Mollusk {
    std::env::set_var("SBF_OUT_DIR", SBF_OUT_DIR);
    Mollusk::new(&PROGRAM_ID, PROGRAM_NAME)
}

fn encode(amounts: &[u64]) -> Vec<u8> {
    let mut data = vec![amounts.len() as u8];
    for a in amounts {
        data.extend_from_slice(&a.to_le_bytes());
    }
    data
}

/// Build the instruction + funded-account set for a bundle. `payer_lamports`
/// lets tests exercise the insufficient-funds path.
fn bundle(
    payer: Pubkey,
    dests: &[Pubkey],
    amounts: &[u64],
    payer_lamports: u64,
) -> (Instruction, Vec<(Pubkey, Account)>) {
    let mut accounts = vec![AccountMeta::new(payer, true)];
    let mut funded = vec![(
        payer,
        Account::new(payer_lamports, 0, &solana_sdk_ids::system_program::id()),
    )];
    for d in dests {
        accounts.push(AccountMeta::new(*d, false));
        funded.push((
            *d,
            Account::new(0, 0, &solana_sdk_ids::system_program::id()),
        ));
    }
    // The program's own logic never reads this account (`Transfer::invoke()`
    // only takes `from`/`to`), but the runtime needs the CPI target present
    // among *this instruction's* accounts to resolve it — confirmed empirically,
    // not assumed (see the module doc and lib.rs).
    accounts.push(AccountMeta::new_readonly(
        solana_sdk_ids::system_program::id(),
        false,
    ));
    let mut system_program_account = Account::new(1, 0, &solana_sdk_ids::native_loader::id());
    system_program_account.executable = true;
    funded.push((solana_sdk_ids::system_program::id(), system_program_account));

    let ix = Instruction {
        program_id: PROGRAM_ID,
        accounts,
        data: encode(amounts),
    };
    (ix, funded)
}

fn fresh_dests(n: usize) -> Vec<Pubkey> {
    (0..n)
        .map(|i| Pubkey::new_from_array([(i + 1) as u8; 32]))
        .collect()
}

fn balance_of(accounts: &[(Pubkey, Account)], key: &Pubkey) -> u64 {
    accounts
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, a)| a.lamports)
        .unwrap_or(0)
}

#[test]
fn single_leg_succeeds_and_moves_value() {
    let payer = Pubkey::new_from_array([200; 32]);
    let dests = fresh_dests(1);
    let (ix, accounts) = bundle(payer, &dests, &[LAMPORTS_PER_SOL], 10 * LAMPORTS_PER_SOL);
    let result = mollusk().process_and_validate_instruction(&ix, &accounts, &[Check::success()]);
    assert_eq!(
        balance_of(&result.resulting_accounts, &dests[0]),
        LAMPORTS_PER_SOL,
        "destination must actually receive the lamports (economic reality)"
    );
}

#[test]
fn multi_leg_bundle_succeeds_and_distributes() {
    let payer = Pubkey::new_from_array([201; 32]);
    let dests = fresh_dests(4);
    let amounts = [250_000_000u64, 1_337_000, 42_000_000, 90_000_000];
    let (ix, accounts) = bundle(payer, &dests, &amounts, 10 * LAMPORTS_PER_SOL);
    let result = mollusk().process_and_validate_instruction(&ix, &accounts, &[Check::success()]);
    for (dest, amt) in dests.iter().zip(amounts.iter()) {
        assert_eq!(
            balance_of(&result.resulting_accounts, dest),
            *amt,
            "each dest funded"
        );
    }
}

#[test]
fn empty_bundle_rejected() {
    let payer = Pubkey::new_from_array([202; 32]);
    let (ix, accounts) = bundle(payer, &[], &[], 10 * LAMPORTS_PER_SOL);
    let result = mollusk().process_instruction(&ix, &accounts);
    assert!(
        result.program_result.is_err(),
        "empty bundle must be rejected"
    );
}

#[test]
fn too_many_legs_rejected() {
    let payer = Pubkey::new_from_array([203; 32]);
    let dests = fresh_dests(MAX_LEGS + 1);
    let amounts = vec![1_000u64; MAX_LEGS + 1];
    let (ix, accounts) = bundle(payer, &dests, &amounts, 10 * LAMPORTS_PER_SOL);
    let result = mollusk().process_instruction(&ix, &accounts);
    assert!(
        result.program_result.is_err(),
        "bundle over MAX_LEGS must be rejected"
    );
}

#[test]
fn account_count_mismatch_rejected() {
    let payer = Pubkey::new_from_array([204; 32]);
    let dests = fresh_dests(1); // one destination for two legs
    let amounts = [1_000u64, 2_000];
    let (ix, accounts) = bundle(payer, &dests, &amounts, 10 * LAMPORTS_PER_SOL);
    let result = mollusk().process_instruction(&ix, &accounts);
    assert!(
        result.program_result.is_err(),
        "leg/destination count mismatch must be rejected"
    );
}

#[test]
fn zero_amount_leg_rejected() {
    let payer = Pubkey::new_from_array([205; 32]);
    let dests = fresh_dests(1);
    let (ix, accounts) = bundle(payer, &dests, &[0u64], 10 * LAMPORTS_PER_SOL);
    let result = mollusk().process_instruction(&ix, &accounts);
    assert!(
        result.program_result.is_err(),
        "zero-amount leg must be rejected"
    );
}

#[test]
fn self_destination_rejected() {
    let payer = Pubkey::new_from_array([206; 32]);
    let (ix, accounts) = bundle(payer, &[payer], &[1_000_000u64], 10 * LAMPORTS_PER_SOL);
    let result = mollusk().process_instruction(&ix, &accounts);
    assert!(
        result.program_result.is_err(),
        "self-destination leg must be rejected"
    );
}

#[test]
fn insufficient_funds_reverts_whole_bundle() {
    let payer = Pubkey::new_from_array([207; 32]);
    let dests = fresh_dests(3);
    // First two legs payable; the third exceeds the balance — all-or-nothing.
    let amounts = [1_000_000u64, 1_000_000, 1_000 * LAMPORTS_PER_SOL];
    let (ix, accounts) = bundle(payer, &dests, &amounts, 10 * LAMPORTS_PER_SOL);
    let result = mollusk().process_instruction(&ix, &accounts);
    assert!(
        result.program_result.is_err(),
        "over-balance leg must fail the bundle"
    );
    for dest in &dests {
        assert_eq!(
            balance_of(&result.resulting_accounts, dest),
            0,
            "no destination may be funded when the bundle reverts (atomicity)"
        );
    }
}
