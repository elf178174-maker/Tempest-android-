//! AES-256-GCM helpers for the file-backed desktop secret store.
//!
//! On Android this module is unused: the token is held by the Android
//! Keystore through [`crate::platform::secrets::SecretStore`], which gives a
//! hardware-backed key rather than one sitting next to the ciphertext.

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use base64::Engine;
use rand::RngCore;
use std::path::Path;

const KEY_FILE: &str = "vortex.key";
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;

fn load_or_create_key(dir: &Path) -> crate::Result<[u8; KEY_LEN]> {
    let path = dir.join(KEY_FILE);
    if path.exists() {
        let raw = std::fs::read(&path)?;
        if raw.len() == KEY_LEN {
            let mut key = [0u8; KEY_LEN];
            key.copy_from_slice(&raw);
            return Ok(key);
        }
        // A truncated key file means every previously stored secret is
        // unrecoverable. Say so rather than silently minting a new key and
        // leaving the user with a token that decrypts to garbage.
        crate::logging::warn(
            "crypto",
            format!(
                "{} is {} bytes, expected {KEY_LEN}; generating a new key — \
                 stored secrets will need to be re-entered",
                path.display(),
                raw.len()
            ),
        );
    }

    let mut key = [0u8; KEY_LEN];
    OsRng.fill_bytes(&mut key);
    std::fs::create_dir_all(dir)?;
    std::fs::write(&path, key)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(key)
}

pub fn encrypt(key_dir: &Path, plaintext: &str) -> crate::Result<String> {
    let key = load_or_create_key(key_dir)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| crate::TempestError::other(format!("bad key length: {e}")))?;

    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    // Upstream used `.expect("encryption failed")` here, which would abort the
    // whole process on an allocation failure inside a launcher. Propagate.
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|_| crate::TempestError::other("AES-GCM encryption failed"))?;

    let mut combined = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&ciphertext);
    Ok(base64::engine::general_purpose::STANDARD.encode(&combined))
}

/// Returns `None` when the input is not ciphertext this key can open, which is
/// how callers distinguish "already plaintext" from "corrupt".
pub fn decrypt(key_dir: &Path, encoded: &str) -> Option<String> {
    let key = load_or_create_key(key_dir).ok()?;
    let cipher = Aes256Gcm::new_from_slice(&key).ok()?;
    let combined = base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
    if combined.len() <= NONCE_LEN {
        return None;
    }
    let (nonce_bytes, ciphertext) = combined.split_at(NONCE_LEN);
    cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let ct = encrypt(dir.path(), "session-abc-123").unwrap();
        assert_ne!(ct, "session-abc-123");
        assert_eq!(decrypt(dir.path(), &ct).as_deref(), Some("session-abc-123"));
    }

    #[test]
    fn nonce_is_fresh_per_encryption() {
        let dir = tempfile::tempdir().unwrap();
        assert_ne!(
            encrypt(dir.path(), "same").unwrap(),
            encrypt(dir.path(), "same").unwrap()
        );
    }

    #[test]
    fn decrypt_rejects_plaintext_and_foreign_ciphertext() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        assert_eq!(decrypt(a.path(), "not base64 at all!"), None);
        let ct = encrypt(a.path(), "secret").unwrap();
        assert_eq!(decrypt(b.path(), &ct), None, "decrypted under the wrong key");
    }

    #[test]
    fn key_file_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        encrypt(dir.path(), "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(KEY_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "key file is world/group readable");
        }
    }
}
