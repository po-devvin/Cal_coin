use anchor_lang::prelude::*;
use anchor_spl::{
    token_2022::{self as token_2022, ID as TOKEN_2022_PROGRAM_ID},
    associated_token::AssociatedToken,
    token_interface::{Mint, Token2022, TokenAccount},
};
use solana_gateway::Gateway;
use anchor_spl::token_2022::MintTo;
use hmac_sha512::HMAC;
use std::str::FromStr;

/* ─── Time constants ───────────────────────────────────────────────────────────── */
const UTC_OFFSET: i64   = 7 * 60 * 60;            // −07:00 (America/Vancouver)
const SECS_PER_DAY: i64 = 24 * 60 * 60;

/* ─── Rewards constants ────────────────────────────────────────────────────────── */
const USERNAME_MAX_LEN: usize = 32;
const PERIOD_SECONDS:  i64    = 90 * 60;          // 90-min claim cap
const DAY_CAP_SECONDS: i64    = 14 * 60 * 60;     // 14-hour daily cap

declare_id!("9matfyqfsoKn9dgnkdf99pGk7dkL2EPuVte9SkQ9AyxV");

/* ─────────────────────────────────────────────────────────────────────────────── */

#[program]
pub mod cal_coin {
    use super::*;

    /*═════════════════════════════════ Initialization ═══════════════════════════*/

    pub fn initialize_dapp(ctx: Context<InitializeDapp>) -> Result<()> {
        let cfg = &mut ctx.accounts.dapp_config;
        require!(!cfg.initialized, ErrorCode::AlreadyInitialized);

        cfg.owner              = ctx.accounts.payer.key();
        cfg.validator_address  = cfg.owner;
        cfg.gatekeeper_network =
            Pubkey::from_str("cidNdd9GGhpgUJRTrto1A1ayN2PKAuaW7pg1rqj6bRD").unwrap();
        cfg.exempt_address      = Pubkey::default();
        cfg.exception_count     = 0;
        cfg.token_mint          = Pubkey::default();
        cfg.mint_authority_bump = 0;
        cfg.commission_address  = cfg.owner;
        cfg.commission_bps      = 2000;
        cfg.stored_hash         = [0u8; 64];          // first call will be free
        cfg.gateway_updates     = 0;
        cfg.initialized         = false;
        Ok(())
    }

    pub fn initialize_mint(ctx: Context<InitializeMint>, _decimals: u8) -> Result<()> {
        let cfg = &mut ctx.accounts.dapp_config;
        require!(!cfg.initialized, ErrorCode::AlreadyInitialized);

        cfg.token_mint          = ctx.accounts.mint_for_dapp.key();
        cfg.mint_authority_bump = ctx.bumps.mint_authority;
        cfg.initialized         = true;
        Ok(())
    }

    /*══════════════════════════════════ User flow ═══════════════════════════════*/

    pub fn register_user(ctx: Context<RegisterUser>, username: String) -> Result<()> {
        require!(username.len() <= USERNAME_MAX_LEN, ErrorCode::UsernameTooLong);
        let cfg      = &ctx.accounts.dapp_config;
        let user_key = ctx.accounts.user.key();

        if user_key != cfg.exempt_address {
            Gateway::verify_gateway_token_account_info(
                &ctx.accounts.gateway_token.to_account_info(),
                &user_key,
                &cfg.gatekeeper_network,
                None,
            )
            .map_err(|_| error!(ErrorCode::GatewayCheckFailed))?;
        }

        let profile = &mut ctx.accounts.user_profile;
        profile.authority         = user_key;
        profile.username          = username;
        profile.last_username_set = 0;
        profile.last_claimed      = 0;
        profile.last_paid         = 0;
        profile.claim_flag        = false;
        profile.is_patron         = false;
        profile.daily_accumulated = 0;
        profile.daily_reset       = 0;
        Ok(())
    }

    pub fn claim(ctx: Context<Claim>) -> Result<()> {
        let cfg      = &ctx.accounts.dapp_config;
        let user_key = ctx.accounts.user.key();
        let profile  = &mut ctx.accounts.user_profile;

        if user_key != cfg.exempt_address {
            Gateway::verify_gateway_token_account_info(
                &ctx.accounts.gateway_token.to_account_info(),
                &user_key,
                &cfg.gatekeeper_network,
                None,
            )
            .map_err(|_| error!(ErrorCode::GatewayCheckFailed))?;
        }

        let now = Clock::get()?.unix_timestamp;
        require!(now - profile.last_paid >= PERIOD_SECONDS, ErrorCode::CooldownNotMet);

        profile.last_claimed = now;
        profile.claim_flag   = true;
        Ok(())
    }

    /*═══════════════════════════════════ Payout ═════════════════════════════════*/

    pub fn payout<'info>(
        ctx:      Context<'_, '_, '_, 'info, Payout<'info>>,
        game_id:  String,
        game_ts:  String,
        old_note: [u8; 64],
        new_hash: [u8; 64],
        new_hash2:[u8; 64],
        noise:    [u8; 64],
    ) -> Result<()> {
        let cfg     = &mut ctx.accounts.dapp_config;
        let profile = &mut ctx.accounts.user_profile;

        require!(profile.claim_flag, ErrorCode::NoClaimPending);
        require!(ctx.accounts.validator.key() == cfg.validator_address, ErrorCode::InvalidValidator);
        require!(new_hash == new_hash2, ErrorCode::HashMismatch);

        /*── H-MAC guard ─────────────────────────────────────────────────────────*/
        let data = [
            &old_note[..],
            &new_hash[..],
            &noise[..],
            game_id.as_bytes(),
            game_ts.as_bytes(),
        ]
        .concat();
        let tag = HMAC::mac(&data, &ctx.accounts.global_key.key);
        require!(tag == cfg.stored_hash, ErrorCode::Unauthorized);
        cfg.stored_hash = new_hash;

        /*── Leftover account checks ─────────────────────────────────────────────*/
        let leftover = &ctx.remaining_accounts;
        require!(leftover.len() >= 2, ErrorCode::InsufficientLeftoverAccounts);
        let user_ata = &leftover[0];
        let comm_ata = &leftover[1];
        require!(
            user_ata.owner == &TOKEN_2022_PROGRAM_ID && comm_ata.owner == &TOKEN_2022_PROGRAM_ID,
            ErrorCode::InvalidAtaAccount
        );

        /*── Local-day cap math ─────────────────────────────────────────────────*/
        let now         = Clock::get()?.unix_timestamp;
        let local_start = ((now + UTC_OFFSET) / SECS_PER_DAY) * SECS_PER_DAY - UTC_OFFSET;
        if profile.daily_reset != local_start {
            profile.daily_reset       = local_start;
            profile.daily_accumulated = 0;
        }
        let elapsed   = now.saturating_sub(profile.last_claimed);
        let to_credit = elapsed.min(PERIOD_SECONDS) as u64;
        let remaining = DAY_CAP_SECONDS
            .saturating_sub(profile.daily_accumulated as i64)
            .max(0) as u64;
        let used      = to_credit.min(remaining);
        require!(used > 0, ErrorCode::DailyCapReached);

        let user_rate = if profile.is_patron { 1.0 } else { 0.75 };
        let comm_rate = if profile.is_patron { 0.25 } else { 0.50 };
        let user_amt  = (used as f64 / 60.0 * user_rate) as u64;
        let comm_amt  = (used as f64 / 60.0 * comm_rate) as u64;

        /*── Mint ───────────────────────────────────────────────────────────────*/
        let seeds  = &[b"mint_authority".as_ref(), &[cfg.mint_authority_bump]];
        let signer = &[&seeds[..]];

        token_2022::mint_to(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                MintTo {
                    mint:      ctx.accounts.token_mint.to_account_info(),
                    to:        user_ata.to_account_info(),
                    authority: ctx.accounts.mint_authority.to_account_info(),
                },
                signer,
            ),
            user_amt,
        )?;
        token_2022::mint_to(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                MintTo {
                    mint:      ctx.accounts.token_mint.to_account_info(),
                    to:        comm_ata.to_account_info(),
                    authority: ctx.accounts.mint_authority.to_account_info(),
                },
                signer,
            ),
            comm_amt,
        )?;

        emit!(PayoutEvent {
            username: profile.username.clone(),
            game_id,
            game_ts,
            tokens: user_amt,
        });
        profile.claim_flag        = false;
        profile.last_paid         = now;
        profile.daily_accumulated = profile.daily_accumulated.saturating_add(used);
        Ok(())
    }

    /*═════════════════════════════ Global key bootstrap ═════════════════════════*/

    pub fn init_global_key(
        ctx: Context<InitGlobalKey>,
        secret: [u8; 64],
    ) -> Result<()> {
        let g = &mut ctx.accounts.global_key;
        require!(g.key == [0u8; 64], ErrorCode::AlreadyInitialized);
        g.key = secret;
        Ok(())
    }

    /*════════════════════════════════ Admin setters ═════════════════════════════*/

    pub fn set_validator_address(
        ctx: Context<SetValidator>,
        old_note: [u8; 64],
        new_hash: [u8; 64],
        new_hash2:[u8; 64],
        noise:    [u8; 64],
        new_validator: Pubkey,
    ) -> Result<()> {
        let state = &mut ctx.accounts.dapp_config;
        require!(ctx.accounts.owner.key() == state.owner, ErrorCode::Unauthorized);
        require!(new_hash == new_hash2, ErrorCode::HashMismatch);

        let data = [
            &old_note[..],
            &new_hash[..],
            &noise[..],
            &new_validator.to_bytes()[..],
        ]
        .concat();
        let tag = HMAC::mac(&data, &ctx.accounts.global_key.key);

        /* first-call-is-free */
        let first_time = state.stored_hash == [0u8; 64];
        if !first_time {
            require!(tag == state.stored_hash, ErrorCode::Unauthorized);
        }

        state.validator_address = new_validator;
        state.stored_hash       = new_hash;
        Ok(())
    }

    pub fn set_exception_address(
        ctx: Context<SetException>,
        old_note: [u8; 64],
        new_hash: [u8; 64],
        new_hash2:[u8; 64],
        noise:    [u8; 64],
        new_exempt: Pubkey,
    ) -> Result<()> {
        let state = &mut ctx.accounts.dapp_config;
        require!(new_hash == new_hash2, ErrorCode::HashMismatch);

        let data = [
            &old_note[..],
            &new_hash[..],
            &noise[..],
            &new_exempt.to_bytes()[..],
        ]
        .concat();
        let tag = HMAC::mac(&data, &ctx.accounts.global_key.key);

        let first_time = state.stored_hash == [0u8; 64];
        if !first_time {
            require!(tag == state.stored_hash, ErrorCode::Unauthorized);
        }

        state.exempt_address  = new_exempt;
        state.exception_count += 1;
        state.stored_hash      = new_hash;
        Ok(())
    }

    pub fn update_commission_bps(
        ctx: Context<SetCommissionBps>,
        old_note: [u8; 64],
        new_hash: [u8; 64],
        new_hash2:[u8; 64],
        noise:    [u8; 64],
        new_bps:  u16,
    ) -> Result<()> {
        let state = &mut ctx.accounts.dapp_config;
        require!(new_hash == new_hash2, ErrorCode::HashMismatch);

        let data = [
            &old_note[..],
            &new_hash[..],
            &noise[..],
            &new_bps.to_le_bytes()[..],
        ]
        .concat();
        let tag = HMAC::mac(&data, &ctx.accounts.global_key.key);

        let first_time = state.stored_hash == [0u8; 64];
        if !first_time {
            require!(tag == state.stored_hash, ErrorCode::Unauthorized);
        }
        require!(new_bps <= 2000, ErrorCode::InvalidCommissionBps);

        state.commission_bps = new_bps;
        state.stored_hash    = new_hash;
        Ok(())
    }

    pub fn update_gateway_network(
        ctx: Context<SetGatewayNetwork>,
        old_note: [u8; 64],
        new_hash: [u8; 64],
        new_hash2:[u8; 64],
        noise:    [u8; 64],
        new_net:  Pubkey,
    ) -> Result<()> {
        let state = &mut ctx.accounts.dapp_config;
        require!(new_hash == new_hash2, ErrorCode::HashMismatch);

        let data = [
            &old_note[..],
            &new_hash[..],
            &noise[..],
            &new_net.to_bytes()[..],
        ]
        .concat();
        let tag = HMAC::mac(&data, &ctx.accounts.global_key.key);

        let first_time = state.stored_hash == [0u8; 64];
        if !first_time {
            require!(tag == state.stored_hash, ErrorCode::Unauthorized);
        }

        state.gatekeeper_network = new_net;
        state.gateway_updates   += 1;
        state.stored_hash        = new_hash;
        Ok(())
    }
}

/*════════════════════════════════ Data Accounts ═════════════════════════════════*/

#[account]
pub struct DappConfig {
    pub owner:              Pubkey,
    pub validator_address:  Pubkey,
    pub gatekeeper_network: Pubkey,
    pub exempt_address:     Pubkey,
    pub exception_count:    u64,
    pub token_mint:         Pubkey,
    pub mint_authority_bump:u8,
    pub commission_address: Pubkey,
    pub commission_bps:     u16,
    pub stored_hash:        [u8; 64],
    pub gateway_updates:    u64,
    pub initialized:        bool,
}
impl DappConfig {
    pub const LEN: usize = 8
        + 32 * 6   // six Pubkeys
        + 8        // exception_count
        + 1        // bump
        + 2        // bps
        + 64       // stored_hash
        + 8        // gateway_updates
        + 1;       // initialized
}

#[account]
pub struct MintAuthority { pub bump: u8 }
impl MintAuthority { pub const LEN: usize = 8 + 1; }

#[account]
pub struct UserProfile {
    pub authority:         Pubkey,
    pub username:          String,
    pub last_username_set: i64,
    pub last_claimed:      i64,
    pub last_paid:         i64,
    pub claim_flag:        bool,
    pub is_patron:         bool,
    pub daily_accumulated: u64,
    pub daily_reset:       i64,
}
impl UserProfile {
    pub const LEN: usize = 8 + 32 + 4 + USERNAME_MAX_LEN
        + 8 * 3 + 1 + 1
        + 8 + 8;
}


#[derive(Accounts)]
pub struct InitializeDapp<'info> {
    #[account(
        init,
        payer  = payer,
        space  = DappConfig::LEN,
        seeds  = [b"dapp_config"],
        bump
    )]
    pub dapp_config: Account<'info, DappConfig>,
    #[account(mut)] pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
    pub rent:           Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct InitializeMint<'info> {
    #[account(
        mut,
        seeds = [b"dapp_config"],
        bump,
        has_one = owner
    )]
    pub dapp_config:    Account<'info, DappConfig>,
    #[account(mut)] pub owner: Signer<'info>,
    #[account(
        init,
        payer = owner,
        space = MintAuthority::LEN,
        seeds = [b"mint_authority"],
        bump
    )]
    pub mint_authority: Account<'info, MintAuthority>,
    #[account(
        init,
        payer   = owner,
        mint::authority         = mint_authority,
        mint::freeze_authority  = mint_authority,
        mint::decimals          = 9
    )]
    pub mint_for_dapp:  InterfaceAccount<'info, Mint>,
    #[account(address = TOKEN_2022_PROGRAM_ID)]
    pub token_program:  Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
    pub rent:           Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct RegisterUser<'info> {
    #[account(seeds = [b"dapp_config"], bump)]
    pub dapp_config: Account<'info, DappConfig>,
    #[account(mut)] pub user: Signer<'info>,
    pub gateway_token: UncheckedAccount<'info>,
    #[account(
        init,
        payer  = user,
        space  = UserProfile::LEN,
        seeds  = [b"user_pda", user.key().as_ref()],
        bump
    )]
    pub user_profile: Account<'info, UserProfile>,
    pub system_program: Program<'info, System>,
    pub rent:           Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct Claim<'info> {
    #[account(seeds = [b"dapp_config"], bump)]
    pub dapp_config: Account<'info, DappConfig>,
    #[account(mut)] pub user: Signer<'info>,
    pub gateway_token: UncheckedAccount<'info>,
    #[account(
        mut,
        seeds = [b"user_pda", user.key().as_ref()],
        bump
    )]
    pub user_profile: Account<'info, UserProfile>,
}

#[derive(Accounts)]
pub struct Payout<'info> {
    #[account(seeds = [b"dapp_config"], bump)]
    pub dapp_config: Account<'info, DappConfig>,
    #[account(mut)] pub validator: Signer<'info>,
    pub gateway_token: UncheckedAccount<'info>,
    #[account(
        mut,
        seeds = [b"user_pda", user.key().as_ref()],
        bump
    )]
    pub user_profile: Account<'info, UserProfile>,
    #[account(mut)] pub user: Signer<'info>,
    #[account(
        mut,
        constraint = token_mint.key() == dapp_config.token_mint
    )]
    pub token_mint: InterfaceAccount<'info, Mint>,
    #[account(
        seeds = [b"mint_authority"],
        bump  = dapp_config.mint_authority_bump
    )]
    pub mint_authority: Account<'info, MintAuthority>,
    #[account(address = TOKEN_2022_PROGRAM_ID)]
    pub token_program: Program<'info, Token2022>,
    pub global_key:    UncheckedAccount<'info>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    // ATAs supplied via ctx.remaining_accounts
}

#[derive(Accounts)]
pub struct SetValidator<'info> {
    #[account(mut)] pub dapp_config: Account<'info, DappConfig>,
    #[account(mut)] pub owner: Signer<'info>,
    pub global_key: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct SetException<'info> {
    #[account(mut)] pub dapp_config: Account<'info, DappConfig>,
    pub global_key: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct SetCommissionBps<'info> {
    #[account(mut)] pub dapp_config: Account<'info, DappConfig>,
    pub global_key: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct SetGatewayNetwork<'info> {
    #[account(mut)] pub dapp_config: Account<'info, DappConfig>,
    pub global_key: UncheckedAccount<'info>,
}

/*═══════════════════════════════ Global H-MAC key account ═══════════════════════════*/

pub const GLOBAL_KEY_SPACE: usize = 8 + 64;
#[account]
pub struct GlobalKey {
    pub key: [u8; 64],
}

#[derive(Accounts)]
pub struct InitGlobalKey<'info> {
    #[account(
        init,
        payer  = payer,
        space  = GLOBAL_KEY_SPACE,
        seeds  = [b"global_key"],
        bump
    )]
    pub global_key: Account<'info, GlobalKey>,
    #[account(mut)] pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
    pub rent:           Sysvar<'info, Rent>,
}

/*════════════════════════════════ Event & Errors ════════════════════════════════════*/

#[event]
pub struct PayoutEvent {
    pub username: String,
    pub game_id:  String,
    pub game_ts:  String,
    pub tokens:   u64,
}

#[error_code]
pub enum ErrorCode {
    AlreadyInitialized,
    Unauthorized,
    InvalidCommissionBps,
    CooldownNotMet,
    NoClaimPending,
    UsernameTooLong,
    GatewayCheckFailed,
    InsufficientLeftoverAccounts,
    InvalidAtaAccount,
    HashMismatch,
    DailyCapReached,
    InvalidValidator,
}
