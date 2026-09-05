//! Android platform.
//!
//! Every root here is supplied by the Kotlin layer, because only `Context` can
//! answer them correctly: package paths differ between users, work profiles,
//! Android versions and OEM builds, so hard-coding `/data/user/0/<pkg>` is
//! wrong. The Rust side never guesses a path.
//!
//! Two Android-specific constraints shape this module:
//!
//! 1. **W^X.** Apps targeting API 29+ may not `execve()` a file in their own
//!    writable data directory; only `nativeLibraryDir` is executable. The
//!    process backend therefore refuses such paths up front with an
//!    explanation, instead of surfacing a bare `EACCES`.
//! 2. **No hardware-backed crypto in Rust.** The session token is stored
//!    through a [`SecretStore`] implemented in Kotlin over the Android
//!    Keystore, injected here as a callback.

use super::paths::TempestPaths;
use super::process::ProcessBackend;
use super::secrets::SecretStore;
use super::unix_process::UnixProcessBackend;
use super::{HostKind, Platform, PlatformInfo, UriRegistration};
use crate::Result;
use std::path::PathBuf;

/// Everything the Kotlin layer must hand to the core at startup.
#[derive(Debug, Clone)]
pub struct AndroidContext {
    /// `Context.getFilesDir()` — private, internal storage.
    pub files_dir: PathBuf,
    /// `Context.getCacheDir()` — private, evictable under storage pressure.
    pub cache_dir: PathBuf,
    /// `ApplicationInfo.nativeLibraryDir` — the only executable directory.
    pub native_lib_dir: PathBuf,
    /// Optional user-chosen location for large payloads, e.g. the result of
    /// `getExternalFilesDirs()[1]` (removable volume) when one exists.
    pub games_dir: Option<PathBuf>,
    /// `Build.VERSION.SDK_INT`.
    pub sdk_int: i32,
    /// `Build.VERSION.RELEASE`.
    pub release: String,
    /// `Build.MODEL`.
    pub model: String,
    /// `Build.SUPPORTED_ABIS[0]`.
    pub primary_abi: String,
}

pub struct AndroidPlatform {
    paths: TempestPaths,
    process: UnixProcessBackend,
    secrets: Box<dyn SecretStore>,
    ctx: AndroidContext,
}

impl AndroidPlatform {
    pub fn new(ctx: AndroidContext, secrets: Box<dyn SecretStore>) -> Result<Self> {
        let mut paths =
            TempestPaths::with_root(ctx.files_dir.join("tempest"), ctx.native_lib_dir.clone())
                .with_cache_dir(ctx.cache_dir.join("tempest"));

        if let Some(games) = &ctx.games_dir {
            paths = paths.with_games_dir(games.clone());
        }
        paths.ensure_all()?;

        Ok(Self {
            process: UnixProcessBackend::restricted_to(ctx.native_lib_dir.clone()),
            secrets,
            paths,
            ctx,
        })
    }
}

impl Platform for AndroidPlatform {
    fn paths(&self) -> &TempestPaths {
        &self.paths
    }

    fn process(&self) -> &dyn ProcessBackend {
        &self.process
    }

    fn secrets(&self) -> &dyn SecretStore {
        self.secrets.as_ref()
    }

    fn info(&self) -> PlatformInfo {
        PlatformInfo {
            kind: HostKind::Android,
            os_description: format!("Android {} (API {})", self.ctx.release, self.ctx.sdk_int),
            cpu_arch: self.ctx.primary_abi.clone(),
            device_model: Some(self.ctx.model.clone()),
            // arm64-v8a hosts cannot run x86/x64 Windows binaries natively.
            needs_x86_translation: !self.ctx.primary_abi.starts_with("x86"),
        }
    }

    fn register_uri_handler(&self) -> Result<UriRegistration> {
        // The `vortex://` intent filter is declared in AndroidManifest.xml and
        // registered by the package installer. There is deliberately nothing to
        // do at runtime — writing a .desktop file here would be meaningless.
        Ok(UriRegistration::ManifestDeclared)
    }
}

/// Bridges the Rust [`SecretStore`] to a Kotlin implementation.
///
/// The closures are supplied by `tempest-jni`, which calls back into the
/// `SecureStore` Kotlin object backed by the Android Keystore.
pub struct CallbackSecretStore {
    #[allow(clippy::type_complexity)]
    getter: Box<dyn Fn(&str) -> Result<Option<String>> + Send + Sync>,
    #[allow(clippy::type_complexity)]
    setter: Box<dyn Fn(&str, &str) -> Result<()> + Send + Sync>,
    #[allow(clippy::type_complexity)]
    deleter: Box<dyn Fn(&str) -> Result<()> + Send + Sync>,
    description: String,
}

impl CallbackSecretStore {
    pub fn new(
        getter: Box<dyn Fn(&str) -> Result<Option<String>> + Send + Sync>,
        setter: Box<dyn Fn(&str, &str) -> Result<()> + Send + Sync>,
        deleter: Box<dyn Fn(&str) -> Result<()> + Send + Sync>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            getter,
            setter,
            deleter,
            description: description.into(),
        }
    }
}

impl SecretStore for CallbackSecretStore {
    fn get(&self, key: &str) -> Result<Option<String>> {
        (self.getter)(key)
    }
    fn set(&self, key: &str, value: &str) -> Result<()> {
        (self.setter)(key, value)
    }
    fn delete(&self, key: &str) -> Result<()> {
        (self.deleter)(key)
    }
    fn describe(&self) -> String {
        self.description.clone()
    }
}
