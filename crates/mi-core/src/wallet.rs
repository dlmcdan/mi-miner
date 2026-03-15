use bitcoin::bip32::{DerivationPath, Xpriv};
use bitcoin::key::CompressedPublicKey;
use bitcoin::{Address, Network};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::str::FromStr;

use crate::MiMinerError;

/// BIP84 derivation path for native segwit (bc1q) addresses.
const BIP84_PATH: &str = "m/84'/0'/0'/0/0";

const PBKDF2_ITERATIONS: u32 = 600_000;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;

// ── Wallet file ──

/// Wallet data stored on disk. Mnemonic is encrypted with a user passphrase.
#[derive(Serialize, Deserialize)]
struct WalletFile {
    address: String,
    network: String,
    /// AES-256-GCM encrypted mnemonic (hex-encoded: salt || nonce || ciphertext+tag).
    /// None for external-address-only wallets.
    #[serde(skip_serializing_if = "Option::is_none")]
    encrypted_mnemonic: Option<String>,
}

/// Legacy wallet file format — plaintext mnemonic, used for migration.
#[derive(Deserialize)]
struct LegacyWalletFile {
    #[serde(default)]
    mnemonic: String,
    address: String,
    #[allow(dead_code)]
    #[serde(default)]
    network: String,
}

/// Result of wallet generation.
pub struct WalletInfo {
    pub mnemonic: String,
    pub address: String,
    pub path: PathBuf,
}

// ── Encryption ──

fn encrypt_mnemonic(mnemonic: &str, passphrase: &str) -> Result<String, MiMinerError> {
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};

    let mut salt = [0u8; SALT_LEN];
    getrandom::getrandom(&mut salt)
        .map_err(|e| MiMinerError::Config(format!("RNG error: {e}")))?;

    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce_bytes)
        .map_err(|e| MiMinerError::Config(format!("RNG error: {e}")))?;

    // Derive 256-bit key from passphrase via PBKDF2-HMAC-SHA256
    let mut key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(
        passphrase.as_bytes(),
        &salt,
        PBKDF2_ITERATIONS,
        &mut key,
    );

    let cipher = Aes256Gcm::new((&key).into());
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, mnemonic.as_bytes())
        .map_err(|e| MiMinerError::Config(format!("Encryption error: {e}")))?;

    // Concatenate: salt[16] || nonce[12] || ciphertext+tag[...]
    let mut blob = Vec::with_capacity(SALT_LEN + NONCE_LEN + ciphertext.len());
    blob.extend_from_slice(&salt);
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ciphertext);

    Ok(hex::encode(blob))
}

fn decrypt_mnemonic(encrypted_hex: &str, passphrase: &str) -> Result<String, MiMinerError> {
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};

    let blob = hex::decode(encrypted_hex)
        .map_err(|e| MiMinerError::Config(format!("Invalid encrypted data: {e}")))?;

    if blob.len() < SALT_LEN + NONCE_LEN + 16 {
        return Err(MiMinerError::Config("Encrypted data too short".to_string()));
    }

    let salt = &blob[..SALT_LEN];
    let nonce_bytes = &blob[SALT_LEN..SALT_LEN + NONCE_LEN];
    let ciphertext = &blob[SALT_LEN + NONCE_LEN..];

    let mut key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(
        passphrase.as_bytes(),
        salt,
        PBKDF2_ITERATIONS,
        &mut key,
    );

    let cipher = Aes256Gcm::new((&key).into());
    let nonce = Nonce::from_slice(nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| MiMinerError::Config("Incorrect passphrase".to_string()))?;

    String::from_utf8(plaintext)
        .map_err(|e| MiMinerError::Config(format!("Invalid mnemonic data: {e}")))
}

// ── File helpers ──

fn save_wallet_file(
    address: &str,
    encrypted_mnemonic: Option<String>,
) -> Result<PathBuf, MiMinerError> {
    let path = wallet_path();
    let wallet_file = WalletFile {
        address: address.to_string(),
        network: "mainnet".to_string(),
        encrypted_mnemonic,
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| MiMinerError::Config(format!("Failed to create wallet dir: {e}")))?;
    }

    let json = serde_json::to_string_pretty(&wallet_file)
        .map_err(|e| MiMinerError::Config(format!("Failed to serialize wallet: {e}")))?;
    std::fs::write(&path, &json)
        .map_err(|e| MiMinerError::Config(format!("Failed to write wallet: {e}")))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&path, perms);
    }

    Ok(path)
}

fn wallet_path() -> PathBuf {
    crate::config::dirs_path().join("wallet.json")
}

// ── Public API ──

/// Generate a new wallet. Mnemonic is encrypted with the passphrase and stored on disk.
pub fn generate_wallet(passphrase: &str) -> Result<WalletInfo, MiMinerError> {
    if passphrase.is_empty() {
        return Err(MiMinerError::Config(
            "Passphrase is required to protect the recovery phrase.".to_string(),
        ));
    }

    let mut entropy = [0u8; 16];
    getrandom::getrandom(&mut entropy)
        .map_err(|e| MiMinerError::Config(format!("Failed to generate random entropy: {e}")))?;
    let mnemonic = bip39::Mnemonic::from_entropy(&entropy)
        .map_err(|e| MiMinerError::Config(format!("Failed to generate mnemonic: {e}")))?;

    let mnemonic_str = mnemonic.to_string();
    let address = derive_address_from_mnemonic(&mnemonic)?;

    let encrypted = encrypt_mnemonic(&mnemonic_str, passphrase)?;
    let path = save_wallet_file(&address, Some(encrypted))?;

    Ok(WalletInfo {
        mnemonic: mnemonic_str,
        address,
        path,
    })
}

/// Load wallet address from disk. Does not require a passphrase.
pub fn load_wallet() -> Result<WalletInfo, MiMinerError> {
    let wallet_path = wallet_path();

    if !wallet_path.exists() {
        return Err(MiMinerError::Config(
            "No wallet found. Run `mi-miner --generate-wallet` to create one.".to_string(),
        ));
    }

    let json = std::fs::read_to_string(&wallet_path)
        .map_err(|e| MiMinerError::Config(format!("Failed to read wallet: {e}")))?;

    // Try legacy format (plaintext mnemonic) first
    if let Ok(legacy) = serde_json::from_str::<LegacyWalletFile>(&json) {
        if !legacy.mnemonic.is_empty() {
            tracing::warn!(
                "Wallet contains unencrypted mnemonic. \
                 Re-generate or restore from Settings to encrypt it."
            );
            return Ok(WalletInfo {
                mnemonic: String::new(), // don't expose plaintext casually
                address: legacy.address,
                path: wallet_path,
            });
        }
    }

    let wallet_file: WalletFile = serde_json::from_str(&json)
        .map_err(|e| MiMinerError::Config(format!("Failed to parse wallet: {e}")))?;

    Ok(WalletInfo {
        mnemonic: String::new(),
        address: wallet_file.address,
        path: wallet_path,
    })
}

/// Get the wallet address if a wallet exists, otherwise None.
pub fn get_wallet_address() -> Option<String> {
    load_wallet().ok().map(|w| w.address)
}

/// Decrypt and return the mnemonic using the passphrase.
pub fn get_mnemonic(passphrase: &str) -> Result<String, MiMinerError> {
    let wallet_path = wallet_path();
    if !wallet_path.exists() {
        return Err(MiMinerError::Config("No wallet found.".to_string()));
    }

    let json = std::fs::read_to_string(&wallet_path)
        .map_err(|e| MiMinerError::Config(format!("Failed to read wallet: {e}")))?;

    // Check for legacy plaintext format
    if let Ok(legacy) = serde_json::from_str::<LegacyWalletFile>(&json) {
        if !legacy.mnemonic.is_empty() {
            // Legacy plaintext — return it but warn
            return Ok(legacy.mnemonic);
        }
    }

    let wallet_file: WalletFile = serde_json::from_str(&json)
        .map_err(|e| MiMinerError::Config(format!("Failed to parse wallet: {e}")))?;

    match wallet_file.encrypted_mnemonic {
        Some(encrypted) => decrypt_mnemonic(&encrypted, passphrase),
        None => Err(MiMinerError::Config(
            "No recovery phrase stored. This is an external-address wallet.".to_string(),
        )),
    }
}

/// Check if the wallet has an encrypted mnemonic (vs external address or legacy plaintext).
pub fn has_encrypted_mnemonic() -> bool {
    let wallet_path = wallet_path();
    if !wallet_path.exists() {
        return false;
    }
    let json = match std::fs::read_to_string(&wallet_path) {
        Ok(j) => j,
        Err(_) => return false,
    };
    if let Ok(w) = serde_json::from_str::<WalletFile>(&json) {
        return w.encrypted_mnemonic.is_some();
    }
    // Legacy plaintext format — has mnemonic but not encrypted
    if let Ok(l) = serde_json::from_str::<LegacyWalletFile>(&json) {
        return !l.mnemonic.is_empty();
    }
    false
}

/// Restore a wallet from a BIP39 mnemonic, encrypting it with the passphrase.
pub fn restore_wallet(mnemonic_str: &str, passphrase: &str) -> Result<WalletInfo, MiMinerError> {
    if passphrase.is_empty() {
        return Err(MiMinerError::Config(
            "Passphrase is required to protect the recovery phrase.".to_string(),
        ));
    }

    let mnemonic = bip39::Mnemonic::parse(mnemonic_str)
        .map_err(|e| MiMinerError::Config(format!("Invalid recovery phrase: {e}")))?;

    let mnemonic_str = mnemonic.to_string();
    let address = derive_address_from_mnemonic(&mnemonic)?;

    let encrypted = encrypt_mnemonic(&mnemonic_str, passphrase)?;
    let path = save_wallet_file(&address, Some(encrypted))?;

    Ok(WalletInfo {
        mnemonic: mnemonic_str,
        address,
        path,
    })
}

/// Delete the existing wallet file.
pub fn delete_wallet() -> Result<(), MiMinerError> {
    let wallet_path = wallet_path();
    if wallet_path.exists() {
        std::fs::remove_file(&wallet_path)
            .map_err(|e| MiMinerError::Config(format!("Failed to delete wallet: {e}")))?;
    }
    Ok(())
}

/// Derive a bc1q (P2WPKH) address from a BIP39 mnemonic using BIP84 derivation.
fn derive_address_from_mnemonic(mnemonic: &bip39::Mnemonic) -> Result<String, MiMinerError> {
    let seed = mnemonic.to_seed("");
    let secp = bitcoin::secp256k1::Secp256k1::new();

    let master = Xpriv::new_master(Network::Bitcoin, &seed)
        .map_err(|e| MiMinerError::Config(format!("Failed to create master key: {e}")))?;

    let path = DerivationPath::from_str(BIP84_PATH)
        .map_err(|e| MiMinerError::Config(format!("Invalid derivation path: {e}")))?;

    let child = master
        .derive_priv(&secp, &path)
        .map_err(|e| MiMinerError::Config(format!("Key derivation failed: {e}")))?;

    let pubkey = child.to_priv().public_key(&secp);
    let compressed = CompressedPublicKey(pubkey.inner);
    let address = Address::p2wpkh(&compressed, Network::Bitcoin);

    Ok(address.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_address_from_known_mnemonic() {
        let mnemonic = bip39::Mnemonic::parse(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
        ).unwrap();

        let address = derive_address_from_mnemonic(&mnemonic).unwrap();

        assert!(address.starts_with("bc1q"), "Expected bc1q address, got: {address}");
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

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let passphrase = "test-passphrase-123";

        let encrypted = encrypt_mnemonic(mnemonic, passphrase).unwrap();
        let decrypted = decrypt_mnemonic(&encrypted, passphrase).unwrap();
        assert_eq!(decrypted, mnemonic);
    }

    #[test]
    fn test_decrypt_wrong_passphrase_fails() {
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let encrypted = encrypt_mnemonic(mnemonic, "correct").unwrap();
        let result = decrypt_mnemonic(&encrypted, "wrong");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Incorrect passphrase"));
    }

    #[test]
    fn test_encrypt_produces_different_ciphertext() {
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let e1 = encrypt_mnemonic(mnemonic, "pass").unwrap();
        let e2 = encrypt_mnemonic(mnemonic, "pass").unwrap();
        // Different random salt+nonce each time
        assert_ne!(e1, e2);
        // But both decrypt to the same plaintext
        assert_eq!(decrypt_mnemonic(&e1, "pass").unwrap(), mnemonic);
        assert_eq!(decrypt_mnemonic(&e2, "pass").unwrap(), mnemonic);
    }

    #[test]
    fn test_encrypt_with_empty_passphrase() {
        // encrypt_mnemonic itself does not reject empty passphrase —
        // that check is in generate_wallet/restore_wallet.
        // Empty passphrase should still produce valid encrypted output.
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let encrypted = encrypt_mnemonic(mnemonic, "").unwrap();
        assert!(!encrypted.is_empty());

        // Should decrypt successfully with the same empty passphrase
        let decrypted = decrypt_mnemonic(&encrypted, "").unwrap();
        assert_eq!(decrypted, mnemonic);

        // Should fail with a non-empty passphrase
        let result = decrypt_mnemonic(&encrypted, "wrong");
        assert!(result.is_err());
    }

    #[test]
    fn test_decrypt_with_corrupted_data_too_short() {
        // Encrypted data must be at least SALT_LEN(16) + NONCE_LEN(12) + 16 (GCM tag) = 44 bytes.
        // Provide something shorter.
        let short_hex = hex::encode(&[0u8; 20]); // only 20 bytes, way too short
        let result = decrypt_mnemonic(&short_hex, "pass");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("too short"),
            "Expected 'too short' error, got: {err_msg}"
        );
    }

    #[test]
    fn test_decrypt_with_data_exactly_at_minimum_boundary() {
        // Exactly SALT_LEN + NONCE_LEN + 16 = 44 bytes, but garbage data
        // should fail decryption (bad GCM tag), not the length check
        let garbage = vec![0xABu8; 44];
        let hex_str = hex::encode(&garbage);
        let result = decrypt_mnemonic(&hex_str, "pass");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Incorrect passphrase"),
            "Expected GCM auth failure, got: {err_msg}"
        );
    }

    #[test]
    fn test_decrypt_with_invalid_hex() {
        let result = decrypt_mnemonic("not_valid_hex_ZZZZ!!!", "pass");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Invalid encrypted data"),
            "Expected hex decode error, got: {err_msg}"
        );
    }

    #[test]
    fn test_decrypt_with_empty_string() {
        let result = decrypt_mnemonic("", "pass");
        assert!(result.is_err());
        // Empty hex decodes to empty bytes, which is too short
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("too short"),
            "Expected 'too short' error for empty input, got: {err_msg}"
        );
    }
}
