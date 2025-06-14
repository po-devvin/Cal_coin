use anchor_lang::prelude::*;
use anchor_lang::prelude::InterfaceAccount;
use anchor_lang::system_program;
use anchor_spl::{
    token_2022::{self as token_2022, mint_to, ID as TOKEN_2022_PROGRAM_ID},
    associated_token::AssociatedToken,
    token_interface::{Mint, Token2022, TokenAccount},
};
use solana_gateway::Gateway;
use anchor_spl::token_2022::MintTo;
use std::str::FromStr;

declare_id!("8gKV6NRFMcaVfxSCofsNQK4AHxm5rFXzjvBvPgKCqENjf");

// ------------------------------------------------------------------------------------------------
// Constants
// ------------------------------------------------------------------------------------------------

/// Seed prefix for mint authority PDA.
const MINT_AUTHORITY_SEED_V1: &[u8] = b"cal_coin_mint_authority_v1";

// 3 days in seconds
const MAX_ACCUMULATION_SECONDS: i64 = 3 * 24 * 60 * 60; // 259200

/// Normal user accrual: 20,833 microtokens per second (~1 token per minute).
const USER_RATE_PER_SEC: u64 = 20_833;
/// Exempt user accrual: 42× the normal rate.
const EXEMPT_RATE_PER_SEC: u64 = 910_170;

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

    /// One-time registration for a user.
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

        msg!(
            "Registered user => user_pda={}, authority={}",
            user_pda.key(),
            user_key
        );
        Ok(())
    }

    /// Claim tokens repeatedly.
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

    /// Change the exempt address. The update can be performed by the owner initially;
    /// afterwards, only the existing exempt or owner may change it.
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
}

// ---------------------------------------------------------------------
//  STATE ACCOUNTS
// ---------------------------------------------------------------------

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

/// Per-user account tracking the last claim timestamp and total claimed so far.
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

// ---------------------------------------------------------------------
//  ACCOUNTS FOR INSTRUCTIONS
// ---------------------------------------------------------------------

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

    // PDA that will own & mint tokens
    #[account(
        init,
        payer = payer,
        space = MintAuthority::LEN,
        seeds = [b"mint_authority"],
        bump
    )]
    pub mint_authority: Account<'info, MintAuthority>,

    // SPL-Token-2022 mint
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

// ---------------------------------------------------------------------
//  ERRORS
// ---------------------------------------------------------------------

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
} 
