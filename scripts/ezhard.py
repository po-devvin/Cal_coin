import hmac, hashlib, secrets
from pathlib import Path
from typing import Tuple

KEYS = Path("./keys")
KEYS.mkdir(exist_ok=True)

class EzHard:
    def __init__(self):
        self.gkey = bytes.fromhex((KEYS / "global_key.txt").read_text().strip())

    # ---------- public helpers --------------------------------------------------
    def current_old_note(self) -> Tuple[int, bytes]:
        """Return (N, note_bytes) for the highest *committed* oldhashN.txt."""
        committed = sorted(KEYS.glob("oldhash[0-9]*.txt"))
        if not committed:
            raise FileNotFoundError("no oldhashN.txt found")
        n = int(committed[-1].stem.replace("oldhash", ""))
        return n, bytes.fromhex(committed[-1].read_text().strip())

    def prepare_new(self, tag: bytes = b"upd") -> Tuple[int, bytes, bytes]:
        """
        Generates (next_n, new_note, new_hash) but **doesn't write files yet**.
        Call commit_success/commit_fail afterwards.
        """
        n, _ = self.current_old_note()
        new_note = secrets.token_bytes(64)
        new_hash = hmac.new(self.gkey, new_note + tag, hashlib.sha512).digest()
        (KEYS / f"oldhash{n+1}_pending.txt").write_text(new_note.hex())
        return n + 1, new_note, new_hash

    def commit_success(self, n: int):
        p = KEYS / f"oldhash{n}_pending.txt"
        p.rename(KEYS / f"oldhash{n}.txt")         # becomes official

    def commit_fail(self, n: int):
        p = KEYS / f"oldhash{n}_pending.txt"
        p.rename(KEYS / f"oldhash{n}_failed.txt")  # keep for forensics
