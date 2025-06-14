#!/usr/bin/env python3
import hashlib

def compute_instruction_discriminator(instruction_name: str) -> bytes:
    """
    Compute the 8-byte discriminator for an instruction.
    Anchor uses the first 8 bytes of the SHA-256 hash of "global:<instruction_name>".
    """
    prefix = "global:"  # prefix for instructions
    data = f"{prefix}{instruction_name}".encode("utf-8")
    hash_bytes = hashlib.sha256(data).digest()
    return hash_bytes[:8]

def compute_account_discriminator(account_name: str) -> bytes:
    """
    Compute the 8-byte discriminator for an account.
    Anchor uses the first 8 bytes of the SHA-256 hash of "account:<account_name>".
    """
    prefix = "account:"  # prefix for accounts
    data = f"{prefix}{account_name}".encode("utf-8")
    hash_bytes = hashlib.sha256(data).digest()
    return hash_bytes[:8]

# Hard-coded functions for specific instructions

def discriminator_initialize_dapp() -> bytes:
    return compute_instruction_discriminator("initialize_dapp_and_mint")

def discriminator_set_exempt() -> bytes:
    return compute_instruction_discriminator("set_exempt")

def discriminator_register_user() -> bytes:
    return compute_instruction_discriminator("register_user")

def discriminator_claim() -> bytes:
    return compute_instruction_discriminator("claim")

# (Optional) Hard-coded functions for specific accounts

def discriminator_dapp_config() -> bytes:
    return compute_account_discriminator("DappConfig")

def discriminator_mint_authority_pda() -> bytes:
    return compute_account_discriminator("MintAuthorityPda")

def discriminator_user_pda() -> bytes:
    return compute_account_discriminator("UserPda")

if __name__ == '__main__':
    # Compute and print instruction discriminators
    init_disc = discriminator_initialize_dapp().hex()
    set_exempt_disc = discriminator_set_exempt().hex()
    register_user_disc = discriminator_register_user().hex()
    claim_disc = discriminator_claim().hex()
    
    print("Instruction Discriminators:")
    print(f"initialize_dapp_and_mint: {init_disc}")
    print(f"set_exempt:      {set_exempt_disc}")
    print(f"register_user:   {register_user_disc}")
    print(f"claim:           {claim_disc}")
    
    # Optionally, compute account discriminators.
    dapp_config_disc = discriminator_dapp_config().hex()
    mint_auth_disc = discriminator_mint_authority_pda().hex()
    user_pda_disc = discriminator_user_pda().hex()
    
    print("\nAccount Discriminators:")
    print(f"DappConfig:       {dapp_config_disc}")
    print(f"MintAuthorityPda: {mint_auth_disc}")
    print(f"UserPda:          {user_pda_disc}")
