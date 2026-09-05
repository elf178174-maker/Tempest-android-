//! User-editable configuration.
//!
//! Differences from upstream: the auth section is gone (the token lives in the
//! platform [`SecretStore`], not in a TOML file), paths are derived from
//! [`TempestPaths`] rather than stored, and every field has a serde default so
//! adding an option cannot make an existing config file unreadable.

use crate::platform::TempestPaths;
use crate::{Result, TempestError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub wine: WineConfig,
    pub launcher: LauncherConfig,
    pub graphics: GraphicsConfig,
    pub storage: StorageConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WineConfig {
    /// Path to the Wine binary *inside the guest filesystem* on Android, or on
    /// `PATH` on desktop.
    pub binary: String,
    /// Extra environment variables applied to every Wine invocation.
    pub env: BTreeMap<String, String>,
    /// Windows version reported by the prefix.
    pub windows_version: String,
}

impl Default for WineConfig {
    fn default() -> Self {
        Self {
            binary: "wine".to_string(),
            env: BTreeMap::new(),
            windows_version: "win10".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LauncherConfig {
    pub filter_wine_noise: bool,
    pub auto_update_vortex: bool,
    /// esync/fsync need eventfd and futex_waitv respectively. Android kernels
    /// have eventfd but `futex_waitv` only landed in 5.16, so fsync defaults
    /// off here and is enabled by the runtime probe when the kernel supports it.
    pub use_esync: bool,
    pub use_fsync: bool,
    pub shader_cache: bool,
    /// Keep the launch alive when the app goes to the background by running it
    /// under a foreground service.
    pub keep_alive_in_background: bool,
    /// Seconds to wait for Vortex.exe to appear before declaring the launch
    /// failed. The first launch on a cold prefix is slow.
    pub launch_timeout_secs: u64,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            filter_wine_noise: true,
            auto_update_vortex: true,
            use_esync: true,
            use_fsync: false,
            shader_cache: true,
            keep_alive_in_background: true,
            launch_timeout_secs: 180,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphicsConfig {
    /// Which Vulkan implementation the guest should load.
    pub vulkan_driver: VulkanDriver,
    pub enable_dxvk: bool,
    /// vkd3d-proton is only needed for D3D12 titles; off by default so the
    /// runtime install stays smaller.
    pub enable_vkd3d: bool,
    pub dxvk_hud: Option<String>,
    /// X server the guest renders to. `:0` over the Termux:X11 abstract socket
    /// is the supported configuration.
    pub display: String,
}

impl Default for GraphicsConfig {
    fn default() -> Self {
        Self {
            vulkan_driver: VulkanDriver::Auto,
            enable_dxvk: true,
            enable_vkd3d: false,
            dxvk_hud: None,
            display: ":0".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VulkanDriver {
    /// Prefer Turnip if installed, else fall back to lavapipe.
    #[default]
    Auto,
    /// Mesa Turnip — hardware Vulkan on Adreno via the KGSL backend.
    Turnip,
    /// Mesa lavapipe — software Vulkan. Slow, but proves the stack works and
    /// runs on any GPU.
    Lavapipe,
}

impl VulkanDriver {
    pub fn as_str(self) -> &'static str {
        match self {
            VulkanDriver::Auto => "auto",
            VulkanDriver::Turnip => "turnip",
            VulkanDriver::Lavapipe => "lavapipe",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// Absolute path chosen by the user for game payloads. `None` means the
    /// default inside app storage.
    pub games_dir: Option<String>,
    /// Delete a downloaded archive once it has been extracted.
    pub prune_archives_after_install: bool,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            games_dir: None,
            prune_archives_after_install: true,
        }
    }
}

impl Config {
    /// Load from `paths.config_file()`, falling back to defaults.
    ///
    /// Unlike upstream, a malformed file is reported rather than silently
    /// replaced with defaults — losing a user's settings without telling them
    /// is worse than showing an error.
    pub fn load(paths: &TempestPaths) -> Result<Self> {
        let path = paths.config_file();
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(&path)?;
        toml::from_str(&contents).map_err(|e| {
            TempestError::Config(format!("{} is not valid TOML: {e}", path.display()))
        })
    }

    /// Load, but never fail: a broken file is logged and defaults are used.
    /// Used on startup paths where refusing to start would be worse.
    pub fn load_or_default(paths: &TempestPaths) -> Self {
        match Self::load(paths) {
            Ok(c) => c,
            Err(e) => {
                crate::logging::error("config", format!("{e}; using defaults"));
                Self::default()
            }
        }
    }

    pub fn save(&self, paths: &TempestPaths) -> Result<()> {
        std::fs::create_dir_all(paths.config_dir())?;
        let contents = toml::to_string_pretty(self)
            .map_err(|e| TempestError::Config(e.to_string()))?;
        // Write-then-rename so an interrupted save cannot truncate the file.
        let tmp = paths.config_file().with_extension("toml.tmp");
        std::fs::write(&tmp, contents)?;
        std::fs::rename(&tmp, paths.config_file())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &std::path::Path) -> TempestPaths {
        TempestPaths::with_root(dir, dir.join("lib"))
    }

    #[test]
    fn round_trips_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let mut cfg = Config::default();
        cfg.wine.env.insert("DXVK_HUD".into(), "fps".into());
        cfg.graphics.vulkan_driver = VulkanDriver::Turnip;
        cfg.launcher.launch_timeout_secs = 42;
        cfg.save(&p).unwrap();

        let loaded = Config::load(&p).unwrap();
        assert_eq!(loaded.wine.env.get("DXVK_HUD").map(String::as_str), Some("fps"));
        assert_eq!(loaded.graphics.vulkan_driver, VulkanDriver::Turnip);
        assert_eq!(loaded.launcher.launch_timeout_secs, 42);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load(&paths(dir.path())).unwrap();
        assert!(cfg.launcher.filter_wine_noise);
        assert!(cfg.graphics.enable_dxvk);
        assert!(!cfg.graphics.enable_vkd3d, "vkd3d should be opt-in");
    }

    #[test]
    fn an_old_config_missing_new_sections_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        std::fs::create_dir_all(p.config_dir()).unwrap();
        std::fs::write(
            p.config_file(),
            "[wine]\nbinary = \"/opt/wine/bin/wine\"\n",
        )
        .unwrap();
        let cfg = Config::load(&p).unwrap();
        assert_eq!(cfg.wine.binary, "/opt/wine/bin/wine");
        // Sections absent from the file fall back to defaults.
        assert_eq!(cfg.graphics.display, ":0");
        assert!(cfg.launcher.use_esync);
    }

    #[test]
    fn a_malformed_file_is_an_error_not_a_silent_reset() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        std::fs::create_dir_all(p.config_dir()).unwrap();
        std::fs::write(p.config_file(), "this is not = = toml").unwrap();
        let err = Config::load(&p).unwrap_err();
        assert_eq!(err.kind(), "config");
        // ...but the non-failing entry point still starts up.
        let _ = Config::load_or_default(&p);
    }

    #[test]
    fn config_never_carries_credentials() {
        // A regression guard: the token belongs in the SecretStore. If someone
        // adds an auth field back to Config, this fails.
        let serialized = toml::to_string_pretty(&Config::default()).unwrap();
        let lower = serialized.to_lowercase();
        for banned in ["token", "password", "session"] {
            assert!(!lower.contains(banned), "config exposes '{banned}':\n{serialized}");
        }
    }
}
