# get_global_key.py  — reads the 64-byte secret from the PDA
import asyncio, pathlib
from solders.pubkey import Pubkey
from solana.rpc.async_api import AsyncClient

PROGRAM_ID = Pubkey.from_string("9matfyqfsoKn9dgnkdf99pGk7dkL2EPuVte9SkQ9AyxV")
RPC_URL    = "https://api.devnet.solana.com"
OUT_FILE   = pathlib.Path("keys/global_key.txt")

async def main():
    pda, _ = Pubkey.find_program_address([b"global_key"], PROGRAM_ID)
    print("global_key PDA:", pda)

    async with AsyncClient(RPC_URL) as c:
        acc = await c.get_account_info(pda)
        if acc.value is None:
            raise RuntimeError("PDA not found")
        data = acc.value.data                      # already raw bytes

    secret = data[8:72]                            # skip 8-byte discriminator
    print("secret (hex):", secret.hex())

    OUT_FILE.parent.mkdir(exist_ok=True)
    OUT_FILE.write_text(secret.hex() + "\n")
    print("→ wrote", OUT_FILE.resolve())

if __name__ == "__main__":
    asyncio.run(main())

