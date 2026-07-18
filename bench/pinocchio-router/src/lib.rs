//! Pinocchio reimplementation of the `supersonic-tx` router core, for the
//! Anchor-vs-Pinocchio benchmark. Same work as the Anchor `execute_bundle`: for each
//! leg, a System-Program transfer from the signer to a destination, atomically.
//!
//! Instruction data layout (a compact manual encoding — no Borsh/discriminator):
//!   `[count: u8][count × u64 LE amounts]`
//! Accounts: `[0]` = payer/signer, `[1..1+count]` = destinations (in leg order).
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
    if accounts.len() < 1 + count {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let accounts: &[AccountView] = accounts;
    let payer = &accounts[0];
    if !payer.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
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
        Transfer {
            from: payer,
            to: dest,
            lamports: amount,
        }
        .invoke()?;
    }

    Ok(())
}
