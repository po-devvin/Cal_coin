use anchor_lang::prelude::*;
use anchor_lang::prelude::InterfaceAccount;
use anchor_lang::system_program;
use anchor_spl::{
    token_2022::{self as token_2022, mint_to, transfer as token_transfer, ID as TOKEN_2022_PROGRAM_ID},
    associated_token::AssociatedToken,
    token_interface::{Mint, Token2022, TokenAccount},
};
use solana_gateway::Gateway;
use anchor_spl::token_2022::{MintTo, Transfer};
use std::str::FromStr;

declare_id!("AFrYiV7fCPEVCbCXktrmGW9YuNPboaPUmFWTca3UTqZp");

// ------------------------------------------------------------------------------------------------
// Constants
// ------------------------------------------------------------------------------------------------

/// Seed prefix for mint authority PDA.
const MINT_AUTHORITY_SEED_V1: &[u8] = b"cal_coin_mint_authority_v1";

/// Seed prefix for stake vault ATA PDA.
const STAKE_VAULT_SEED: &[u8] = b"stake_vault";

/// 3 days in seconds (for claim delay in unstaking).
const UNSTAKE_DELAY_SECONDS: i64 = 2 * 24 * 60 * 60; // 2 days

/// Minimum stake: 1,800 tokens = 1_800 * 10^9 (9 decimals).
const MIN_STAKE_LAMPORTS: u64 = 1_800 * 10u64.pow(9);

/// APY 5.55% expressed as numerator/denominator for integer math.
const APY_NUMERATOR: u128 = 555;     // 5.55% → 0.0555 = 555 / 10000
const APY_DENOMINATOR: u128 = 10000;
// 3 days in seconds
const MAX_ACCUMULATION_SECONDS: i64 = 3 * 24 * 60 * 60; // 259200

/// Normal user accrual: 20,833 microtokens per second (~1 token per minute).
const USER_RATE_PER_SEC: u64 = 20_833;
/// Exempt user accrual: 42× the normal rate.
const EXEMPT_RATE_PER_SEC: u64 = 910_170;

/// Seconds in one year, for APR calculation.
const SECONDS_PER_YEAR: u128 = 365 * 24 * 60 * 60;

// ------------------------------------------------------------------------------------------------
// Program Entrypoint
// ------------------------------------------------------------------------------------------------

#[program]
pub mod cal_coin {
    use super::*;

    /// Initialize the dapp configuration.
    pub fn initialize_dapp(ctx: Context<InitializeDapp>) -> Result<()> {
        let cfg = &mut ctx.accounts.dapp_config;

        // Block double-init
        require!(!cfg.initialized, ErrorCode::AlreadyInitialized);

        cfg.owner = ctx.accounts.payer.key();
        cfg.gatekeeper_network = Pubkey::from_str("uniqobk8oGh4XBLMqM68K8M2zNu3CdYX7q5go7whQiv").unwrap();
        cfg.exempt_address = Pubkey::default();
        cfg.token_mint = Pubkey::default();       // filled in phase 2
        cfg.mint_authority_bump = 0;              // filled in phase 2
        cfg.initialized = false;

        // Initialize supply controls
        cfg.total_minted = 0;
        cfg.max_supply = 1_000_000_000_000_000;   // example max supply in microtokens

        // Start claim‐counter at zero
        cfg.total_claims = 0;

        msg!("Dapp config stored; run initialize_mint next.");
        Ok(())
    }

    /// Phase 2: Create the SPL Token 2022 mint with the PDA as authority.
    pub fn initialize_mint(
        ctx: Context<InitializeMint>,
        decimals: u8,
    ) -> Result<()> {
        let cfg = &mut ctx.accounts.dapp_config;
        require!(!cfg.initialized, ErrorCode::AlreadyInitialized);

        // Save PDA bump and mint address
        cfg.mint_authority_bump = ctx.bumps.mint_authority;
        cfg.token_mint = ctx.accounts.mint_for_dapp.key();
        cfg.initialized = true; // config complete

        msg!(
            "Mint {} created; dapp fully initialized.",
            cfg.token_mint
        );
        Ok(())
    }

    /// One-time registration for a user, creates their UserPda and StakeAccount.
    pub fn register_user(ctx: Context<RegisterUser>) -> Result<()> {
        let cfg = &ctx.accounts.dapp_config;
        let user_key = ctx.accounts.user.key();

        if user_key != cfg.exempt_address {
            // Use on-chain gatekeeper_network instead of hardcoded literal
            let gatekeeper_network = cfg.gatekeeper_network;
            Gateway::verify_gateway_token_account_info(
                &ctx.accounts.gateway_token.to_account_info(),
                &user_key,
                &gatekeeper_network,
                None,
            )
            .map_err(|_e| {
                msg!("Gateway token verification failed");
                error!(ErrorCode::GatewayCheckFailed)
            })?;
        } else {
            msg!("User is exempt => skipping gateway check in register_user");
        }

        let user_pda = &mut ctx.accounts.user_pda;
        user_pda.authority = user_key;
        user_pda.last_claimed_timestamp = 0;
        user_pda.claimed_so_far = 0;

        // Initialize their StakeAccount
        let stake = &mut ctx.accounts.stake_account;
        stake.authority = user_key;
        stake.stake_amount = 0;
        stake.last_reward_timestamp = Clock::get()?.unix_timestamp;
        stake.pending_withdrawal_amount = 0;
        stake.withdraw_request_timestamp = 0;

        msg!(
            "Registered user => user_pda={}, authority={}",
            user_pda.key(),
            user_key
        );
        Ok(())
    }

    /// Claim faucet tokens (same as before).
    pub fn claim(ctx: Context<Claim>) -> Result<()> {
        let cfg = &mut ctx.accounts.dapp_config;
        let user_key = ctx.accounts.user.key();
        let user_pda = &mut ctx.accounts.user_pda;

        if user_key != cfg.exempt_address {
            let gatekeeper_network = cfg.gatekeeper_network;
            Gateway::verify_gateway_token_account_info(
                &ctx.accounts.gateway_token.to_account_info(),
                &user_key,
                &gatekeeper_network,
                None,
            )
            .map_err(|_e| {
                msg!("Gateway token verification failed");
                error!(ErrorCode::GatewayCheckFailed)
            })?;
        } else {
            msg!("User is exempt => skipping gateway check in claim");
        }

        // Update the total_claims counter
        cfg.total_claims = cfg
            .total_claims
            .checked_add(1)
            .ok_or(ErrorCode::IssuanceRateTooHigh)?;
        
        let now = Clock::get()?.unix_timestamp;
        let raw_elapsed = now.saturating_sub(user_pda.last_claimed_timestamp);
        require!(raw_elapsed >= 60, ErrorCode::CooldownNotMet);

        // Cap elapsed at 3 days (259,200 seconds)
        let elapsed = std::cmp::min(raw_elapsed, MAX_ACCUMULATION_SECONDS);

        // Determine per-second rate: exempt = 42×, else normal
        let tokens_per_second_micro = if user_key == cfg.exempt_address {
            EXEMPT_RATE_PER_SEC
        } else {
            USER_RATE_PER_SEC
        };

        let minted_amount = tokens_per_second_micro
            .checked_mul(elapsed as u64)
            .ok_or(ErrorCode::IssuanceRateTooHigh)?;
        if minted_amount == 0 {
            msg!("No tokens to mint; skipping claim.");
            return Ok(());
        }

        // Enforce global max supply cap
        let new_total = cfg
            .total_minted
            .checked_add(minted_amount)
            .ok_or(ErrorCode::IssuanceRateTooHigh)?;
        require!(new_total <= cfg.max_supply, ErrorCode::SupplyExceeded);
        cfg.total_minted = new_total;

        // Track per-user total claimed
        let new_user_total = user_pda
            .claimed_so_far
            .checked_add(minted_amount)
            .ok_or(ErrorCode::IssuanceRateTooHigh)?;
        user_pda.claimed_so_far = new_user_total;

        // Perform the mint via CPI, with PDA signer
        let bump = cfg.mint_authority_bump;
        let seeds = &[MINT_AUTHORITY_SEED_V1, &[bump]];
        let signer_seeds = &[&seeds[..]];
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            MintTo {
                mint: ctx.accounts.token_mint.to_account_info(),
                to: ctx.accounts.user_ata.to_account_info(),
                authority: ctx.accounts.mint_authority.to_account_info(),
            },
            signer_seeds,
        );
        token_2022::mint_to(cpi_ctx, minted_amount)?;

        user_pda.last_claimed_timestamp = now;
        msg!(
            "Claim => user={}, minted={} μtokens, elapsed={}s, last_claimed={}",
            user_key,
            minted_amount,
            elapsed,
            now
        );
        Ok(())
    }

    /// Change the exempt address.
    pub fn set_exempt(ctx: Context<SetExempt>, new_exempt: Pubkey) -> Result<()> {
        let cfg = &mut ctx.accounts.dapp_config;
        let signer_key = ctx.accounts.current_exempt.key();

        if cfg.exempt_address == Pubkey::default() {
            // First-time set: only owner can define it.
            require!(signer_key == cfg.owner, ErrorCode::NotAuthorized);
        } else {
            // Once an exempt exists, only exempt or owner can update.
            require!(
                signer_key == cfg.exempt_address || signer_key == cfg.owner,
                ErrorCode::NotExemptAddress
            );
        }

        cfg.exempt_address = new_exempt;
        msg!("Exempt address updated => new_exempt={}", new_exempt);
        Ok(())
    }

    /// Stake a given amount of tokens. Must be ≥ MIN_STAKE_LAMPORTS.
    pub fn stake(ctx: Context<Stake>, amount: u64) -> Result<()> {
        let stake_acc = &mut ctx.accounts.stake_account;
        let user_key = ctx.accounts.user.key();

        // Must stake at least the minimum
        require!(amount >= MIN_STAKE_LAMPORTS, ErrorCode::StakeTooSmall);

        // Transfer tokens from user ATA to stake vault PDA
        let cpi_ctx = CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.user_ata.to_account_info(),
                to: ctx.accounts.stake_vault.to_account_info(),
                authority: ctx.accounts.user.to_account_info(),
            },
        );
        token_2022::transfer(cpi_ctx, amount)?;

        // Compute rewards on existing stake and compound
        let now = Clock::get()?.unix_timestamp;
        if stake_acc.stake_amount > 0 {
            let elapsed = (now.saturating_sub(stake_acc.last_reward_timestamp)) as u128;
            let principal = stake_acc.stake_amount as u128;
            let reward = principal
                .checked_mul(APY_NUMERATOR)
                .unwrap()
                .checked_mul(elapsed)
                .unwrap()
                .checked_div(APY_DENOMINATOR.checked_mul(SECONDS_PER_YEAR).unwrap())
                .unwrap() as u64;
            // Compound reward into stake balance
            stake_acc.stake_amount = stake_acc
                .stake_amount
                .checked_add(reward)
                .ok_or(ErrorCode::ArithmeticError)?;
        }

        // Add newly staked tokens
        stake_acc.stake_amount = stake_acc
            .stake_amount
            .checked_add(amount)
            .ok_or(ErrorCode::ArithmeticError)?;
        stake_acc.last_reward_timestamp = now;

        msg!(
            "Stake => user={}, amount_staked={}, new_total={}",
            user_key,
            amount,
            stake_acc.stake_amount
        );
        Ok(())
    }

    /// Request to unstake all tokens. After 2 days, user can call `claim_stake`.
    pub fn request_unstake(ctx: Context<RequestUnstake>) -> Result<()> {
        let stake_acc = &mut ctx.accounts.stake_account;
        let user_key = ctx.accounts.user.key();
        let now = Clock::get()?.unix_timestamp;

        require!(stake_acc.stake_amount > 0, ErrorCode::NothingToUnstake);
        require!(
            stake_acc.pending_withdrawal_amount == 0,
            ErrorCode::UnstakeAlreadyRequested
        );

        // Compute rewards up to now
        let elapsed = (now.saturating_sub(stake_acc.last_reward_timestamp)) as u128;
        let principal = stake_acc.stake_amount as u128;
        let reward = principal
            .checked_mul(APY_NUMERATOR)
            .unwrap()
            .checked_mul(elapsed)
            .unwrap()
            .checked_div(APY_DENOMINATOR.checked_mul(SECONDS_PER_YEAR).unwrap())
            .unwrap() as u64;

        // Total to withdraw = stake + reward
        let total_withdraw = stake_acc
            .stake_amount
            .checked_add(reward)
            .ok_or(ErrorCode::ArithmeticError)?;

        stake_acc.pending_withdrawal_amount = total_withdraw;
        stake_acc.stake_amount = 0;
        stake_acc.withdraw_request_timestamp = now;
        stake_acc.last_reward_timestamp = now;

        msg!(
            "Unstake requested => user={}, amount_pending={}, can_claim_at={}",
            user_key,
            total_withdraw,
            now + UNSTAKE_DELAY_SECONDS
        );
        Ok(())
    }

    /// After 2 days from unstake request, claim tokens back to user's ATA.
    pub fn claim_stake(ctx: Context<ClaimStake>) -> Result<()> {
        let stake_acc = &mut ctx.accounts.stake_account;
        let user_key = ctx.accounts.user.key();
        let now = Clock::get()?.unix_timestamp;

        require!(
            stake_acc.pending_withdrawal_amount > 0,
            ErrorCode::NothingToClaim
        );
        require!(
            now >= stake_acc.withdraw_request_timestamp + UNSTAKE_DELAY_SECONDS,
            ErrorCode::UnstakeDelayNotMet
        );

        let amount = stake_acc.pending_withdrawal_amount;

        // Transfer from stake vault PDA to user ATA (vault authority is PDA signer)
        // grab the user Pubkey once, so `.as_ref()` borrows from a named variable
        let user_key = ctx.accounts.user.key();
        let (_vault_authority, vault_bump) =
            Pubkey::find_program_address(&[STAKE_VAULT_SEED, user_key.as_ref()], ctx.program_id);
        let vault_seeds = &[STAKE_VAULT_SEED, user_key.as_ref(), &[vault_bump]];
        let signer_seeds = &[&vault_seeds[..]];
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.stake_vault.to_account_info(),
                to: ctx.accounts.user_ata.to_account_info(),
                authority: ctx.accounts.vault_authority.to_account_info(),
            },
            signer_seeds,
        );
        token_2022::transfer(cpi_ctx, amount)?;

        // Reset pending fields
        stake_acc.pending_withdrawal_amount = 0;
        stake_acc.withdraw_request_timestamp = 0;
        stake_acc.last_reward_timestamp = now;

        msg!(
            "Claimed stake => user={}, amount_returned={}",
            user_key,
            amount
        );
        Ok(())
    }
}

// ------------------------------------------------------------------------------------------------
//  STATE ACCOUNTS
// ------------------------------------------------------------------------------------------------

#[account]
pub struct DappConfig {
    pub gatekeeper_network: Pubkey,  // Gatekeeper network ID.
    pub token_mint: Pubkey,          // Token mint used for distribution.
    pub mint_authority_bump: u8,     // Bump for mint authority PDA.
    pub exempt_address: Pubkey,      // Exempt address.
    pub owner: Pubkey,               // Owner of the dapp.
    pub initialized: bool,           // Flag to block re-initializing.

    // Supply control fields
    pub total_minted: u64,           // Total microtokens minted so far.
    pub max_supply: u64,             // Maximum microtoken supply.

    // Claim counter
    pub total_claims: u64,           // Counts how many times `claim` was invoked
}

impl DappConfig {
    // 8 discriminator + 32 + 32 + 1 + 32 + 32 + 1 + 8 + 8 + 8 = 170 bytes
    pub const LEN: usize = 8 + 32 + 32 + 1 + 32 + 32 + 1 + 8 + 8 + 8;
}

#[account]
pub struct MintAuthority {
    pub bump: u8,
}

impl MintAuthority {
    pub const LEN: usize = 8 + 1;
}

/// Per-user account tracking faucet claims.
#[account]
pub struct UserPda {
    pub authority: Pubkey,
    pub last_claimed_timestamp: i64,
    pub claimed_so_far: u64,
}

impl UserPda {
    // 8 discriminator + 32 + 8 + 8 = 56 bytes
    pub const LEN: usize = 8 + 32 + 8 + 8;
}

/// Per-user staking account.
#[account]
pub struct StakeAccount {
    pub authority: Pubkey,             // User who owns this stake
    pub stake_amount: u64,             // Currently staked balance
    pub last_reward_timestamp: i64,    // Last time rewards were calculated
    pub pending_withdrawal_amount: u64,// Amount locked for withdrawal
    pub withdraw_request_timestamp: i64,// When unstake was requested
}

impl StakeAccount {
    // 8 discriminator + 32 + 8 + 8 + 8 + 8 = 72 bytes
    pub const LEN: usize = 8 + 32 + 8 + 8 + 8 + 8;
}

// ------------------------------------------------------------------------------------------------
//  ACCOUNTS FOR INSTRUCTIONS
// ------------------------------------------------------------------------------------------------

#[derive(Accounts)]
pub struct InitializeDapp<'info> {
    #[account(
        init,
        payer = payer,
        space = DappConfig::LEN,
        seeds = [b"dapp_config"],
        bump
    )]
    pub dapp_config: Account<'info, DappConfig>,

    #[account(mut)]
    pub payer: Signer<'info>,

    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct InitializeMint<'info> {
    #[account(
        mut,
        seeds = [b"dapp_config"],
        bump,
    )]
    pub dapp_config: Account<'info, DappConfig>,

    /// PDA that will own & mint tokens
    #[account(
        init,
        payer = payer,
        space = MintAuthority::LEN,
        seeds = [b"mint_authority"],
        bump
    )]
    pub mint_authority: Account<'info, MintAuthority>,

    /// SPL-Token-2022 mint
    #[account(
        init,
        payer = payer,
        mint::decimals = 9,
        mint::authority = mint_authority,
        mint::freeze_authority = mint_authority,
        owner = token_program.key(),
    )]
    pub mint_for_dapp: InterfaceAccount<'info, Mint>,

    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(address = TOKEN_2022_PROGRAM_ID)]
    pub token_program: Program<'info, Token2022>,

    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct RegisterUser<'info> {
    #[account(
        seeds = [b"dapp_config"],
        bump
    )]
    pub dapp_config: Account<'info, DappConfig>,

    #[account(mut)]
    pub user: Signer<'info>,

    pub gateway_token: UncheckedAccount<'info>,

    #[account(
        init,
        payer = user,
        space = UserPda::LEN,
        seeds = [b"user_pda", user.key().as_ref()],
        bump
    )]
    pub user_pda: Account<'info, UserPda>,

    /// Initialize staking account for this user
    #[account(
        init,
        payer = user,
        space = StakeAccount::LEN,
        seeds = [b"stake_account", user.key().as_ref()],
        bump
    )]
    pub stake_account: Account<'info, StakeAccount>,

    #[account(address = TOKEN_2022_PROGRAM_ID)]
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct Claim<'info> {
    #[account(
        seeds = [b"dapp_config"],
        bump
    )]
    pub dapp_config: Account<'info, DappConfig>,

    #[account(mut)]
    pub user: Signer<'info>,

    pub gateway_token: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [b"user_pda", user.key().as_ref()],
        bump
    )]
    pub user_pda: Account<'info, UserPda>,

    #[account(
        mut,
        constraint = token_mint.key() == dapp_config.token_mint
    )]
    pub token_mint: InterfaceAccount<'info, Mint>,

    #[account(
        seeds = [b"mint_authority"],
        bump = dapp_config.mint_authority_bump,
    )]
    pub mint_authority: Account<'info, MintAuthority>,

    #[account(
        init_if_needed,
        payer = user,
        associated_token::mint = token_mint,
        associated_token::authority = user
    )]
    pub user_ata: InterfaceAccount<'info, TokenAccount>,

    #[account(address = TOKEN_2022_PROGRAM_ID)]
    pub token_program: Program<'info, Token2022>,

    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct SetExempt<'info> {
    #[account(
        mut,
        seeds = [b"dapp_config"],
        bump
    )]
    pub dapp_config: Account<'info, DappConfig>,

    #[account(mut)]
    pub current_exempt: Signer<'info>,
}

#[derive(Accounts)]
pub struct Stake<'info> {
    #[account(
        seeds = [b"dapp_config"],
        bump
    )]
    pub dapp_config: Account<'info, DappConfig>,

    #[account(mut)]
    pub user: Signer<'info>,

    #[account(
        mut,
        seeds = [b"stake_account", user.key().as_ref()],
        bump
    )]
    pub stake_account: Account<'info, StakeAccount>,

    /// Vault PDA associated token account for holding staked tokens
    #[account(
            init_if_needed,
            payer = user,
            associated_token::mint = token_mint,
            associated_token::authority = vault_authority,
        )]
    pub stake_vault: InterfaceAccount<'info, TokenAccount>,

    /// PDA that signs for transferring from vault
    #[account(
        seeds = [STAKE_VAULT_SEED, user.key().as_ref()],
        bump
    )]
    pub vault_authority: AccountInfo<'info>,

    /// User's token account (source of stake)
    #[account(
        mut,
        associated_token::mint = token_mint,
        associated_token::authority = user
    )]
    pub user_ata: InterfaceAccount<'info, TokenAccount>,

    /// The mint for cal_coin
    #[account(constraint = token_mint.key() == dapp_config.token_mint)]
    pub token_mint: InterfaceAccount<'info, Mint>,

    #[account(address = TOKEN_2022_PROGRAM_ID)]
    pub token_program: Program<'info, Token2022>,

    pub system_program: Program<'info, System>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct RequestUnstake<'info> {
    #[account(
        seeds = [b"dapp_config"],
        bump
    )]
    pub dapp_config: Account<'info, DappConfig>,

    #[account(mut)]
    pub user: Signer<'info>,

    #[account(
        mut,
        seeds = [b"stake_account", user.key().as_ref()],
        bump
    )]
    pub stake_account: Account<'info, StakeAccount>,

    /// Vault PDA authority (for potential later transfers)
    #[account(
        seeds = [STAKE_VAULT_SEED, user.key().as_ref()],
        bump
    )]
    pub vault_authority: AccountInfo<'info>,

    #[account(address = TOKEN_2022_PROGRAM_ID)]
    pub token_program: Program<'info, Token2022>,

    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct ClaimStake<'info> {
    #[account(
        seeds = [b"dapp_config"],
        bump
    )]
    pub dapp_config: Account<'info, DappConfig>,

    #[account(mut)]
    pub user: Signer<'info>,

    #[account(
        mut,
        seeds = [b"stake_account", user.key().as_ref()],
        bump
    )]
    pub stake_account: Account<'info, StakeAccount>,

    /// Vault PDA token account
    #[account(
        mut,
        associated_token::mint = token_mint,
        associated_token::authority = vault_authority,
    )]
    pub stake_vault: InterfaceAccount<'info, TokenAccount>,

    /// Vault PDA authority
    #[account(
        seeds = [STAKE_VAULT_SEED, user.key().as_ref()],
        bump
    )]
    pub vault_authority: AccountInfo<'info>,

    /// User's token account (destination for withdrawal)
    #[account(
        mut,
        associated_token::mint = token_mint,
        associated_token::authority = user
    )]
    pub user_ata: InterfaceAccount<'info, TokenAccount>,

    /// The mint for cal_coin
    #[account(constraint = token_mint.key() == dapp_config.token_mint)]
    pub token_mint: InterfaceAccount<'info, Mint>,

    #[account(address = TOKEN_2022_PROGRAM_ID)]
    pub token_program: Program<'info, Token2022>,

    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

// ------------------------------------------------------------------------------------------------
//  ERRORS
// ------------------------------------------------------------------------------------------------

#[error_code]
pub enum ErrorCode {
    #[msg("Must wait 60s before claiming again.")]
    CooldownNotMet,
    #[msg("Gateway token check failed.")]
    GatewayCheckFailed,
    #[msg("Signer is not authorized to change exempt address.")]
    NotExemptAddress,
    #[msg("Caller is not authorized for this action.")]
    NotAuthorized,
    #[msg("Dapp configuration already initialized.")]
    AlreadyInitialized,
    #[msg("Issuance rate overflowed.")]
    IssuanceRateTooHigh,
    #[msg("Supply would exceed maximum allowed.")]
    SupplyExceeded,
    #[msg("Stake amount is below minimum requirement.")]
    StakeTooSmall,
    #[msg("Arithmetic overflow occurred.")]
    ArithmeticError,
    #[msg("Nothing to unstake.")]
    NothingToUnstake,
    #[msg("Unstake already requested.")]
    UnstakeAlreadyRequested,
    #[msg("Nothing to claim from stake.")]
    NothingToClaim,
    #[msg("Unstake delay not yet met.")]
    UnstakeDelayNotMet,
}
