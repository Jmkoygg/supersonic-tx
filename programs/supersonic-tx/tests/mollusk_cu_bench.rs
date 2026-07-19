//! Compute-unit benchmark for `execute_bundle`, via Mollusk (isolated instruction
//! harness, no full runtime/AccountsDB — faster and more precise than a full LiteSVM
//! transaction for pure CU measurement).
//!
//! This answers the question BENCHMARK.md left open: "how does CU scale with the
//! number of legs (N)?" The framework-overhead saving Pinocchio would remove is a
//! *fixed* per-instruction cost (parsing/discriminator), so it should look like a
//! constant gap between Anchor and Pinocchio, not a gap that grows with N — this
//! measures the Anchor side of that curve for real, so a future Pinocchio
//! measurement has a concrete baseline to compare against instead of a guess.
//!
//! Note: Mollusk 0.14 depends on a newer generation of `solana-pubkey`/
//! `solana-instruction`/`solana-account` than our SDK/CLI (`solana-sdk` 2.2) does —
//! the two `Pubkey`/`Instruction` types are not interchangeable, so this test builds
//! the Mollusk-side instruction directly (reusing only the SDK's plan data and raw
//! instruction-data encoding, both version-agnostic) rather than the SDK's
//! `build_instruction` helper, which returns the older-generation `Instruction`.

use mollusk_svm::{result::Check, Mollusk};
use mollusk_svm_bencher::MolluskComputeUnitBencher;
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use supersonic_sdk::{execute_bundle_data, plan_bundle, DecoyConfig};

const SBF_OUT_DIR: &str = "../../target/deploy";
const PROGRAM_NAME: &str = "supersonic_tx";
const PROGRAM_ID: &str = "BCrR3JKi5EWhC5DuKYzV4EX7ogawoWaoKkhSqZYeYabn";
const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

fn mollusk() -> Mollusk {
    std::env::set_var("SBF_OUT_DIR", SBF_OUT_DIR);
    let program_id: Pubkey = PROGRAM_ID.parse().unwrap();
    Mollusk::new(&program_id, PROGRAM_NAME)
}

/// Build a real bundle instruction — same plan/encoding the SDK, CLI, and harness all
/// use — against the Mollusk-generation account/instruction types, plus the funded
/// accounts Mollusk needs to execute it: the payer and one fresh, unfunded
/// destination per leg.
fn bundle_ix_and_accounts(program_id: Pubkey, k: usize) -> (Instruction, Vec<(Pubkey, Account)>) {
    // The SDK's own Pubkey type (older solana-sdk generation) — only used to compute
    // the plan; converted to the Mollusk-generation Pubkey via raw bytes below, which
    // is safe because the 32-byte address representation is identical across
    // generations.
    let sdk_payer = solana_sdk::pubkey::Pubkey::new_unique();
    let sdk_real_dest = solana_sdk::pubkey::Pubkey::new_unique();
    let master_seed = [7u8; 32];
    let plan = plan_bundle(
        &master_seed,
        1,
        sdk_real_dest,
        1_337_000,
        k,
        DecoyConfig::default(),
    )
    .expect("valid bundle params");

    let to_new = |p: solana_sdk::pubkey::Pubkey| Pubkey::new_from_array(p.to_bytes());
    let payer = to_new(sdk_payer);
    let system_program = solana_sdk_ids::system_program::id();

    let data = execute_bundle_data(&plan.amounts());
    let mut accounts = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new_readonly(system_program, false),
    ];
    // Mollusk only auto-resolves the top-level `program_id`'s own account (the one
    // registered via `Mollusk::new`); every other referenced account — including
    // builtins like the System Program CPI target here — must be supplied
    // explicitly, correctly marked executable and owned by the native loader.
    let mut system_program_account = Account::new(1, 0, &solana_sdk_ids::native_loader::id());
    system_program_account.executable = true;
    let mut dest_accounts = vec![
        (
            payer,
            Account::new(10 * LAMPORTS_PER_SOL, 0, &system_program),
        ),
        (system_program, system_program_account),
    ];
    for dest in plan.destinations() {
        let dest = to_new(dest);
        accounts.push(AccountMeta::new(dest, false));
        dest_accounts.push((dest, Account::new(0, 0, &system_program)));
    }

    let ix = Instruction {
        program_id,
        accounts,
        data,
    };
    (ix, dest_accounts)
}

/// Sanity check: the real bundle instruction actually succeeds under Mollusk before
/// we trust any CU number it reports.
#[test]
fn bundle_executes_successfully_under_mollusk() {
    let program_id: Pubkey = PROGRAM_ID.parse().unwrap();
    let (ix, accounts) = bundle_ix_and_accounts(program_id, 8);
    mollusk().process_and_validate_instruction(&ix, &accounts, &[Check::success()]);
}

/// The CU-vs-N curve: how compute units scale with leg count. Writes a markdown
/// report to target/benches, matching BENCHMARK.md's "natural validation step."
#[test]
fn cu_scales_with_leg_count() {
    let program_id: Pubkey = PROGRAM_ID.parse().unwrap();
    let (ix2, acc2) = bundle_ix_and_accounts(program_id, 2);
    let (ix4, acc4) = bundle_ix_and_accounts(program_id, 4);
    let (ix8, acc8) = bundle_ix_and_accounts(program_id, 8);
    let (ix16, acc16) = bundle_ix_and_accounts(program_id, 16);

    MolluskComputeUnitBencher::new(mollusk())
        .bench(("execute_bundle_k2", &ix2, &acc2))
        .bench(("execute_bundle_k4", &ix4, &acc4))
        .bench(("execute_bundle_k8", &ix8, &acc8))
        .bench(("execute_bundle_k16", &ix16, &acc16))
        .must_pass(true)
        .out_dir("../../target/benches")
        .execute();
}
