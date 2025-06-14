# scripts/ezhard_validator_demo.py
import asyncio, json, hmac, hashlib, secrets, traceback
from pathlib import Path
from anchorpy import Program, Provider, Wallet, Context, Idl
from solana.rpc.async_api import AsyncClient
from solders.keypair import Keypair
from solders.pubkey import Pubkey
from solders.system_program import ID as SYS_PROGRAM_ID

# ─── program constants ────────────────────────────────────────────────────────
RPC_URL   = "https://api.devnet.solana.com"
PROGRAM_ID= Pubkey.from_string("9matfyqfsoKn9dgnkdf99pGk7dkL2EPuVte9SkQ9AyxV")
IDL_PATH  = "../target/idl/cal_coin.json"
KEYS      = Path("./keys")
KEYS.mkdir(exist_ok=True)

# ─── EzHard helper (from previous message) ────────────────────────────────────
class EzHard:
    def __init__(self):
        self.gkey = bytes.fromhex((KEYS / "global_key.txt").read_text().strip())

    def current_old_note(self):
        committed = sorted(
            fn for fn in KEYS.glob("oldhash[0-9]*.txt")
            if not (fn.stem.endswith("_pending") or fn.stem.endswith("_failed"))
        )
        n = int(committed[-1].stem.replace("oldhash", ""))
        return n, bytes.fromhex(committed[-1].read_text().strip())

    def prepare_new(self, tag=b"upd"):
        n, _ = self.current_old_note()
        new_note = secrets.token_bytes(64)
        new_hash = hmac.new(self.gkey, new_note + tag, hashlib.sha512).digest()
        (KEYS / f"oldhash{n+1}_pending.txt").write_text(new_note.hex())
        return n + 1, new_note, new_hash

    def commit_success(self, n: int):
        (KEYS / f"oldhash{n}_pending.txt").rename(KEYS / f"oldhash{n}.txt")

    def commit_fail(self, n: int):
        (KEYS / f"oldhash{n}_pending.txt").rename(KEYS / f"oldhash{n}_failed.txt")

# ─── utility ──────────────────────────────────────────────────────────────────
def load_kp(json_path: str) -> Keypair:
    data = json.load(open(json_path))
    return Keypair.from_bytes(bytes(data[:64]))

async def anchor_program() -> Program:
    client   = AsyncClient(RPC_URL)
    provider = Provider(client, Wallet.local())
    idl      = Idl.from_json(Path(IDL_PATH).read_text())
    return Program(idl, PROGRAM_ID, provider)

# ─── main logic ───────────────────────────────────────────────────────────────
async def main():
    prog   = await anchor_program()
    wallet = prog.provider.wallet
    user   = wallet.public_key

    dapp_pda, _   = Pubkey.find_program_address([b"dapp_config"], PROGRAM_ID)
    global_key, _ = Pubkey.find_program_address([b"global_key"],  PROGRAM_ID)

    ez = EzHard()
    noise1 = secrets.token_bytes(64)          # or bytes(64) for all-zero padding

    # 1️⃣ SUCCESS: add val1
    n1, new_note1, new_hash1 = ez.prepare_new(tag=b"val_add")
    _, old_note = ez.current_old_note()       # committed note
    val1_pk = load_kp(KEYS / "val1-keypair.json").pubkey()
    try:
        tx = await prog.rpc["set_validator_address"](
            old_note,
            new_hash1,
            new_hash1,                       # new_hash2 identical
            noise1,
            val1_pk,
            ctx=Context(
                accounts={
                    "dapp_config": dapp_pda,
                    "owner":       user,
                    "global_key":  global_key,
                },
                signers=[wallet.payer],
            ),
        )
        print("✅ SUCCESS — val1 added. Tx:", tx)
        ez.commit_success(n1)
    except Exception as e:
        ez.commit_fail(n1)
        print("Unexpected failure adding val1:", e)
        return

    noise2 = secrets.token_bytes(64)      # for the second, deliberate-fail call

    # 2️⃣ FAIL: attempt val2 with hash mismatch
    n2, new_note2, good_hash2 = ez.prepare_new(tag=b"val_add")
    bad_hash2 = bytearray(good_hash2); bad_hash2[0] ^= 0xFF; bad_hash2 = bytes(bad_hash2)
    val2_pk = load_kp(KEYS / "val2-keypair.json").pubkey()
    try:
        await prog.rpc["set_validator_address"](
            new_note1,          # now the latest committed note
            good_hash2,
            bad_hash2,          # mismatch triggers HashMismatch
            noise2,
            val2_pk,
            ctx=Context(
                accounts={
                    "dapp_config": dapp_pda,
                    "owner":       user,
                    "global_key":  global_key,
                },
                signers=[wallet.payer],
            ),
        )
    except Exception as fail:
        ez.commit_fail(n2)
        print("❌ EXPECTED FAILURE adding val2:", fail)

    await prog.provider.close()

if __name__ == "__main__":
    asyncio.run(main())
