// We're assuming that your Rollup bundle exposes the full API on window.solanaWeb3.
//const solanaWeb3 = window.solanaWeb3;

/**
 * Helper function to derive the Associated Token Account (ATA) address.
 */
async function findAssociatedTokenAddress(walletAddress, tokenMintAddress) {
  const associatedTokenProgramId = new solanaWeb3.PublicKey(
    'ATokenGPvbhRt7Z8BUGKh9dn1dPnse5xCCom1ULxq'
  );
  const tokenProgramId = new solanaWeb3.PublicKey(
    'TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb'
  );
  const [ata] = solanaWeb3.PublicKey.findProgramAddressSync(
    [
      walletAddress.toBytes(),
      tokenProgramId.toBytes(),
      tokenMintAddress.toBytes(),
    ],
    associatedTokenProgramId
  );
  return ata;
}

document.addEventListener('DOMContentLoaded', async () => {
  const connectWalletButton = document.getElementById('connectWallet');
  const setExemptButton = document.getElementById('setExempt');
  const registerUserButton = document.getElementById('registerUser');
  const claimTokensButton = document.getElementById('claimTokens');
  const walletAddressDiv = document.getElementById('walletAddress');
  const statusDiv = document.getElementById('status');

  let walletAdapter = null;
  let program = null;

  const idl = await fetch('./idl.json').then((res) => res.json());

  const programId = new solanaWeb3.PublicKey(
    'BYJtTQxe8F1Zi41bzWRStVPf57knpst3JqvZ7P5EMjex'
  );

  const [dappConfigPda] = solanaWeb3.PublicKey.findProgramAddressSync(
    [Uint8Array.from(new TextEncoder().encode('dapp_config'))],
    programId
  );

  connectWalletButton.onclick = async () => {
    if (window.solana && window.solana.isPhantom) {
      try {
        await window.solana.connect();
        walletAdapter = window.solana;
        walletAddressDiv.textContent = `Connected: ${walletAdapter.publicKey.toString()}`;
        statusDiv.textContent = 'Wallet connected successfully.';

        const connection = new solanaWeb3.Connection(
          solanaWeb3.clusterApiUrl('devnet'),
          'confirmed'
        );

        const provider = new window.anchor.AnchorProvider(
          connection,
          walletAdapter,
          window.anchor.AnchorProvider.defaultOptions
        );

        if (window.anchor && window.anchor.setProvider) {
          window.anchor.setProvider(provider);
        }

        program = new window.anchor.Program(idl, programId, provider);

        setExemptButton.disabled = false;
        registerUserButton.disabled = false;
        claimTokensButton.disabled = false;
      } catch (err) {
        statusDiv.textContent = `Error connecting wallet: ${err.message}`;
      }
    } else {
      statusDiv.textContent = 'Phantom wallet not found. Please install it.';
    }
  };

  setExemptButton.onclick = async () => {
    const exemptAddressInput = document.getElementById('exemptAddress').value.trim();
    if (!exemptAddressInput) {
      statusDiv.textContent = 'Please enter a public key.';
      return;
    }
    try {
      const newExempt = new solanaWeb3.PublicKey(exemptAddressInput);
      await program.methods
        .setExempt(newExempt)
        .accounts({
          dappConfig: dappConfigPda,
          currentExempt: walletAdapter.publicKey,
        })
        .rpc();
      statusDiv.textContent = `Exempt address set to: ${newExempt.toString()}`;
    } catch (err) {
      statusDiv.textContent = `Error setting exempt address: ${err.message}`;
    }
  };

  registerUserButton.onclick = async () => {
    try {
      const [userPda] = solanaWeb3.PublicKey.findProgramAddressSync(
        [Uint8Array.from(new TextEncoder().encode('user')), walletAdapter.publicKey.toBytes()],
        programId
      );
      await program.methods
        .registerUser()
        .accounts({
          dappConfig: dappConfigPda,
          user: walletAdapter.publicKey,
          gatewayToken: walletAdapter.publicKey,
          userPda,
          systemProgram: solanaWeb3.SystemProgram.programId,
          rent: solanaWeb3.SYSVAR_RENT_PUBKEY,
        })
        .rpc();
      statusDiv.textContent = 'User registered successfully.';
    } catch (err) {
      statusDiv.textContent = `Error registering user: ${err.message}`;
    }
  };

  claimTokensButton.onclick = async () => {
    try {
      const [userPda] = solanaWeb3.PublicKey.findProgramAddressSync(
        [Uint8Array.from(new TextEncoder().encode('user')), walletAdapter.publicKey.toBytes()],
        programId
      );

      const dappConfigAccount = await program.account.dappConfig.fetch(dappConfigPda);
      const tokenMint = new solanaWeb3.PublicKey(dappConfigAccount.token_mint);

      const [mintAuthorityPda] = solanaWeb3.PublicKey.findProgramAddressSync(
        [Uint8Array.from(new TextEncoder().encode('mint_authority'))],
        programId
      );

      const userAta = await findAssociatedTokenAddress(walletAdapter.publicKey, tokenMint);

      await program.methods
        .claim()
        .accounts({
          dappConfig: dappConfigPda,
          user: walletAdapter.publicKey,
          gatewayToken: walletAdapter.publicKey,
          userPda,
          tokenMint,
          mintAuthority: mintAuthorityPda,
          userAta,
          tokenProgram: new solanaWeb3.PublicKey(
            'TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb'
          ),
          associatedTokenProgram: new solanaWeb3.PublicKey(
            'ATokenGPvbhRt7Z8BUGKh9dn1dPnse5xCCom1ULxq'
          ),
          systemProgram: solanaWeb3.SystemProgram.programId,
          rent: solanaWeb3.SYSVAR_RENT_PUBKEY,
        })
        .rpc();
      statusDiv.textContent = 'Tokens claimed successfully.';
    } catch (err) {
      statusDiv.textContent = `Error claiming tokens: ${err.message}`;
    }
  };
});
