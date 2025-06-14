import asyncio
import json
import traceback
from pathlib import Path

# anchorpy
from anchorpy import Program, Provider, Wallet, Idl, Context
from anchorpy.program.namespace.instruction import AccountMeta

# solders / solana
from solders.keypair import Keypair
# from solders.pubkey import Pubkey
from solana.rpc.api import Pubkey
from solders.system_program import ID as SYS_PROGRAM_ID
from solana.rpc.async_api import AsyncClient
from solana.rpc.commitment import Confirmed
from solana.rpc.core import RPCException

ASSOCIATED_TOKEN_PROGRAM_ID = Pubkey.from_string("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL")
SPL_TOKEN_PROGRAM_ID = Pubkey.from_string("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb")
RENT_SYSVAR_ID = Pubkey.from_string("SysvarRent111111111111111111111111111111111")


def find_associated_token_address(owner: Pubkey, mint: Pubkey) -> Pubkey:
    """Derive the Associated Token Account (ATA) for a given owner and mint."""
    seeds = [bytes(owner), bytes(SPL_TOKEN_PROGRAM_ID), bytes(mint)]
    (ata, _) = Pubkey.find_program_address(seeds, ASSOCIATED_TOKEN_PROGRAM_ID)
    return ata

async def initialize_dapp_and_mint(
    program: Program,
    client: AsyncClient,
    description: str,
    socials: str,
    commission_percent: int,
    coin_issuance_rate: int,
    validator_claim_rate: int,
    curated_val: int,
    initial_commission_tokens: int,
    #gatekeeper_network: Pubkey,
    #player_limit: int,

):
    """
    Create a new Dapp + Mint in a single transaction using initialize_dapp_and_mint.
    Returns:
      (dapp_pda, mint_authority_pda, mint_for_dapp_pda,
       commission_percent, coin_issuance_rate,
       validator_claim_rate, curated_val, initial_commission_tokens)
    """
    print("Initializing Dapp + Mint...")

    user_pubkey = program.provider.wallet.public_key

    # (1) Derive new mint with seed [b"my_spl_mint", user]
    (mint_for_dapp_pda, mint_bump) = Pubkey.find_program_address(
        [b"my_spl_mint", bytes(user_pubkey)],
        program.program_id
    )

    # (2) Derive MintAuthority => [b"mint_authority"]
    (mint_authority_pda, ma_bump) = Pubkey.find_program_address(
        [b"mint_authority"],
        program.program_id
    )

    # (3) Derive Dapp => [b"dapp", mint_for_dapp_pda]
    (dapp_pda, dapp_bump) = Pubkey.find_program_address(
        [b"dapp", bytes(mint_for_dapp_pda)],
        program.program_id
    )

    # Check if Dapp already exists (to avoid re-initializing)
    acct_info = await client.get_account_info(dapp_pda, commitment=Confirmed)
    if acct_info.value is not None:
        print(f"Dapp PDA {dapp_pda} already initialized. Skipping.")
        return (
            dapp_pda,
            mint_authority_pda,
            mint_for_dapp_pda,
            commission_percent,
            coin_issuance_rate,
            validator_claim_rate,
            curated_val,
            initial_commission_tokens,
        )

    commission_ata = find_associated_token_address(user_pubkey, mint_for_dapp_pda)

    try:
        tx_sig = await program.rpc["initialize_dapp_and_mint"](
            description,
            socials,
            commission_percent,
            coin_issuance_rate,
            validator_claim_rate,
            curated_val,
            initial_commission_tokens,
            #gatekeeper_network,
            ctx=Context(
                accounts={
                    "dapp": dapp_pda,
                    "mint_authority": mint_authority_pda,
                    "mint_for_dapp": mint_for_dapp_pda,
                    "user": user_pubkey,
                    "commission_ata": commission_ata,
                    "token_program": SPL_TOKEN_PROGRAM_ID,
                    "associated_token_program": ASSOCIATED_TOKEN_PROGRAM_ID,
                    "system_program": SYS_PROGRAM_ID,
                    "rent": RENT_SYSVAR_ID,
                },
                signers=[program.provider.wallet.payer],
            )
        )
        print(f"Success! initialize_dapp_and_mint => Tx: {tx_sig}")

        tx_resp = await client.get_transaction(tx_sig, commitment=Confirmed)
        if tx_resp.value and tx_resp.value.transaction.meta:
            logs = tx_resp.value.transaction.meta.log_messages
            print("Transaction logs:")
            for line in logs:
                print(line)
    except RPCException as e:
        print(f"Error: {e}")
        traceback.print_exc()
        raise

    return (
        dapp_pda,
        mint_authority_pda,
        mint_for_dapp_pda,
        commission_percent,
        coin_issuance_rate,
        validator_claim_rate,
        curated_val,
        initial_commission_tokens,
    )


async def register_validator_curated(
    program: Program,
    client: AsyncClient,
    dapp_pda: Pubkey,
    mint_pubkey: Pubkey,
    owner_kp: Keypair,
    validator_pubkey: Pubkey,
):
    """
    Calls register_validator_curated(mint_pubkey, validator_to_add).
    leftover[0] => the wallet_pda for new_validator
    """
    print("\n[GATED] Registering new validator =>", validator_pubkey)

    # Derive the validator PDA
    (validator_pda, _) = Pubkey.find_program_address(
        [b"validator", bytes(mint_pubkey), bytes(validator_pubkey)],
        program.program_id
    )

    # leftover[0] => wallet_pda = [b"wallet_pda", mint, validator_pubkey]
    (wallet_pda, _) = Pubkey.find_program_address(
        [b"wallet_pda", bytes(mint_pubkey), bytes(validator_pubkey)],
        program.program_id
    )
    leftover_accounts = [
        AccountMeta(pubkey=wallet_pda, is_signer=False, is_writable=True),
    ]

    # The rest
    accounts_dict = {
        "dapp": dapp_pda,
        "owner": owner_kp.pubkey(),
        "validator_pda": validator_pda,
        "fancy_mint": mint_pubkey,
        "new_validator": validator_pubkey,  # must match on-chain field
        "validator_ata": find_associated_token_address(validator_pubkey, mint_pubkey),
        "token_program": SPL_TOKEN_PROGRAM_ID,
        "associated_token_program": ASSOCIATED_TOKEN_PROGRAM_ID,
        "system_program": SYS_PROGRAM_ID,
        # We'll omit "rent": RENT_SYSVAR_ID if Anchor doesn't require it in this Ix
    }

    try:
        tx_sig = await program.rpc["register_validator_curated"](
            mint_pubkey,
            validator_pubkey,
            ctx=Context(
                accounts=accounts_dict,
                signers=[owner_kp],
            ),
        )
        print("register_validator_curated => success, Tx:", tx_sig)
        tx_resp = await client.get_transaction(tx_sig, commitment=Confirmed)
        if tx_resp.value and tx_resp.value.transaction.meta:
            print("Transaction logs:")
            for line in tx_resp.value.transaction.meta.log_messages:
                print(line)
    except RPCException as e:
        print("Error in register_validator_gated:", e)
        traceback.print_exc()
        raise

    return validator_pda


async def register_validator_pda(
    program: Program,
    client: AsyncClient,
    dapp_pda: Pubkey,
    mint_pubkey: Pubkey,
    validator_kp: Keypair,
):
    """
    Calls open registration register_validator_pda(mint_pubkey).
    leftover[0] => wallet_pda for the validator
    """
    print("\nRegistering a new ValidatorPDA (OPEN REG)...")

    # Derive validator_pda
    (validator_pda, val_bump) = Pubkey.find_program_address(
        [b"validator", bytes(mint_pubkey), bytes(validator_kp.pubkey())],
        program.program_id
    )

    # leftover[0] => wallet_pda
    (wallet_pda, _) = Pubkey.find_program_address(
        [b"wallet_pda", bytes(mint_pubkey), bytes(validator_kp.pubkey())],
        program.program_id
    )
    leftover_accounts = [
        AccountMeta(pubkey=wallet_pda, is_signer=False, is_writable=True),
    ]

    validator_ata = find_associated_token_address(validator_kp.pubkey(), mint_pubkey)

    try:
        tx = await program.rpc["register_validator_pda"](
            mint_pubkey,
            ctx=Context(
                accounts={
                    "dapp": dapp_pda,
                    "fancy_mint": mint_pubkey,
                    "validator_pda": validator_pda,
                    "user": validator_kp.pubkey(),
                    "validator_ata": validator_ata,
                    "token_program": SPL_TOKEN_PROGRAM_ID,
                    "associated_token_program": ASSOCIATED_TOKEN_PROGRAM_ID,
                    "system_program": SYS_PROGRAM_ID,
                    "rent": RENT_SYSVAR_ID,
                },
                signers=[validator_kp],
                remaining_accounts=leftover_accounts,
            )
        )
        print(f"Validator PDA created => {validator_pda}. Tx: {tx}")

        tx_resp = await client.get_transaction(tx, commitment=Confirmed)
        if tx_resp.value and tx_resp.value.transaction.meta:
            logs = tx_resp.value.transaction.meta.log_messages
            print("Transaction logs:")
            for line in logs:
                print(line)
    except RPCException as e:
        print(f"Error registering validator: {e}")
        traceback.print_exc()
        raise

    return validator_pda


async def punch_in(
    program: Program,
    client: AsyncClient,
    dapp_pda: Pubkey,
    mint_pubkey: Pubkey,
    validator_kp: Keypair,
    validator_pda: Pubkey,
    gateway_token: Pubkey,
):
    """Calls punch_in(mint_pubkey)."""
    print("\nPunching in...")

    try:
        tx_sig = await program.rpc["punch_in"](
            mint_pubkey,
            ctx=Context(
                accounts={
                    "dapp": dapp_pda,
                    "validator_pda": validator_pda,
                    "validator": validator_kp.pubkey(),
                    "system_program": SYS_PROGRAM_ID,
                    "gateway_token": gateway_token,
                },
                signers=[validator_kp],
            )
        )
        print(f"Punched in => Tx: {tx_sig}")

        tx_resp = await client.get_transaction(tx_sig, commitment=Confirmed)
        if tx_resp.value and tx_resp.value.transaction.meta:
            logs = tx_resp.value.transaction.meta.log_messages
            print("Transaction logs:")
            for line in logs:
                print(line)
    except RPCException as e:
        print(f"Error punching in: {e}")
        traceback.print_exc()
        raise


async def create_player_ata(
    program: Program,
    client: AsyncClient,
    dapp_pda: Pubkey,
    mint_pubkey: Pubkey,
    user_kp: Keypair,
):
    user_pubkey = user_kp.pubkey()
    user_ata = find_associated_token_address(user_pubkey, mint_pubkey)

    # *** Also derive the wallet_pda ***
    (wallet_pda, _) = Pubkey.find_program_address(
        [b"wallet_pda", bytes(mint_pubkey), bytes(user_pubkey)],
        program.program_id
    )

    print(f"\nCreating ATA (if needed) for user={user_pubkey}, mint={mint_pubkey}")
    print(f" => user_ata: {user_ata}, wallet_pda: {wallet_pda}")

    try:
        tx_sig = await program.rpc["create_user_ata_if_needed"](
            mint_pubkey,
            ctx=Context(
                accounts={
                    "user": user_pubkey,
                    "fancy_mint": mint_pubkey,
                    "dapp": dapp_pda,
                    "user_ata": user_ata,
                    "wallet_pda": wallet_pda,  # <-- ADD THIS LINE
                    "token_program": SPL_TOKEN_PROGRAM_ID,
                    "associated_token_program": ASSOCIATED_TOKEN_PROGRAM_ID,
                    "system_program": SYS_PROGRAM_ID,
                    "rent": RENT_SYSVAR_ID,
                },
                signers=[user_kp],
            ),
        )
        print(f"create_user_ata_if_needed => success. Tx: {tx_sig}")
        # ...
    except RPCException as e:
        print(f"Error creating ATA: {e}")
        traceback.print_exc()
        raise
    return user_ata


async def register_player_pda(
    program: Program,
    client: AsyncClient,
    dapp_pda: Pubkey,
    mint_pubkey: Pubkey,
    name: str
):
    """
    Calls register_player_pda(name, mint_pubkey).
    leftover[0] => wallet_pda
    leftover[1] => user_ata
    """
    user_pubkey = program.provider.wallet.public_key

    # 1) Fetch Dapp to get the current player_count
    dapp_data = await program.account["Dapp"].fetch(dapp_pda)
    player_count = dapp_data.player_count
    print(f"Current dapp.player_count = {player_count}")

    # 2) Derive PDAs
    (player_pda, _) = Pubkey.find_program_address(
        [b"player_pda", bytes(dapp_pda), player_count.to_bytes(4, "little")],
        program.program_id
    )
    (player_name_pda, _) = Pubkey.find_program_address(
        [b"player_name", bytes(dapp_pda), name.encode("utf-8")],
        program.program_id
    )

    # leftover[0] => wallet_pda
    (wallet_pda, _) = Pubkey.find_program_address(
        [b"wallet_pda", bytes(mint_pubkey), bytes(user_pubkey)],
        program.program_id
    )
    # leftover[1] => user_ata
    user_ata = find_associated_token_address(user_pubkey, mint_pubkey)

    leftover_accounts = [
        AccountMeta(pubkey=wallet_pda, is_signer=False, is_writable=True),
        AccountMeta(pubkey=user_ata,  is_signer=False, is_writable=True),
    ]
    print(f"wallet_pda: {wallet_pda}, user_ata: {user_ata}")
    try:
        tx = await program.rpc["register_player_pda"](
            mint_pubkey,
            name,
            ctx=Context(
                accounts={
                    "dapp": dapp_pda,
                    "fancy_mint": mint_pubkey,
                    "player_pda": player_pda,
                    "player_name_pda": player_name_pda,
                    "user": user_pubkey,
                    "token_program": SPL_TOKEN_PROGRAM_ID,
                    "associated_token_program": ASSOCIATED_TOKEN_PROGRAM_ID,
                    "system_program": SYS_PROGRAM_ID,
                    "rent": RENT_SYSVAR_ID,
                },
                signers=[program.provider.wallet.payer],
                remaining_accounts=leftover_accounts,
            )
        )
        print(f"Registered player '{name}' => PlayerPDA={player_pda}. Tx: {tx}")
        tx_resp = await client.get_transaction(tx, commitment=Confirmed)
        if tx_resp.value and tx_resp.value.transaction.meta:
            logs = tx_resp.value.transaction.meta.log_messages
            print("Transaction logs:")
            for line in logs:
                print(line)
    except RPCException as e:
        print(f"Error registering player {name}: {e}")
        traceback.print_exc()
        raise

    return (player_pda, player_name_pda)


async def submit_minting_list(
    program: Program,
    client: AsyncClient,
    dapp_pda: Pubkey,
    mint_pubkey: Pubkey,
    validator_kp: Keypair,
    validator_pda: Pubkey,
    mint_authority_pda: Pubkey,
    player_pda: Pubkey,
    player_ata: Pubkey,
    commission_ata: Pubkey,
):
    """
    Calls submit_minting_list(...). The on-chain code expects leftover accounts:
        leftover[0] = commission_ata
        leftover[1] = PlayerPda
        leftover[2] = PlayerATA
    and so on for each player in player_ids.
    """
    print("\nSubmitting minting list...")

    # For demonstration, we’ll mint to one player with ID=[1]
    player_ids = [1]

    leftover_accounts = [
        AccountMeta(pubkey=commission_ata, is_signer=False, is_writable=True),
        AccountMeta(pubkey=player_pda, is_signer=False, is_writable=True),
        AccountMeta(pubkey=player_ata, is_signer=False, is_writable=True),
    ]

    try:
        tx_sig = await program.rpc["submit_minting_list"](
            mint_pubkey,
            player_ids,
            ctx=Context(
                accounts={
                    "dapp": dapp_pda,
                    "validator_pda": validator_pda,
                    "validator": validator_kp.pubkey(),
                    "fancy_mint": mint_pubkey,
                    "mint_authority": mint_authority_pda,
                    "token_program": SPL_TOKEN_PROGRAM_ID,
                    "associated_token_program": ASSOCIATED_TOKEN_PROGRAM_ID,
                    "system_program": SYS_PROGRAM_ID,
                },
                signers=[validator_kp],
                remaining_accounts=leftover_accounts,
            ),
        )
        print(f"submit_minting_list => success. Tx: {tx_sig}")
        tx_resp = await client.get_transaction(tx_sig, commitment=Confirmed)
        if tx_resp.value and tx_resp.value.transaction.meta:
            logs = tx_resp.value.transaction.meta.log_messages
            print("Transaction logs:")
            for line in logs:
                print(line)
    except RPCException as e:
        print(f"Error in submit_minting_list: {e}")
        traceback.print_exc()


async def main():
    client = AsyncClient("https://api.devnet.solana.com", commitment=Confirmed)
    wallet = Wallet.local()
    provider = Provider(client, wallet)

    # Load IDL from your local JSON
    idl_path = Path("../target/idl/fancoin.json")
    if not idl_path.exists():
        print(f"IDL file not found at {idl_path}")
        return

    with idl_path.open() as f:
        idl_json = f.read()
    idl = Idl.from_json(idl_json)

    # Program ID
    program_id = Pubkey.from_string("8gKV6NRFMcaVfxSCofsNQK4AHxm5rFXzjvBvPgKCqENjf")
    program = Program(idl, program_id, provider)

    try:
        # 1) Setup arguments
        description = "Grens Dapp"
        socials = "www.website.com"
        commission_percent = 0
        coin_issuance_rate = 2_833_333
        validator_claim_rate = 28_570
        curated_val = 1  # set to 1 => Gated
        player_limit = int(9000)
        # initial_commission_tokens = 21000000000000 * 2.83333
        initial_commission_tokens = int(210000000000 * 2.83333)
        gatekeeper_network = Pubkey.from_string("uniqobk8oGh4XBLMqM68K8M2zNu3CdYX7q5go7whQiv")
        # 2) Create the Dapp + Mint in one go
        (
            dapp_pda,
            mint_auth_pda,
            minted_mint_pda,
            commission_percent,
            coin_issuance_rate,
            validator_claim_rate,
            curated_val,
            initial_commission_tokens,
        ) = await initialize_dapp_and_mint(
            program,
            client,
            description,
            socials,
            commission_percent,
            coin_issuance_rate,
            validator_claim_rate,
            curated_val,
            initial_commission_tokens,
            #gatekeeper_network,
            #player_limit,
        )

        # Write PDAs to disk
        with open("dapp_pda.txt", "w") as f:
            f.write(str(dapp_pda))
        with open("mint_auth_pda.txt", "w") as f:
            f.write(str(mint_auth_pda))
        with open("minted_mint_pda.txt", "w") as f:
            f.write(str(minted_mint_pda))

        print("PDAs written to .txt files successfully!")

        # 3) If you want GATED registration:
        def load_keypair(json_path: str) -> Keypair:
            with open(json_path, "r") as f:
                data = json.load(f)
            return Keypair.from_bytes(bytes(data[:64]))

        owner_kp = load_keypair("id.json")  # the "owner" of the dapp
        curated_val_kp = load_keypair("val1-keypair.json")  # new validator

        # GATED approach
        validator_pda = await register_validator_curated(
            program,
            client,
            dapp_pda=dapp_pda,
            mint_pubkey=minted_mint_pda,
            owner_kp=owner_kp,
            validator_pubkey=curated_val_kp.pubkey(),
        )
        print("Curated validator =>", validator_pda)
        gateway_token=curated_val_kp.pubkey()
        # => Then you'd punch in with that new validator:
        await punch_in(
            program,
            client,
            dapp_pda=dapp_pda,
            mint_pubkey=minted_mint_pda,
            validator_kp=curated_val_kp,
            validator_pda=validator_pda,
            gateway_token=gateway_token,
        )

        # (Optional) If you want to do open registration (which is normally disallowed if curated_val=1):
        # validator_pda = await register_validator_pda(
        #     program,
        #     client,
        #     dapp_pda=dapp_pda,
        #     mint_pubkey=minted_mint_pda,
        #     validator_kp=curated_val_kp,
        # )

        # 5) Create a user ATA
        # user_kp = load_keypair("id.json")  # same as dapp owner for demonstration
        # user_ata = await create_player_ata(
        #     program,
        #     client,
        #     dapp_pda=dapp_pda,
        #     mint_pubkey=minted_mint_pda,
        #     user_kp=user_kp
        # )

        # # 6) Register a player
        # player_name = "Alice"
        # (alice_pda, alice_name_pda) = await register_player_pda(
        #     program,
        #     client,
        #     dapp_pda=dapp_pda,
        #     mint_pubkey=minted_mint_pda,
        #     name=player_name
        # )

        # _commission_ata = find_associated_token_address(owner_kp.pubkey(), minted_mint_pda)

        # # 7) Submit a minting list with leftover accounts for 1 player
        # await submit_minting_list(
        #     program,
        #     client,
        #     dapp_pda=dapp_pda,
        #     mint_pubkey=minted_mint_pda,
        #     validator_kp=curated_val_kp,
        #     validator_pda=validator_pda,
        #     mint_authority_pda=mint_auth_pda,
        #     player_pda=alice_pda,
        #     player_ata=user_ata,
        #     commission_ata=_commission_ata
        # )

        print("\nAll done.")
    except Exception as err:
        print("Unexpected error:", err)
        traceback.print_exc()
    finally:
        await client.close()


if __name__ == "__main__":
    asyncio.run(main())
