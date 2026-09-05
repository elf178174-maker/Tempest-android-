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

    fn uri_handler_status(&self) -> Result<UriRegistration> {
        Ok(UriRegistration::ManifestDeclared)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::secrets::MemorySecretStore;
    use crate::platform::{HostKind, ProcessSpec, UriRegistration};

    fn ctx(dir: &std::path::Path) -> AndroidContext {
        AndroidContext {
            files_dir: dir.join("data/user/0/io.tempest.android/files"),
            cache_dir: dir.join("data/user/0/io.tempest.android/cache"),
            native_lib_dir: dir.join("data/app/io.tempest.android/lib/arm64"),
            games_dir: None,
            sdk_int: 35,
            release: "15".into(),
            model: "POCO F7 Ultra".into(),
            primary_abi: "arm64-v8a".into(),
        }
    }

    fn platform(dir: &std::path::Path) -> AndroidPlatform {
        AndroidPlatform::new(ctx(dir), Box::new(MemorySecretStore::default())).unwrap()
    }

    #[test]
    fn every_path_is_derived_from_the_injected_context() {
        let dir = tempfile::tempdir().unwrap();
        let p = platform(dir.path());
        let paths = p.paths();

        // Nothing may be hard-coded: each root must sit under what Kotlin gave us.
        assert!(paths.root().starts_with(dir.path()));
        assert!(paths.config_dir().starts_with(dir.path().join("data/user/0")));
        assert!(paths.cache_dir().starts_with(dir.path().join("data/user/0")));
        assert!(paths.native_bin_dir().ends_with("lib/arm64"));
        assert!(paths.guest_rootfs().starts_with(paths.runtime_dir()));
    }

    #[test]
    fn a_different_android_user_gets_a_different_tree() {
        // Work profiles and secondary users have package paths under
        // /data/user/<id>; nothing may assume user 0.
        let dir = tempfile::tempdir().unwrap();
        let mut other = ctx(dir.path());
        other.files_dir = dir.path().join("data/user/10/io.tempest.android/files");
        let p = AndroidPlatform::new(other, Box::new(MemorySecretStore::default())).unwrap();
        assert!(p.paths().root().starts_with(dir.path().join("data/user/10")));
    }

    #[test]
    fn startup_creates_the_whole_directory_layout() {
        let dir = tempfile::tempdir().unwrap();
        let p = platform(dir.path());
        for d in [
            p.paths().config_dir(),
            p.paths().cache_dir(),
            p.paths().logs_dir(),
            p.paths().runtime_dir(),
            p.paths().vortex_dir(),
            p.paths().games_dir(),
            p.paths().wine_prefix(),
            p.paths().tmp_dir(),
        ] {
            assert!(d.is_dir(), "{} was not created", d.display());
        }
    }

    #[test]
    fn a_relocated_games_directory_is_honoured() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = ctx(dir.path());
        c.games_dir = Some(dir.path().join("storage/sdcard/tempest"));
        let p = AndroidPlatform::new(c, Box::new(MemorySecretStore::default())).unwrap();
        assert_eq!(p.paths().games_dir(), dir.path().join("storage/sdcard/tempest"));
        // Config stays on internal storage even so.
        assert!(p.paths().config_dir().starts_with(dir.path().join("data/user/0")));
    }

    #[test]
    fn execution_is_confined_to_the_native_library_directory() {
        let dir = tempfile::tempdir().unwrap();
        let p = platform(dir.path());
        let native = p.paths().native_bin_dir().to_path_buf();

        assert!(p.process().can_execute(&native.join("libproot.so")));
        // The classic Android mistake: downloading a binary into app data and
        // trying to run it. The platform forbids this, so we must too.
        assert!(!p.process().can_execute(&p.paths().runtime_dir().join("rootfs/usr/bin/wine")));
        assert!(!p.process().can_execute(std::path::Path::new("/system/bin/sh")));
    }

    #[test]
    fn attempting_to_run_something_from_app_data_explains_the_platform_rule() {
        let dir = tempfile::tempdir().unwrap();
        let p = platform(dir.path());
        let target = p.paths().runtime_dir().join("box64");
        std::fs::write(&target, b"#!/bin/sh\n").unwrap();

        let msg = match p.process().spawn(ProcessSpec::new("box64", &target)) {
            Ok(_) => panic!("Android platform executed a file from app data"),
            Err(e) => e.to_string(),
        };
        assert!(msg.contains("native library directory"), "{msg}");
    }

    #[test]
    fn native_executables_use_the_lib_prefix_android_requires() {
        let dir = tempfile::tempdir().unwrap();
        let p = platform(dir.path());
        let proot = p.paths().native_executable("proot");
        // Only files matching lib*.so are extracted into nativeLibraryDir by
        // the package installer, so the mapping must be applied.
        let name = proot.file_name().unwrap().to_string_lossy().to_string();
        if cfg!(target_os = "android") {
            assert_eq!(name, "libproot.so");
        } else {
            assert_eq!(name, "proot");
        }
    }

    #[test]
    fn platform_info_reports_the_device_and_the_need_for_translation() {
        let dir = tempfile::tempdir().unwrap();
        let info = platform(dir.path()).info();
        assert_eq!(info.kind, HostKind::Android);
        assert_eq!(info.device_model.as_deref(), Some("POCO F7 Ultra"));
        assert_eq!(info.os_description, "Android 15 (API 35)");
        assert!(info.needs_x86_translation, "arm64 must report needing translation");
    }

    #[test]
    fn an_x86_android_device_does_not_claim_to_need_translation() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = ctx(dir.path());
        c.primary_abi = "x86_64".into();
        let p = AndroidPlatform::new(c, Box::new(MemorySecretStore::default())).unwrap();
        assert!(!p.info().needs_x86_translation);
    }

    #[test]
    fn uri_registration_is_a_manifest_concern_not_a_runtime_one() {
        let dir = tempfile::tempdir().unwrap();
        let p = platform(dir.path());
        assert_eq!(p.register_uri_handler().unwrap(), UriRegistration::ManifestDeclared);
        assert_eq!(p.uri_handler_status().unwrap(), UriRegistration::ManifestDeclared);
    }

    #[test]
    fn callback_secret_store_forwards_every_operation() {
        use std::sync::{Arc, Mutex};
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let (a, b, c) = (log.clone(), log.clone(), log.clone());
        let store = CallbackSecretStore::new(
            Box::new(move |k| {
                a.lock().unwrap().push(format!("get {k}"));
                Ok(Some("value".into()))
            }),
            Box::new(move |k, v| {
                b.lock().unwrap().push(format!("set {k}={v}"));
                Ok(())
            }),
            Box::new(move |k| {
                c.lock().unwrap().push(format!("del {k}"));
                Ok(())
            }),
            "Android Keystore (AES-256-GCM)",
        );

        assert_eq!(store.get("k").unwrap().as_deref(), Some("value"));
        store.set("k", "v").unwrap();
        store.delete("k").unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            vec!["get k", "set k=v", "del k"]
        );
        assert!(store.describe().contains("Keystore"));
    }

    #[test]
    fn a_failing_keystore_surfaces_as_an_error_not_a_missing_value() {
        let store = CallbackSecretStore::new(
            Box::new(|_| Err(crate::TempestError::Auth("keystore key invalidated".into()))),
            Box::new(|_, _| Ok(())),
            Box::new(|_| Ok(())),
            "test",
        );
        let err = store.get("k").unwrap_err();
        assert_eq!(err.kind(), "auth");
    }
}
