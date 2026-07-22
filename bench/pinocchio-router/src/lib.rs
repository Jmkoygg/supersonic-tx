//! Pinocchio reimplementation of the `supersonic-tx` router core, for the
//! Anchor-vs-Pinocchio benchmark. Same work as the Anchor `execute_bundle`: for each
//! leg, a System-Program transfer from the signer to a destination, atomically.
//!
//! Instruction data layout (a compact manual encoding — no Borsh/discriminator):
//!   `[count: u8][count × u64 LE amounts]`
//! Accounts: `[0]` = payer/signer, `[1..1+count]` = destinations (in leg order),
//! `[1+count]` = System Program. The program's own logic never reads that last
//! account (`Transfer::invoke()` only takes `from`/`to`), but the runtime still
//! needs the CPI target present among the calling instruction's accounts to
//! resolve it — confirmed empirically via Mollusk (dropping it fails every
//! successful transfer with `NotEnoughAccountKeys`), not assumed from the
//! pinocchio-system source alone.
//!
//! This is a benchmark artifact, not the shipped program. It exists to measure the
//! framework overhead (binary size → rent, and compute units vs. Anchor), which the
//! research showed is a *fixed* per-call saving that matters most at small leg counts.

#![no_std]

use pinocchio::{entrypoint, error::ProgramError, AccountView, Address, ProgramResult};
use pinocchio_system::instructions::Transfer;

entrypoint!(process_instruction);
// no_std program: the entrypoint macro sets up the allocator; we only add a panic handler.
pinocchio::nostd_panic_handler!();

pub const MAX_LEGS: usize = 16;

pub fn process_instruction(
    _program_id: &Address,
    accounts: &mut [AccountView],
    data: &[u8],
) -> ProgramResult {
    if data.is_empty() {
        return Err(ProgramError::InvalidInstructionData);
    }
    let count = data[0] as usize;
    if count == 0 || count > MAX_LEGS {
        return Err(ProgramError::InvalidInstructionData);
    }
    if data.len() != 1 + count * 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    // Structural uniformity, matching the Anchor program's `dests.len() ==
    // legs.len()` exactly: not just "at least" (the original check), extra
    // accounts are rejected too. `+1` for the trailing System Program account
    // the CPI target resolution needs (see the module doc).
    if accounts.len() != 2 + count {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let accounts: &[AccountView] = accounts;
    let payer = &accounts[0];
    if !payer.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    // Defense in depth: the runtime already refuses a system transfer whose
    // `from`/`to` aren't writable, so this can't currently be bypassed — but
    // unlike Anchor, this program has no framework asserting it on our behalf,
    // so it shouldn't rely on that implicitly (auditor-skill AV-079).
    if !payer.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }

    for i in 0..count {
        let off = 1 + i * 8;
        let mut amt = [0u8; 8];
        amt.copy_from_slice(&data[off..off + 8]);
        let amount = u64::from_le_bytes(amt);
        if amount == 0 {
            return Err(ProgramError::InvalidInstructionData);
        }
        let dest = &accounts[1 + i];
        // Matching the Anchor program's `dest.key() != user.key()` exactly: a
        // self-send is an economically pointless tell, rejected fail-closed.
        if dest.address() == payer.address() {
            return Err(ProgramError::InvalidInstructionData);
        }
        if !dest.is_writable() {
            return Err(ProgramError::InvalidAccountData);
        }
        Transfer {
            from: payer,
            to: dest,
            lamports: amount,
        }
        .invoke()?;
    }

    Ok(())
}
