use crate::Result;
use std::path::{Path, PathBuf};

/// Every filesystem location Tempest uses, resolved once at startup.
///
/// Nothing in the core may call `dirs::*`, read `$HOME`, or join a literal
/// `/usr`, `~/.config` or `/data/user/0/...`. On Android the roots come from
/// `Context.getFilesDir()` / `getCacheDir()` / `getExternalFilesDir()`, which
/// differ between users, work profiles and OEMs; on Linux they come from the
/// XDG directories.
#[derive(Debug, Clone)]
pub struct TempestPaths {
    root: PathBuf,
    config: PathBuf,
    cache: PathBuf,
    logs: PathBuf,
    runtime: PathBuf,
    vortex: PathBuf,
    games: PathBuf,
    prefix: PathBuf,
    tmp: PathBuf,
    /// Directory holding executable native binaries. On Android this is
    /// `ApplicationInfo.nativeLibraryDir`, the only place an app targeting
    /// API 29+ is allowed to `execve()` from. On Linux it is the directory
    /// containing the `tempest` binary.
    native_bin: PathBuf,
}

impl TempestPaths {
    /// Build the standard layout beneath a single writable root:
    ///
    /// ```text
    /// <root>/
    ///     config/          config.toml, key material
    ///     cache/           downloads in flight, shader caches
    ///     logs/            tempest.log
    ///     runtime/         rootfs/, wine/, dxvk/, component state
    ///     vortex/          Vortex.exe, receiver.exe
    ///     games/           game payloads (may be relocated, see with_games_dir)
    ///     prefix/          the Wine prefix
    ///     tmp/             scratch space for extraction
    /// ```
    pub fn with_root(root: impl Into<PathBuf>, native_bin: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            config: root.join("config"),
            cache: root.join("cache"),
            logs: root.join("logs"),
            runtime: root.join("runtime"),
            vortex: root.join("vortex"),
            games: root.join("games"),
            prefix: root.join("prefix"),
            tmp: root.join("tmp"),
            native_bin: native_bin.into(),
            root,
        }
    }

    /// Point large payloads at a different volume (e.g. an SD card or the
    /// app-specific external directory) while keeping config and state on
    /// internal storage.
    pub fn with_games_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.games = dir.into();
        self
    }

    /// Relocate the download cache, which is the other space-hungry directory.
    pub fn with_cache_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cache = dir.into();
        self
    }

    /// Put the config directory somewhere outside the root.
    ///
    /// Only desktop Linux needs this: XDG splits `XDG_CONFIG_HOME` from
    /// `XDG_DATA_HOME`, and an existing upstream install already has its
    /// `config.toml` under `~/.config/tempest`.
    pub fn with_config_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.config = dir.into();
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn config_dir(&self) -> &Path {
        &self.config
    }
    pub fn cache_dir(&self) -> &Path {
        &self.cache
    }
    pub fn logs_dir(&self) -> &Path {
        &self.logs
    }
    pub fn runtime_dir(&self) -> &Path {
        &self.runtime
    }
    pub fn vortex_dir(&self) -> &Path {
        &self.vortex
    }
    pub fn games_dir(&self) -> &Path {
        &self.games
    }
    pub fn wine_prefix(&self) -> &Path {
        &self.prefix
    }
    pub fn tmp_dir(&self) -> &Path {
        &self.tmp
    }
    pub fn native_bin_dir(&self) -> &Path {
        &self.native_bin
    }

    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }
    pub fn key_file(&self) -> PathBuf {
        self.config.join("vortex.key")
    }
    pub fn log_file(&self) -> PathBuf {
        self.logs.join("tempest.log")
    }

    pub fn vortex_exe(&self) -> PathBuf {
        self.vortex.join("Vortex.exe")
    }
    pub fn receiver_exe(&self) -> PathBuf {
        self.vortex.join("receiver.exe")
    }

    /// Root of the extracted Linux guest filesystem (Android only; unused on
    /// desktop, where the host filesystem *is* the guest filesystem).
    pub fn guest_rootfs(&self) -> PathBuf {
        self.runtime.join("rootfs")
    }

    /// Where per-component install state is recorded.
    pub fn runtime_state_file(&self) -> PathBuf {
        self.runtime.join("components.json")
    }

    pub fn shader_cache_dir(&self) -> PathBuf {
        self.cache.join("shaders")
    }

    /// A named executable inside the executable-permitted native directory.
    ///
    /// Android requires these to be packaged as `lib<name>.so` inside the APK
    /// so the installer places them in `nativeLibraryDir`; the mapping is
    /// applied here so callers can ask for a logical name.
    pub fn native_executable(&self, logical_name: &str) -> PathBuf {
        if cfg!(target_os = "android") {
            self.native_bin.join(format!("lib{logical_name}.so"))
        } else {
            self.native_bin.join(logical_name)
        }
    }

    /// Create every directory Tempest writes into. Called once at startup so
    /// later code never has to guess whether a parent exists.
    pub fn ensure_all(&self) -> Result<()> {
        for dir in [
            &self.root,
            &self.config,
            &self.cache,
            &self.logs,
            &self.runtime,
            &self.vortex,
            &self.games,
            &self.prefix,
            &self.tmp,
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    /// Total bytes currently occupied by the Tempest tree. Used by the storage
    /// screen; walks lazily and tolerates unreadable entries.
    pub fn disk_usage(&self) -> u64 {
        fn walk(p: &Path) -> u64 {
            let Ok(entries) = std::fs::read_dir(p) else {
                return 0;
            };
            entries
                .flatten()
                .map(|e| match e.file_type() {
                    Ok(t) if t.is_dir() => walk(&e.path()),
                    Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
                    _ => 0,
                })
                .sum()
        }
        let mut total = walk(&self.root);
        // games/ and cache/ may have been relocated outside root.
        for extra in [&self.games, &self.cache] {
            if !extra.starts_with(&self.root) {
                total += walk(extra);
            }
        }
        total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_rooted_and_contains_no_absolute_assumptions() {
        let p = TempestPaths::with_root("/anywhere/at/all", "/nativelibs");
        for dir in [
            p.config_dir(),
            p.cache_dir(),
            p.logs_dir(),
            p.runtime_dir(),
            p.vortex_dir(),
            p.games_dir(),
            p.wine_prefix(),
            p.tmp_dir(),
        ] {
            assert!(
                dir.starts_with("/anywhere/at/all"),
                "{} escaped the root",
                dir.display()
            );
        }
        assert_eq!(
            p.vortex_exe(),
            Path::new("/anywhere/at/all/vortex/Vortex.exe")
        );
        assert_eq!(
            p.receiver_exe(),
            Path::new("/anywhere/at/all/vortex/receiver.exe")
        );
    }

    #[test]
    fn games_dir_can_be_relocated_without_moving_config() {
        let p = TempestPaths::with_root("/data/app", "/nativelibs")
            .with_games_dir("/storage/sdcard/tempest-games");
        assert_eq!(p.games_dir(), Path::new("/storage/sdcard/tempest-games"));
        assert!(p.config_dir().starts_with("/data/app"));
    }

    #[test]
    fn native_executable_uses_the_platform_naming_rule() {
        let p = TempestPaths::with_root("/root", "/nativelibs");
        let got = p.native_executable("proot");
        if cfg!(target_os = "android") {
            assert_eq!(got, Path::new("/nativelibs/libproot.so"));
        } else {
            assert_eq!(got, Path::new("/nativelibs/proot"));
        }
    }
}
