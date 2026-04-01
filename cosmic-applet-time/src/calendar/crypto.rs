// SPDX-License-Identifier: GPL-3.0-only

//! Cache-file encryption using ChaCha20-Poly1305 (AEAD).
//!
//! File format: `[12-byte nonce][ciphertext + 16-byte Poly1305 tag]`.

use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit,
    aead::{Aead, OsRng},
};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// 256-bit encryption key with automatic memory scrubbing.
#[derive(Clone, Debug, Zeroize, ZeroizeOnDrop)]
pub struct EncryptionKey([u8; 32]);

impl EncryptionKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Errors from encrypt/decrypt operations.
#[derive(Debug)]
pub enum CryptoError {
    /// Ciphertext too short to contain nonce.
    DataTooShort,
    /// AEAD decryption failed (wrong key, corrupted, or tampered).
    DecryptionFailed,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DataTooShort => f.write_str("encrypted data too short"),
            Self::DecryptionFailed => f.write_str("decryption failed (wrong key or corrupted data)"),
        }
    }
}

const NONCE_LEN: usize = 12;

/// Encrypt `plaintext` with the given key.
///
/// Returns `nonce || ciphertext` as raw bytes.
pub fn encrypt(plaintext: &[u8], key: &EncryptionKey) -> Vec<u8> {
    use chacha20poly1305::aead::AeadCore;

    let cipher = ChaCha20Poly1305::new((&key.0).into());
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .expect("ChaCha20-Poly1305 encryption should not fail");

    let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    out
}

/// Decrypt data produced by [`encrypt`].
///
/// Expects `[12-byte nonce][ciphertext + tag]`.
pub fn decrypt(data: &[u8], key: &EncryptionKey) -> Result<Vec<u8>, CryptoError> {
    if data.len() < NONCE_LEN + 1 {
        return Err(CryptoError::DataTooShort);
    }
    let (nonce_bytes, ciphertext) = data.split_at(NONCE_LEN);
    let nonce = chacha20poly1305::Nonce::from_slice(nonce_bytes);

    let cipher = ChaCha20Poly1305::new((&key.0).into());
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| CryptoError::DecryptionFailed)
}

/// Generate a random 256-bit encryption key.
pub fn generate_key() -> EncryptionKey {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    EncryptionKey(bytes)
}

/// Derive a 256-bit key from a passphrase + salt using Argon2id.
///
/// The salt should be stored alongside the encrypted data (it is not secret).
pub fn derive_key_from_passphrase(passphrase: &str, salt: &[u8; 16]) -> EncryptionKey {
    use argon2::Argon2;

    let mut output = [0u8; 32];
    // Argon2id with reasonable defaults (m=19456 KiB, t=2, p=1)
    Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, &mut output)
        .expect("Argon2 key derivation should not fail with valid params");
    EncryptionKey(output)
}

/// Generate a random 16-byte salt for passphrase-based key derivation.
pub fn generate_salt() -> [u8; 16] {
    use rand::RngCore;
    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    salt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_round_trip() {
        let key = generate_key();
        let plaintext = b"hello, calendar cache!";
        let encrypted = encrypt(plaintext, &key);
        let decrypted = decrypt(&encrypted, &key).unwrap();
        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn decrypt_wrong_key_fails() {
        let key1 = generate_key();
        let key2 = generate_key();
        let encrypted = encrypt(b"secret data", &key1);
        assert!(decrypt(&encrypted, &key2).is_err());
    }

    #[test]
    fn decrypt_corrupted_data_fails() {
        let key = generate_key();
        let mut encrypted = encrypt(b"secret data", &key);
        // Flip a byte in the ciphertext
        if let Some(byte) = encrypted.last_mut() {
            *byte ^= 0xff;
        }
        assert!(decrypt(&encrypted, &key).is_err());
    }

    #[test]
    fn decrypt_too_short_fails() {
        let key = generate_key();
        assert!(decrypt(&[0u8; 5], &key).is_err());
    }

    #[test]
    fn derive_key_deterministic() {
        let salt = generate_salt();
        let key1 = derive_key_from_passphrase("my passphrase", &salt);
        let key2 = derive_key_from_passphrase("my passphrase", &salt);
        assert_eq!(key1.as_bytes(), key2.as_bytes());
    }

    #[test]
    fn derive_key_different_passphrase() {
        let salt = generate_salt();
        let key1 = derive_key_from_passphrase("passphrase1", &salt);
        let key2 = derive_key_from_passphrase("passphrase2", &salt);
        assert_ne!(key1.as_bytes(), key2.as_bytes());
    }
}
