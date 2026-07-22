//! Temporary verification (mirrors the auditor's F-001 reproduction): confirms
//! the fixed sweep pattern in `warm_pool` (payer as fee-payer, kp as co-signer
//! authorizing its own full balance) actually succeeds, where the old pattern
//! (kp as its own fee-payer) provably failed by exactly the fee amount.
use litesvm::LiteSVM;
use solana_sdk::{
    signature::{Keypair, Signer},
    system_instruction,
    transaction::Transaction,
};

#[test]
fn fixed_sweep_pattern_succeeds_with_full_balance() {
    let mut svm = LiteSVM::new();
    let payer = Keypair::new();
    let kp = Keypair::new();
    svm.airdrop(&payer.pubkey(), 10_000_000_000).unwrap();

    const DUST_LAMPORTS: u64 = 5_000_000;
    let bh = svm.latest_blockhash();
    let fund_ix = system_instruction::transfer(&payer.pubkey(), &kp.pubkey(), DUST_LAMPORTS);
    let fund_tx =
        Transaction::new_signed_with_payer(&[fund_ix], Some(&payer.pubkey()), &[&payer], bh);
    svm.send_transaction(fund_tx).expect("fund must succeed");
    assert_eq!(svm.get_balance(&kp.pubkey()).unwrap(), DUST_LAMPORTS);

    let bal = svm.get_balance(&kp.pubkey()).unwrap();
    let bh = svm.latest_blockhash();
    let sweep_ix = system_instruction::transfer(&kp.pubkey(), &payer.pubkey(), bal);
    // The fix: payer is the fee-payer, kp only co-signs to authorize the move.
    let sweep_tx =
        Transaction::new_signed_with_payer(&[sweep_ix], Some(&payer.pubkey()), &[&payer, &kp], bh);
    let result = svm.send_transaction(sweep_tx);
    assert!(result.is_ok(), "fixed sweep must succeed: {result:?}");
    assert_eq!(
        svm.get_balance(&kp.pubkey()).unwrap(),
        0,
        "kp must be fully swept, unlike the old buggy pattern which always left ~fee lamports stuck"
    );
}
