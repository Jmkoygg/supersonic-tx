use anchor_lang::prelude::*;
use anchor_lang::system_program::{transfer, Transfer};

declare_id!("BCrR3JKi5EWhC5DuKYzV4EX7ogawoWaoKkhSqZYeYabn");

/// Upper bound on legs per bundle. Bounds compute and keeps the tx within the
/// account and size limits of a single transaction.
pub const MAX_LEGS: usize = 16;

/// Lower bound on legs per bundle. A single-leg bundle has no decoys, so it
/// advertises "this used the privacy tool" without hiding anything — worse
/// than a plain transfer. At least one decoy (K >= 2) is required.
pub const MIN_LEGS: usize = 2;

#[program]
pub mod supersonic_tx {
    use super::*;

    /// Execute an atomic bundle of structurally-identical transfer legs.
    ///
    /// Each leg moves `amount` lamports from the signer to a destination provided
    /// in `remaining_accounts`, in the same order as `legs`. Real and decoy legs
    /// are byte-for-byte identical in shape, and the program is deliberately
    /// **oblivious** to which leg is the user's real intent.
    ///
    /// # What the program guarantees (see THREAT_MODEL.md §5)
    /// - **I3 Atomicity:** all legs settle or the whole bundle reverts.
    /// - **I4 Fail-closed:** any malformed leg (zero amount, self-send, bad
    ///   account count, insufficient funds) reverts the entire bundle; no leg
    ///   moves funds partially.
    /// - **Uniformity:** every leg goes through the identical code path, so the
    ///   program cannot and does not treat the real leg differently.
    ///
    /// # What the program deliberately does NOT do
    /// It does not inspect, classify, or constrain destinations. Checking whether
    /// a destination is "the user's own" would put an on-chain marker on decoys
    /// and leak exactly what we hide. Decoy recoverability (I2) and real/decoy
    /// semantics live in the off-chain SDK, which sends decoys only to
    /// user-controlled destinations. The program is a neutral atomic executor.
    pub fn execute_bundle<'info>(
        ctx: Context<'_, '_, '_, 'info, ExecuteBundle<'info>>,
        legs: Vec<Leg>,
    ) -> Result<()> {
        require!(!legs.is_empty(), SupersonicError::EmptyBundle);
        require!(legs.len() >= MIN_LEGS, SupersonicError::TooFewLegs);
        require!(legs.len() <= MAX_LEGS, SupersonicError::TooManyLegs);

        let user = &ctx.accounts.user;
        let system_program = &ctx.accounts.system_program;
        let dests = ctx.remaining_accounts;

        // Structural uniformity: exactly one destination per leg, same order.
        require!(
            dests.len() == legs.len(),
            SupersonicError::AccountCountMismatch
        );

        for (leg, dest) in legs.iter().zip(dests.iter()) {
            // I4: a zero-value leg is a trivially-filterable decoy — reject.
            require!(leg.amount > 0, SupersonicError::ZeroAmount);
            // I4: a self-send is an economically pointless tell — reject.
            require!(dest.key() != user.key(), SupersonicError::SelfDestination);

            // The user signs the outer transaction; this CPI moves the user's own
            // lamports to `dest`. If the user lacks funds, the CPI fails and — by
            // I3 — the whole bundle reverts (fail-closed).
            transfer(
                CpiContext::new(
                    system_program.to_account_info(),
                    Transfer {
                        from: user.to_account_info(),
                        to: dest.to_account_info(),
                    },
                ),
                leg.amount,
            )?;
        }

        Ok(())
    }
}

/// A single leg of a bundle.
///
/// Structurally identical for real and decoy legs: the program cannot tell them
/// apart, and an observer cannot separate them by the leg's on-chain shape. Only
/// the off-chain SDK knows which index carries the user's real intent and which
/// destinations are recoverable decoys.
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct Leg {
    /// Lamports moved by this leg to its paired destination.
    pub amount: u64,
}

#[derive(Accounts)]
pub struct ExecuteBundle<'info> {
    #[account(mut)]
    pub user: Signer<'info>,

    pub system_program: Program<'info, System>,
    // Destinations are passed as `remaining_accounts`, one writable account per
    // leg, in leg order. They are intentionally untyped and unconstrained so that
    // real and decoy destinations are indistinguishable at the program interface.
}

#[error_code]
pub enum SupersonicError {
    #[msg("Bundle must contain at least one leg")]
    EmptyBundle,
    #[msg("Bundle must contain at least two legs — a single-leg bundle reveals tool usage without hiding anything")]
    TooFewLegs,
    #[msg("Bundle exceeds the maximum number of legs")]
    TooManyLegs,
    #[msg("Number of destination accounts must equal the number of legs")]
    AccountCountMismatch,
    #[msg("Leg amount must be greater than zero")]
    ZeroAmount,
    #[msg("Leg destination must not be the signer")]
    SelfDestination,
}
