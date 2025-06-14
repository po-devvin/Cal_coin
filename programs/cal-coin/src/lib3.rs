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
use sha3::{Digest, Keccak256};
use std::str::FromStr;

declare_id!("G5USAdnRvUW4jf14QmxSJPEV5YNXzRahXjZNHEmZmqTM");

#[program]
pub mod cal_coin {
    use super::*;

    /// Initialize the dapp configuration.
    ///
    /// This instruction sets up the global config by storing:
    /// - The token mint (that will be used for distribution),
    /// - The gatekeeper network (hard-coded),
    /// - The bump for the mint authority PDA,
    /// - The owner.
    ///
    /// It also pre-mints tokens to the commission ATA if specified.
    /// Importantly, it ensures that the configuration is only initialized once.
    pub fn initialize_dapp_and_mint(
        ctx: Context<InitializeDappAndMint>,
        initial_commission_tokens: u64,
    ) -> Result<()> {
        let dapp = &mut ctx.accounts.dapp_config;
        
        // Block reinitialization.
        require!(!dapp.initialized, ErrorCode::AlreadyInitialized);

        // (A) Store the token mint address.
        dapp.token_mint = ctx.accounts.mint_for_dapp.key();
        // (B) Set the owner.
        dapp.owner = ctx.accounts.user.key();
        // (C) Set the gatekeeper network (hard-coded).
        dapp.gatekeeper_network = Pubkey::from_str("uniqobk8oGh4XBLMqM68K8M2zNu3CdYX7q5go7whQiv")
            .unwrap();
        // (D) Set exempt_address to default (all zeros).
        dapp.exempt_address = Pubkey::default();
        // (E) Store the bump for the mint authority.
        dapp.mint_authority_bump = ctx.bumps.mint_authority;
        
        // (F) Optionally pre-mint commission tokens.
        if initial_commission_tokens > 0 {
            let bump = ctx.bumps.mint_authority;
            let seeds = &[b"mint_authority".as_ref(), &[bump]];
            let signer_seeds = &[&seeds[..]];
            let cpi_ctx = CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                MintTo {
                    mint: ctx.accounts.mint_for_dapp.to_account_info(),
                    to: ctx.accounts.commission_ata.to_account_info(),
                    authority: ctx.accounts.mint_authority.to_account_info(),
                },
                signer_seeds,
            );
            token_2022::mint_to(cpi_ctx, initial_commission_tokens)?;
        }

        // Mark the configuration as initialized.
        dapp.initialized = true;
        
        msg!("Dapp initialized! Mint: {}", dapp.token_mint);
        Ok(())
    }

    /// One-time registration for a user.
    pub fn register_user(ctx: Context<RegisterUser>) -> Result<()> {
        let cfg = &ctx.accounts.dapp_config;
        let user_key = ctx.accounts.user.key();

        if user_key != cfg.exempt_address {
            let gatekeeper_network = Pubkey::from_str("uniqobk8oGh4XBLMqM68K8M2zNu3CdYX7q5go7whQiv").unwrap();
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

    /// Claim tokens repeatedly.
    pub fn claim(ctx: Context<Claim>) -> Result<()> {
        let cfg = &ctx.accounts.dapp_config;
        let user_key = ctx.accounts.user.key();
        let user_pda = &mut ctx.accounts.user_pda;

        if user_key != cfg.exempt_address {
            let gatekeeper_network = Pubkey::from_str("uniqobk8oGh4XBLMqM68K8M2zNu3CdYX7q5go7whQiv").unwrap();
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

        let now = Clock::get()?.unix_timestamp;
        let elapsed = now.saturating_sub(user_pda.last_claimed_timestamp);
        require!(elapsed >= 60, ErrorCode::CooldownNotMet);

        let tokens_per_second_micro = 20_833u64;
        let minted_amount = tokens_per_second_micro.saturating_mul(elapsed as u64);
        if minted_amount == 0 {
            msg!("No tokens to mint; skipping claim.");
            return Ok(());
        }

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

    /// Change the exempt address. The update can be performed by the current exempt or the owner.
    pub fn set_exempt(ctx: Context<SetExempt>, new_exempt: Pubkey) -> Result<()> {
        let cfg = &mut ctx.accounts.dapp_config;
        let signer_key = ctx.accounts.current_exempt.key();

        require!(
            cfg.exempt_address == Pubkey::default() ||
            signer_key == cfg.exempt_address ||
            signer_key == cfg.owner,
            ErrorCode::NotExemptAddress
        );

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
}

impl DappConfig {
    pub const LEN: usize = 8 + 32 + 32 + 1 + 32 + 1; // 8 discriminator + sum of fields
}

#[account]
pub struct MintAuthority {
    pub bump: u8,
}

impl MintAuthority {
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
#[instruction(initial_commission_tokens: u64)]
pub struct InitializeDappAndMint<'info> {
    #[account(
        init,
        payer = user,
        space = DappConfig::LEN,
        seeds = [b"dapp", mint_for_dapp.key().as_ref()],
        bump
    )]
    pub dapp_config: Account<'info, DappConfig>,
    
    #[account(
        init,
        payer = user,
        space = MintAuthority::LEN,
        seeds = [b"mint_authority"],
        bump
    )]
    pub mint_authority: Account<'info, MintAuthority>,
    
    #[account(
        init,
        payer = user,
        seeds = [b"my_spl_mint", user.key().as_ref()],
        bump,
        mint::decimals = 6,
        mint::authority = mint_authority,
        mint::freeze_authority = mint_authority
    )]
    pub mint_for_dapp: InterfaceAccount<'info, Mint>,
    
    #[account(mut)]
    pub user: Signer<'info>,
    
    #[account(
        init_if_needed,
        payer = user,
        associated_token::mint = mint_for_dapp,
        associated_token::authority = user
    )]
    pub commission_ata: InterfaceAccount<'info, TokenAccount>,
    
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
    #[msg("Dapp configuration already initialized.")]
    AlreadyInitialized,
    #[msg("Commission too high.")]
    CommissionTooLarge,
    #[msg("Issuance rate too high.")]
    IssuanceRateTooHigh,
    #[msg("Claim rate too high.")]
    ClaimRateTooHigh,
}
