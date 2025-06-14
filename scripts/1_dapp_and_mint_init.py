# scripts/bootstrap_cal_coin.py

import asyncio
import json
import secrets
from pathlib import Path

from anchorpy import Idl, Program, Provider, Wallet, Context
from solana.rpc.async_api import AsyncClient
from solders.keypair import Keypair
from solders.pubkey import Pubkey
from solders.system_program import ID as SYS_PROGRAM_ID
#from solders.sysvar import SysvarRent

# ─── Constants ────────────────────────────────────────────────────────────────

# Change this if you deploy to a different network.
RPC_URL       = "https://api.devnet.solana.com"
PROGRAM_ID    = Pubkey.from_string("AFrYiV7fCPEVCbCXktrmGW9YuNPboaPUmFWTca3UTqZp")
IDL_PATH = "../target/idl/cal_coin.json"

# SPL‐Token‐2022 Program ID on Devnet
SPL_TOKEN_2022_ID = Pubkey.from_string("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb")

# Rent sysvar
RENT_SYSVAR_ID    =  Pubkey.from_string("SysvarRent111111111111111111111111111111111")

# Where to write any on-chain addresses (PDAs or mints) for later.
OUTPUT_DIR   = Path("./keys")
OUTPUT_DIR.mkdir(exist_ok=True)

# ─── Helper Functions ─────────────────────────────────────────────────────────

async def program_client() -> Program:
    """
    Connect to Devnet via your local ~/.config/solana/id.json wallet,
    load the IDL, and return an AnchorPy Program client.
    """
    client   = AsyncClient(RPC_URL)
    provider = Provider(client, Wallet.local())
    idl      = Idl.from_json(Path(IDL_PATH).read_text())
    return Program(idl, PROGRAM_ID, provider)

def write_pubkey(path: Path, key: Pubkey):
    path.write_text(str(key))
    print(f"→ wrote {path}: {key}")

# ─── Main Bootstrap Script ───────────────────────────────────────────────────

async def main(decimals: int = 9):
    prog   = await program_client()
    wallet = prog.provider.wallet
    user   = wallet.public_key

    # Derive the same PDAs you use on‐chain:
    #   let (dapp_config, _)   = Pubkey::find_program_address(&[b"dapp_config"], &PROGRAM_ID);
    #   let (mint_authority, _) = Pubkey::find_program_address(&[b"mint_authority"], &PROGRAM_ID);
    dapp_pda, _   = Pubkey.find_program_address([b"dapp_config"], PROGRAM_ID)
    mint_auth, _  = Pubkey.find_program_address([b"mint_authority"], PROGRAM_ID)

    print("Derived PDAs:")
    print("  dapp_config PDA   =", dapp_pda)
    print("  mint_authority PDA=", mint_auth)

    #
    # ─── 1️⃣ Initialize DappConfig on‐Chain ────────────────────────────────────
    #
    print("\n1️⃣  Running initialize_dapp...")

    await prog.rpc["initialize_dapp"](
        ctx=Context(
            accounts={
                "dapp_config":    dapp_pda,
                "payer":          user,
                "system_program": SYS_PROGRAM_ID,
                "rent":           RENT_SYSVAR_ID,
            },
            signers=[wallet.payer],
        )
    )
    print("✓ initialize_dapp succeeded")

    #
    # ─── 2️⃣ Create & Initialize the Mint ───────────────────────────────────────
    #
    print("\n2️⃣  Creating new SPL‐Token‐2022 mint keypair locally...")
    mint_kp = Keypair()
    mint    = mint_kp.pubkey()
    print("  New mint pubkey:", mint)

    print("2️⃣  Running initialize_mint (decimals =", decimals, ")...")
    await prog.rpc["initialize_mint"](
        decimals,
        ctx=Context(
            accounts={
                "dapp_config":    dapp_pda,
                "mint_authority": mint_auth,
                "mint_for_dapp":  mint,
                "payer":          user,             # ← must be "payer"
                "token_program":  SPL_TOKEN_2022_ID,
                "system_program": SYS_PROGRAM_ID,
                "rent":           RENT_SYSVAR_ID,
            },
            signers=[wallet.payer, mint_kp],
        )
    )
    print("✓ initialize_mint succeeded")

    #
    # ─── 3️⃣ Write Out Addresses for Later ───────────────────────────────────────
    #
    write_pubkey(OUTPUT_DIR / "dapp_pda.txt",         dapp_pda)
    write_pubkey(OUTPUT_DIR / "mint_auth_pda.txt",     mint_auth)
    write_pubkey(OUTPUT_DIR / "mint_pubkey.txt",       mint)

    print("\n✅  Bootstrap complete.")
    print("   dapp_pda       =", dapp_pda)
    print("   mint_authority =", mint_auth)
    print("   mint_address   =", mint)

    await prog.provider.close()

if __name__ == "__main__":
    asyncio.run(main())
