//! Desktop Linux platform. This preserves the behaviour of upstream Tempest:
//! XDG directories, freedesktop URI registration, and a file-backed encrypted
//! secret store.

use super::paths::TempestPaths;
use super::process::ProcessBackend;
use super::secrets::SecretStore;
use super::unix_process::UnixProcessBackend;
use super::{HostKind, Platform, PlatformInfo, UriRegistration};
use crate::{Result, TempestError};
use std::path::PathBuf;

pub struct LinuxPlatform {
    paths: TempestPaths,
    process: UnixProcessBackend,
    secrets: FileSecretStore,
}

impl LinuxPlatform {
    /// Resolve the XDG layout. Upstream used `dirs::data_local_dir()` with a
    /// literal `~/.local/share` fallback, which is not a real path once it
    /// reaches `std::fs`; here the fallback is resolved against `$HOME`.
    pub fn new() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| TempestError::Config("$HOME is not set".into()))?;

        let data_root = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("tempest");

        let config_root = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("tempest");

        let cache_root = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cache"))
            .join("tempest");

        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/usr/local/bin"));

        // Keep upstream's split: config under XDG_CONFIG_HOME, everything else
        // under XDG_DATA_HOME. `with_root` would have put config inside the
        // data root, so override it explicitly.
        let paths = TempestPaths::with_root(&data_root, exe_dir)
            .with_cache_dir(&cache_root)
            .with_config_dir(&config_root);

        Ok(Self {
            secrets: FileSecretStore::new(paths.config_dir().to_path_buf()),
            process: UnixProcessBackend::permissive(),
            paths,
        })
    }
}

impl Platform for LinuxPlatform {
    fn paths(&self) -> &TempestPaths {
        &self.paths
    }

    fn process(&self) -> &dyn ProcessBackend {
        &self.process
    }

    fn secrets(&self) -> &dyn SecretStore {
        &self.secrets
    }

    fn info(&self) -> PlatformInfo {
        PlatformInfo {
            kind: HostKind::LinuxDesktop,
            os_description: read_os_release().unwrap_or_else(|| "Linux".to_string()),
            cpu_arch: std::env::consts::ARCH.to_string(),
            device_model: None,
            needs_x86_translation: std::env::consts::ARCH != "x86_64",
        }
    }

    fn register_uri_handler(&self) -> Result<UriRegistration> {
        let exe = std::env::current_exe()?;
        let apps_dir = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
            })
            .join("applications");
        std::fs::create_dir_all(&apps_dir)?;

        let desktop = apps_dir.join("tempest-vortex.desktop");
        std::fs::write(
            &desktop,
            format!(
                "[Desktop Entry]\n\
                 Name=Tempest (Vortex Launcher)\n\
                 Exec={} uri-handler %u\n\
                 Type=Application\n\
                 MimeType=x-scheme-handler/vortex;\n\
                 NoDisplay=true\n",
                exe.display()
            ),
        )?;

        for (prog, args) in [
            ("xdg-mime", vec!["default", "tempest-vortex.desktop", "x-scheme-handler/vortex"]),
            ("gio", vec!["mime", "x-scheme-handler/vortex", "tempest-vortex.desktop"]),
        ] {
            std::process::Command::new(prog).args(args).status().ok();
        }
        std::process::Command::new("update-desktop-database")
            .arg(&apps_dir)
            .status()
            .ok();

        Ok(UriRegistration::Desktop)
    }
}

fn read_os_release() -> Option<String> {
    let contents = std::fs::read_to_string("/etc/os-release").ok()?;
    contents.lines().find_map(|l| {
        l.strip_prefix("PRETTY_NAME=")
            .map(|v| v.trim_matches('"').to_string())
    })
}

/// AES-256-GCM at rest under a 0600 key file — upstream's scheme, kept intact
/// so an existing desktop install keeps working.
pub struct FileSecretStore {
    dir: PathBuf,
}

impl FileSecretStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn file(&self, key: &str) -> PathBuf {
        // Secret names are internal constants, never user input, but sanitise
        // anyway so a future caller cannot traverse out of the directory.
        let safe: String = key
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
            .collect();
        self.dir.join(format!("{safe}.secret"))
    }
}

impl SecretStore for FileSecretStore {
    fn get(&self, key: &str) -> Result<Option<String>> {
        let path = self.file(key);
        if !path.exists() {
            return Ok(None);
        }
        let encoded = std::fs::read_to_string(&path)?;
        Ok(crate::crypto::decrypt(&self.dir, encoded.trim()))
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let encoded = crate::crypto::encrypt(&self.dir, value)?;
        let path = self.file(key);
        std::fs::write(&path, encoded)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).ok();
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<()> {
        let path = self.file(key);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    fn describe(&self) -> String {
        format!(
            "encrypted file store (AES-256-GCM, key at {}/vortex.key, mode 0600)",
            self.dir.display()
        )
    }
}
