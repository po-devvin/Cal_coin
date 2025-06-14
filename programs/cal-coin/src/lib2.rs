use anchor_lang::prelude::*;
use anchor_lang::prelude::InterfaceAccount;
use anchor_lang::system_program;
use anchor_spl::{
    token_2022::{self as token_2022, mint_to, ID as TOKEN_2022_PROGRAM_ID},
    // The 2022 account interfaces:
    //token_interface::{Mint, MintTo, Account as TokenAccount, Token2022 as Token},
};
//use anchor_spl::token_2022::TokenAccount;
//use spl_token_2022::state::Account as TokenAccount2022;
use anchor_spl::token_interface::{
    //Account as TokenAccount2022,//GenericTokenAccount as TokenAccount,
    Mint,
    Token2022,       // The “program” type for your token_program field
};
use anchor_spl::token_interface::TokenAccount;

use solana_gateway::Gateway;

use anchor_spl::token_2022::MintTo;
//use anchor_spl::token_2022::Account;
use anchor_spl::associated_token::AssociatedToken;
use sha3::{Digest, Keccak256};
use std::convert::TryInto;
use std::str::FromStr;
declare_id!("BYJtTQxe8F1Zi41bzWRStVPf57knpst3JqvZ7P5EMjex");

#[program]
pub mod cal_coin {
    use super::*;

    /// Initialize the dapp configuration.
    ///
    /// This instruction sets up the global config by saving:
    /// - The token mint (that will be used for distribution),
    /// - The gatekeeper network for gateway checks (hard-coded here),
    /// - The bump for the mint authority PDA.
    ///
    /// It also creates (or reuses) the initializer’s associated token account (ATA)
    /// and mints `initial_mint_amount` tokens to that ATA.
    pub fn initialize_dapp(ctx: Context<InitializeDapp>, initial_mint_amount: u64) -> Result<()> {
        let dapp_config = &mut ctx.accounts.dapp_config;
        // Use a hard-coded gatekeeper network (adjust if needed)
        dapp_config.gatekeeper_network = Pubkey::from_str("uniqobk8oGh4XBLMqM68K8M2zNu3CdYX7q5go7whQiv")
            .unwrap();
        // Set the token mint from the provided token_mint account.
        dapp_config.token_mint = ctx.accounts.token_mint.key();
        // Initially, set exempt_address to default (all zeros).
        dapp_config.exempt_address = Pubkey::default();
        // Store the mint authority bump, derived from the seeds.
        dapp_config.mint_authority_bump = *ctx.bumps.get("mint_authority").unwrap();

        // The ATA for the initializer is auto-created via the associated_token constraint.
        // Mint the specified initial amount of tokens to the initializer's ATA.
        let seeds = &[b"mint_authority".as_ref(), &[dapp_config.mint_authority_bump]];
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
        token_2022::mint_to(cpi_ctx, initial_mint_amount)?;

        msg!("Dapp initialized! Token Mint: {}", dapp_config.token_mint);
        Ok(())
    }

    /// One-time registration for a user:
    /// - Skips gateway check if exempt,
    /// - Otherwise checks gateway token,
    /// - Creates a [UserPda] for storing `last_claimed_timestamp`.
    pub fn register_user(ctx: Context<RegisterUser>) -> Result<()> {
        let cfg = &ctx.accounts.dapp_config;
        let user_key = ctx.accounts.user.key();

        if user_key != cfg.exempt_address {
            let gatekeeper_network: Pubkey =
                Pubkey::from_str("uniqobk8oGh4XBLMqM68K8M2zNu3CdYX7q5go7whQiv").unwrap();

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

        msg!(
            "Registered user => user_pda={}, authority={}",
            user_pda.key(),
            user_key
        );
        Ok(())
    }

    /// Claim tokens repeatedly:
    /// - If exempt, skip gateway check,
    /// - Otherwise check gateway token,
    /// - Enforces a 60s cooldown,
    /// - Mints tokens to the user’s associated token account from the dapp mint.
    pub fn claim(ctx: Context<Claim>) -> Result<()> {
        let cfg = &ctx.accounts.dapp_config;
        let user_key = ctx.accounts.user.key();
        let user_pda = &mut ctx.accounts.user_pda;

        // 1) Gateway or Exempt check.
        if user_key != cfg.exempt_address {
            let gatekeeper_network = Pubkey::from_str("uniqobk8oGh4XBLMqM68K8M2zNu3CdYX7q5go7whQiv")
                .unwrap();
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

        // 2) Enforce a 60-second cooldown.
        let now = Clock::get()?.unix_timestamp;
        let elapsed = now.saturating_sub(user_pda.last_claimed_timestamp);
        require!(elapsed >= 60, ErrorCode::CooldownNotMet);

        // 3) Determine the mint amount (1.25 tokens per minute at 6 decimals ~ 20,833 μtokens per second).
        let tokens_per_second_micro = 20_833u64;
        let minted_amount = tokens_per_second_micro.saturating_mul(elapsed as u64);
        if minted_amount == 0 {
            msg!("No tokens to mint; skipping claim.");
            return Ok(());
        }

        // 4) Mint tokens to user's ATA using the mint authority PDA.
        let bump = cfg.mint_authority_bump;
        let seeds = &[b"mint_authority".as_ref(), &[bump]];
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

        // 5) Update last claimed timestamp.
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

    /// Sets a new exempt address. Only the *current* exempt address may do so
    /// (or the first caller if unset).
    pub fn set_exempt(ctx: Context<SetExempt>, new_exempt: Pubkey) -> Result<()> {
        let cfg = &mut ctx.accounts.dapp_config;
        let signer_key = ctx.accounts.current_exempt.key();

        if cfg.exempt_address != Pubkey::default() {
            require!(
                signer_key == cfg.exempt_address,
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

/// Global configuration storing the gatekeeper network, token mint, and mint authority bump.
#[account]
pub struct DappConfig {
    pub gatekeeper_network: Pubkey,
    pub token_mint: Pubkey,
    pub mint_authority_bump: u8,
    pub exempt_address: Pubkey,
}

impl DappConfig {
    pub const LEN: usize = 8 + 32 + 32 + 1 + 32;
}

/// A PDA that holds the bump for the mint authority (used for signing).
#[account]
pub struct MintAuthorityPda {
    pub bump: u8,
}

impl MintAuthorityPda {
    pub const LEN: usize = 8 + 1;
}

/// Per-user account tracking the last claim timestamp.
#[account]
pub struct UserPda {
    pub authority: Pubkey,
    pub last_claimed_timestamp: i64,
}

impl UserPda {
    pub const LEN: usize = 8 + 32 + 8;
}

// ---------------------------------------------------------------------
//  ACCOUNTS FOR INSTRUCTIONS
// ---------------------------------------------------------------------

#[derive(Accounts)]
pub struct InitializeDapp<'info> {
    #[account(
        init,
        seeds = [b"dapp_config"],
        bump,
        payer = initializer,
        space = DappConfig::LEN
    )]
    pub dapp_config: Account<'info, DappConfig>,

    #[account(mut)]
    pub initializer: Signer<'info>,

    /// The token mint to be used for distribution.
    /// This account must be pre-created (or created via a separate process).
    pub token_mint: InterfaceAccount<'info, Mint>,

    /// The mint authority PDA, derived with seeds [b"mint_authority"].
    #[account(
        init,
        seeds = [b"mint_authority"],
        bump,
        payer = initializer,
        space = MintAuthorityPda::LEN,
    )]
    pub mint_authority: Account<'info, MintAuthorityPda>,

    /// The initializer's associated token account for the token mint.
    #[account(
        init_if_needed,
        payer = initializer,
        associated_token::mint = token_mint,
        associated_token::authority = initializer
    )]
    pub user_ata: InterfaceAccount<'info, TokenAccount>,

    #[account(address = TOKEN_2022_PROGRAM_ID)]
    pub token_program: Program<'info, Token2022>,

    pub associated_token_program: Program<'info, AssociatedToken>,
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
    pub mint_authority: Account<'info, MintAuthorityPda>,

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
    #[msg("Signer is not the current exempt address.")]
    NotExemptAddress,
}
