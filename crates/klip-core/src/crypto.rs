use argon2::{Algorithm, Argon2, Params, Version};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use rand::RngCore;
use thiserror::Error;

const MAGIC: &[u8; 5] = b"KLIP1";
const NONCE_LEN: usize = 24;
const SALT_LEN: usize = 16;

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("key derivation failed: {0}")]
    Kdf(String),
    #[error("encryption failed")]
    Encrypt,
    #[error("decryption failed or invalid passphrase")]
    Decrypt,
    #[error("invalid key material: {0}")]
    InvalidKey(String),
}

pub fn generate_salt() -> [u8; SALT_LEN] {
    let mut salt = [0u8; SALT_LEN];
    rand::rng().fill_bytes(&mut salt);
    salt
}

pub fn encode_bytes(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

pub fn decode_key(b64: &str) -> Result<[u8; 32], CryptoError> {
    let bytes = B64
        .decode(b64.trim())
        .map_err(|e| CryptoError::InvalidKey(e.to_string()))?;
    bytes
        .try_into()
        .map_err(|_| CryptoError::InvalidKey("expected 32-byte key".to_string()))
}

pub fn decode_salt(b64: &str) -> Result<[u8; SALT_LEN], CryptoError> {
    let bytes = B64
        .decode(b64.trim())
        .map_err(|e| CryptoError::InvalidKey(e.to_string()))?;
    bytes
        .try_into()
        .map_err(|_| CryptoError::InvalidKey("expected 16-byte salt".to_string()))
}

/// Derives a deterministic 32-byte master key from a passphrase and 16-byte salt via Argon2id.
/// Wait: for cross-device deterministic derivation without a separate key server, devices must share
/// the same vault salt (for example, derived deterministically or stored in vault metadata).
pub fn derive_key(passphrase: &str, salt: &[u8; SALT_LEN]) -> Result<[u8; 32], CryptoError> {
    let params = Params::new(19 * 1024, 2, 1, Some(32))
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = [0u8; 32];
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, &mut out)
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    Ok(out)
}

/// Deterministic vault salt so a fresh second machine can derive the exact same key
/// from only the passphrase + bucket/vault identifier, without copying salt manually.
pub fn deterministic_vault_salt(vault_id: &str) -> [u8; SALT_LEN] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"klip-vault-salt-v1:");
    h.update(vault_id.as_bytes());
    let digest = h.finalize();
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&digest[..SALT_LEN]);
    salt
}

pub fn seal(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::from(nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Encrypt)?;
    let mut out = Vec::with_capacity(MAGIC.len() + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

pub fn open(key: &[u8; 32], aad: &[u8], envelope: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if envelope.len() < MAGIC.len() + NONCE_LEN + 16 || &envelope[..MAGIC.len()] != MAGIC {
        return Err(CryptoError::Decrypt);
    }
    let nonce_arr: [u8; NONCE_LEN] = envelope[MAGIC.len()..MAGIC.len() + NONCE_LEN]
        .try_into()
        .map_err(|_| CryptoError::Decrypt)?;
    let nonce = XNonce::from(nonce_arr);
    let ciphertext = &envelope[MAGIC.len() + NONCE_LEN..];
    let cipher = XChaCha20Poly1305::new(key.into());
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_tamper_detection() {
        let salt = deterministic_vault_salt("clip-sync");
        let key = derive_key("correct horse battery staple", &salt).unwrap();
        let sealed = seal(&key, b"events/dev1/evt1", b"secret clipboard").unwrap();
        let opened = open(&key, b"events/dev1/evt1", &sealed).unwrap();
        assert_eq!(opened, b"secret clipboard");

        assert!(open(&key, b"events/dev1/other", &sealed).is_err());
        let bad_key = derive_key("wrong passphrase", &salt).unwrap();
        assert!(open(&bad_key, b"events/dev1/evt1", &sealed).is_err());
    }
}
