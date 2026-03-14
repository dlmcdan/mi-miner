use bitcoin::bip32::{DerivationPath, Xpriv};
use bitcoin::key::CompressedPublicKey;
use bitcoin::{Address, Network};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::str::FromStr;

use crate::MiMinerError;

/// BIP84 derivation path for native segwit (bc1q) addresses.
/// m/84'/0'/0'/0/0
const BIP84_PATH: &str = "m/84'/0'/0'/0/0";

/// Wallet data stored on disk.
#[derive(Serialize, Deserialize)]
struct WalletFile {
    /// BIP39 mnemonic (12 words)
    mnemonic: String,
    /// Derived Bitcoin address (for quick access without re-deriving)
    address: String,
    /// Network (mainnet/testnet)
    network: String,
    /// Warning shown to the user
    warning: String,
}

/// Result of wallet generation.
pub struct WalletInfo {
    pub mnemonic: String,
    pub address: String,
    pub path: PathBuf,
}

/// Generate a new wallet with a BIP39 mnemonic and derive a bc1q address.
/// Saves to ~/.mi-miner/wallet.json
pub fn generate_wallet() -> Result<WalletInfo, MiMinerError> {
    let wallet_path = wallet_path();

    if wallet_path.exists() {
        return Err(MiMinerError::Config(format!(
            "Wallet already exists at {}. Use --show-wallet to view it, or delete the file to generate a new one.",
            wallet_path.display()
        )));
    }

    // Generate 12-word BIP39 mnemonic (128 bits of entropy = 12 words)
    let mut entropy = [0u8; 16];
    getrandom::getrandom(&mut entropy)
        .map_err(|e| MiMinerError::Config(format!("Failed to generate random entropy: {e}")))?;
    let mnemonic = bip39::Mnemonic::from_entropy(&entropy)
        .map_err(|e| MiMinerError::Config(format!("Failed to generate mnemonic: {e}")))?;

    let mnemonic_str = mnemonic.to_string();

    // Derive address from mnemonic
    let address = derive_address_from_mnemonic(&mnemonic)?;

    // Save wallet file
    let wallet_file = WalletFile {
        mnemonic: mnemonic_str.clone(),
        address: address.clone(),
        network: "mainnet".to_string(),
        warning: "BACK UP YOUR 12 WORDS. If you lose them and find a block, the BTC is gone forever.".to_string(),
    };

    if let Some(parent) = wallet_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| MiMinerError::Config(format!("Failed to create wallet dir: {e}")))?;
    }

    let json = serde_json::to_string_pretty(&wallet_file)
        .map_err(|e| MiMinerError::Config(format!("Failed to serialize wallet: {e}")))?;

    std::fs::write(&wallet_path, &json)
        .map_err(|e| MiMinerError::Config(format!("Failed to write wallet: {e}")))?;

    // Set restrictive permissions on the wallet file (owner read/write only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&wallet_path, perms);
    }

    Ok(WalletInfo {
        mnemonic: mnemonic_str,
        address,
        path: wallet_path,
    })
}

/// Load an existing wallet and return the address.
pub fn load_wallet() -> Result<WalletInfo, MiMinerError> {
    let wallet_path = wallet_path();

    if !wallet_path.exists() {
        return Err(MiMinerError::Config(
            "No wallet found. Run `mi-miner --generate-wallet` to create one.".to_string(),
        ));
    }

    let json = std::fs::read_to_string(&wallet_path)
        .map_err(|e| MiMinerError::Config(format!("Failed to read wallet: {e}")))?;

    let wallet_file: WalletFile = serde_json::from_str(&json)
        .map_err(|e| MiMinerError::Config(format!("Failed to parse wallet: {e}")))?;

    Ok(WalletInfo {
        mnemonic: wallet_file.mnemonic,
        address: wallet_file.address,
        path: wallet_path,
    })
}

/// Get the wallet address if a wallet exists, otherwise None.
pub fn get_wallet_address() -> Option<String> {
    load_wallet().ok().map(|w| w.address)
}

/// Derive a bc1q (P2WPKH) address from a BIP39 mnemonic using BIP84 derivation.
fn derive_address_from_mnemonic(mnemonic: &bip39::Mnemonic) -> Result<String, MiMinerError> {
    let seed = mnemonic.to_seed("");

    let secp = bitcoin::secp256k1::Secp256k1::new();

    // Create master key from seed
    let master = Xpriv::new_master(Network::Bitcoin, &seed)
        .map_err(|e| MiMinerError::Config(format!("Failed to create master key: {e}")))?;

    // Derive BIP84 path: m/84'/0'/0'/0/0
    let path = DerivationPath::from_str(BIP84_PATH)
        .map_err(|e| MiMinerError::Config(format!("Invalid derivation path: {e}")))?;

    let child = master
        .derive_priv(&secp, &path)
        .map_err(|e| MiMinerError::Config(format!("Key derivation failed: {e}")))?;

    // Get the public key and create P2WPKH address
    let pubkey = child.to_priv().public_key(&secp);
    let compressed = CompressedPublicKey(pubkey.inner);

    let address = Address::p2wpkh(&compressed, Network::Bitcoin);

    Ok(address.to_string())
}

fn wallet_path() -> PathBuf {
    crate::config::dirs_path().join("wallet.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_address_from_known_mnemonic() {
        // Use a known mnemonic to verify derivation produces a valid bc1q address
        let mnemonic = bip39::Mnemonic::parse(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
        ).unwrap();

        let address = derive_address_from_mnemonic(&mnemonic).unwrap();

        // Should produce a valid bc1q... address
        assert!(address.starts_with("bc1q"), "Expected bc1q address, got: {address}");
        // The well-known "abandon" mnemonic BIP84 first address
        assert_eq!(address, "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu");
    }

    #[test]
    fn test_generate_different_mnemonics() {
        let mut e1 = [0u8; 16];
        let mut e2 = [0u8; 16];
        getrandom::getrandom(&mut e1).unwrap();
        getrandom::getrandom(&mut e2).unwrap();
        let m1 = bip39::Mnemonic::from_entropy(&e1).unwrap();
        let m2 = bip39::Mnemonic::from_entropy(&e2).unwrap();

        let a1 = derive_address_from_mnemonic(&m1).unwrap();
        let a2 = derive_address_from_mnemonic(&m2).unwrap();

        assert_ne!(a1, a2, "Two random mnemonics should produce different addresses");
        assert!(a1.starts_with("bc1q"));
        assert!(a2.starts_with("bc1q"));
    }
}
