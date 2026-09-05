use crate::Result;

/// Where the Vortex session token lives at rest.
///
/// Upstream encrypted the token with AES-256-GCM under a key written to
/// `~/.config/tempest/vortex.key` with mode 0600 — reasonable on a desktop with
/// per-user home directories. On Android the equivalent guarantee comes from
/// the hardware-backed keystore, which the Rust core cannot reach directly, so
/// the storage decision is delegated to the platform.
pub trait SecretStore: Send + Sync {
    /// Retrieve a stored secret, or `None` when it was never set.
    fn get(&self, key: &str) -> Result<Option<String>>;
    /// Store (or replace) a secret.
    fn set(&self, key: &str, value: &str) -> Result<()>;
    /// Remove a secret. Removing a missing key is not an error.
    fn delete(&self, key: &str) -> Result<()>;
    /// Short description for the diagnostics screen, e.g.
    /// "Android Keystore (AES-256-GCM, StrongBox: no)".
    fn describe(&self) -> String;
}

/// The key under which the Vortex session cookie is stored.
pub const SESSION_TOKEN_KEY: &str = "vortex.session_token";
/// The key under which the last known username is stored (not sensitive, but
/// kept alongside so a logout clears both atomically).
pub const USERNAME_KEY: &str = "vortex.username";

/// Test/desktop-fallback store that keeps secrets in memory only.
#[derive(Default)]
pub struct MemorySecretStore {
    inner: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

impl SecretStore for MemorySecretStore {
    fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.inner.lock().unwrap().get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<()> {
        self.inner
            .lock()
            .unwrap()
            .insert(key.to_string(), value.to_string());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<()> {
        self.inner.lock().unwrap().remove(key);
        Ok(())
    }
    fn describe(&self) -> String {
        "in-memory (not persisted)".to_string()
    }
}
