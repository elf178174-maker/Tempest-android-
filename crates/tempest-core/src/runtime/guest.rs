//! The guest Linux environment.
//!
//! On Android, Wine cannot run directly: it is a glibc program and Android's C
//! library is Bionic, and Android will not let the app `execve()` anything it
//! wrote to its own data directory. Both problems are solved the same way as
//! every other unprivileged Linux-on-Android project does it — with **PRoot**.
//!
//! PRoot ships inside the APK as `libproot.so` in `nativeLibraryDir`, the one
//! directory an app may execute from. It `ptrace()`s its children and rewrites
//! path-related syscalls so the extracted Ubuntu tree appears at `/`. Crucially,
//! when a guest program is executed PRoot does not hand the guest binary to
//! `execve()`; it executes its own loader (also in `nativeLibraryDir`) and the
//! loader maps the guest ELF itself. Mapping executable pages from app data is
//! allowed — it is what `dlopen()` does — so this stays inside the platform's
//! rules rather than trying to defeat them.
//!
//! On desktop Linux there is no guest: commands run directly on the host, and
//! [`GuestEnv::command`] returns them unwrapped.

use crate::config::{Config, VulkanDriver};
use crate::platform::{PlatformRef, ProcessSpec, TempestPaths};
use crate::{Result, TempestError};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Logical name of the PRoot binary inside the APK's native library directory.
pub const PROOT_BIN: &str = "proot";
/// PRoot's ELF loaders, which must sit next to it.
pub const PROOT_LOADER: &str = "proot-loader";
pub const PROOT_LOADER32: &str = "proot-loader32";

/// Where the Hangover `.deb` files are staged inside the guest before apt
/// installs them.
pub const GUEST_STAGING: &str = "/tmp/tempest-staging";

pub struct GuestEnv {
    rootfs: PathBuf,
    proot: PathBuf,
    loader: PathBuf,
    loader32: PathBuf,
    /// False on desktop Linux, where commands run on the host directly.
    containerised: bool,
}

impl GuestEnv {
    pub fn new(platform: &PlatformRef) -> Self {
        let paths = platform.paths();
        Self {
            rootfs: paths.guest_rootfs(),
            proot: paths.native_executable(PROOT_BIN),
            loader: paths.native_executable(PROOT_LOADER),
            loader32: paths.native_executable(PROOT_LOADER32),
            containerised: platform.info().kind == crate::platform::HostKind::Android,
        }
    }

    pub fn rootfs(&self) -> &Path {
        &self.rootfs
    }

    pub fn is_containerised(&self) -> bool {
        self.containerised
    }

    /// The Wine prefix **as the guest sees it**.
    ///
    /// Inside the container it is the bind-mount target; on the desktop there
    /// is no container, so it is the host path. Scripts that need the prefix
    /// take it as a positional argument rather than hard-coding either form.
    pub fn guest_prefix(&self, paths: &TempestPaths) -> String {
        if self.containerised {
            "/home/tempest/.wine".to_string()
        } else {
            paths.wine_prefix().display().to_string()
        }
    }

    /// Check that everything needed to enter the guest is present, with an
    /// error that says exactly which piece is missing and what to do about it.
    pub fn preflight(&self) -> Result<()> {
        if !self.containerised {
            return Ok(());
        }
        if !self.proot.exists() {
            return Err(TempestError::missing(
                "PRoot",
                format!(
                    "{} is not in the APK. The app was built without the native \
                     PRoot binary, so no Linux program can be started. Reinstall \
                     a build produced by the project's CI workflow.",
                    self.proot.display()
                ),
            ));
        }
        if !self.loader.exists() {
            return Err(TempestError::missing(
                "PRoot loader",
                format!(
                    "{} is missing. PRoot cannot start guest programs without its \
                     loader.",
                    self.loader.display()
                ),
            ));
        }
        if !self.rootfs.join("usr/bin/env").exists() {
            return Err(TempestError::missing(
                "Linux guest filesystem",
                "the Ubuntu base image has not been installed yet — install the \
                 runtime components from Settings first",
            ));
        }
        Ok(())
    }

    /// Bind mounts made available inside the guest.
    ///
    /// `/dev`, `/proc` and `/sys` come from Android. `/dev/dri` and `/dev/kgsl*`
    /// are what a Vulkan driver needs to reach the GPU; they are bound when
    /// present and simply absent otherwise, which the diagnostics report.
    fn bindings(&self, paths: &TempestPaths) -> Vec<(String, String)> {
        let mut binds: Vec<(String, String)> = vec![
            ("/dev".into(), "/dev".into()),
            ("/proc".into(), "/proc".into()),
            ("/sys".into(), "/sys".into()),
            // Android's ashmem/binder nodes; harmless when missing.
            ("/dev/urandom".into(), "/dev/random".into()),
        ];

        for gpu in ["/dev/dri", "/dev/kgsl-3d0", "/dev/mali0"] {
            if Path::new(gpu).exists() {
                binds.push((gpu.into(), gpu.into()));
            }
        }

        // Game payloads and the Wine prefix live outside the rootfs so the user
        // can put them on another volume and so wiping the runtime does not
        // destroy save data.
        binds.push((
            paths.wine_prefix().display().to_string(),
            "/home/tempest/.wine".into(),
        ));
        binds.push((paths.games_dir().display().to_string(), "/games".into()));
        binds.push((paths.vortex_dir().display().to_string(), "/vortex".into()));
        binds.push((
            paths.shader_cache_dir().display().to_string(),
            "/shadercache".into(),
        ));

        binds
    }

    /// Wrap a guest command line in the PRoot invocation that runs it.
    ///
    /// On desktop the command is returned unchanged.
    pub fn command(
        &self,
        paths: &TempestPaths,
        label: &str,
        program: &str,
        args: &[String],
        env: BTreeMap<String, String>,
    ) -> Result<ProcessSpec> {
        if !self.containerised {
            let spec = ProcessSpec::new(label, program)
                .args(args.iter().cloned())
                .envs(env);
            return Ok(spec);
        }

        self.preflight()?;

        let mut proot_args: Vec<String> = vec![
            "-r".into(),
            self.rootfs.display().to_string(),
            // Run as uid/gid 0 inside the guest. Nothing is actually privileged;
            // it just stops apt and dpkg refusing to run.
            "-0".into(),
            // Resolve the guest's own symlinks rather than the host's.
            "--link2symlink".into(),
            // Kill the whole guest process group when PRoot exits, so a game
            // cannot outlive the session and keep the CPU busy in the background.
            "--kill-on-exit".into(),
            "-w".into(),
            "/home/tempest".into(),
        ];

        for (host, guest) in self.bindings(paths) {
            proot_args.push("-b".into());
            proot_args.push(format!("{host}:{guest}"));
        }

        proot_args.push("/usr/bin/env".into());
        proot_args.push("-i".into());
        for (k, v) in &env {
            proot_args.push(format!("{k}={v}"));
        }
        proot_args.push(program.to_string());
        proot_args.extend(args.iter().cloned());

        Ok(ProcessSpec::new(label, &self.proot)
            .args(proot_args)
            // PRoot's own environment, distinct from the guest's.
            .env("PROOT_LOADER", self.loader.display().to_string())
            .env("PROOT_LOADER_32", self.loader32.display().to_string())
            .env("PROOT_TMP_DIR", paths.tmp_dir().display().to_string())
            .env("PROOT_NO_SECCOMP", "1"))
    }

    /// Run a shell command inside the guest.
    ///
    /// The command string is a fixed template from this crate — never anything
    /// derived from a URI, a game name or a server response — so passing it to
    /// `sh -c` is safe. Values that vary are passed as positional parameters.
    pub fn shell(
        &self,
        paths: &TempestPaths,
        label: &str,
        script: &str,
        script_args: &[String],
        env: BTreeMap<String, String>,
    ) -> Result<ProcessSpec> {
        let mut args = vec!["-c".to_string(), script.to_string(), "sh".to_string()];
        args.extend(script_args.iter().cloned());
        self.command(paths, label, "/bin/sh", &args, env)
    }
}

/// Base environment for every guest process.
pub fn base_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert(
        "PATH".into(),
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into(),
    );
    env.insert("HOME".into(), "/home/tempest".into());
    env.insert("USER".into(), "tempest".into());
    env.insert("TMPDIR".into(), "/tmp".into());
    env.insert("LANG".into(), "C.UTF-8".into());
    env.insert("TERM".into(), "xterm-256color".into());
    // Ubuntu images have no machine-id; some libraries warn loudly without one.
    env.insert("DEBIAN_FRONTEND".into(), "noninteractive".into());
    env
}

/// Environment for a Wine invocation, assembled from config.
pub fn wine_env(
    config: &Config,
    guest: &GuestEnv,
    paths: &TempestPaths,
) -> BTreeMap<String, String> {
    let mut env = base_env();

    env.insert("WINEPREFIX".into(), guest.guest_prefix(paths));
    // Wine's own noise is filtered for display, but keeping err+fixme in the
    // stream is what makes the log useful when something breaks.
    env.insert("WINEDEBUG".into(), "err+all,fixme-all".into());

    if config.launcher.use_esync {
        env.insert("WINEESYNC".into(), "1".into());
    }
    if config.launcher.use_fsync {
        env.insert("WINEFSYNC".into(), "1".into());
    }

    env.insert("DISPLAY".into(), config.graphics.display.clone());

    if config.launcher.shader_cache {
        env.insert("DXVK_STATE_CACHE_PATH".into(), "/shadercache".into());
        env.insert("VKD3D_SHADER_CACHE_PATH".into(), "/shadercache".into());
        env.insert("MESA_SHADER_CACHE_DIR".into(), "/shadercache".into());
    }

    if let Some(hud) = &config.graphics.dxvk_hud {
        env.insert("DXVK_HUD".into(), hud.clone());
    }

    match config.graphics.vulkan_driver {
        VulkanDriver::Auto => {}
        VulkanDriver::Lavapipe => {
            env.insert(
                "VK_ICD_FILENAMES".into(),
                "/usr/share/vulkan/icd.d/lvp_icd.aarch64.json".into(),
            );
            // lavapipe is a software rasteriser; without this it can pick a
            // thread count that thrashes a phone's little cores.
            env.insert("LP_NUM_THREADS".into(), "4".into());
        }
        VulkanDriver::Turnip => {
            env.insert(
                "VK_ICD_FILENAMES".into(),
                "/usr/share/vulkan/icd.d/freedreno_icd.aarch64.json".into(),
            );
            // Turnip on Android talks to the GPU through KGSL, not DRM.
            env.insert("TU_DEBUG".into(), "noconform".into());
        }
    }

    if guest.is_containerised() {
        // Hangover picks its x86-64 emulator from this; FEX is the default and
        // wowbox64 is the alternative the user can select.
        env.entry("HODLL".into())
            .or_insert_with(|| "libarm64ecfex.dll".into());
    }

    for (k, v) in &config.wine.env {
        env.insert(k.clone(), v.clone());
    }

    env
}

/// Wine log lines that carry no diagnostic value. Kept from upstream and
/// extended with the messages the Android stack produces.
const NOISE: &[&str] = &[
    "fixme:",
    "libEGL warning",
    "pci id for fd",
    "wine-staging",
    "experimental patches",
    "DxgiFactory::QueryInterface",
    "DxgiAdapter::QueryInterface",
    "create_factory_media",
    "EnableNonClientDpiScaling",
    "DwmSetWindowAttribute",
    "proot info:",
    "proot warning:",
    "MESA-INTEL: warning",
];

pub fn is_noise(line: &str) -> bool {
    NOISE.iter().any(|p| line.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &Path) -> TempestPaths {
        TempestPaths::with_root(dir.join("data"), dir.join("lib"))
    }

    fn android_guest(dir: &Path) -> GuestEnv {
        GuestEnv {
            rootfs: dir.join("data/runtime/rootfs"),
            proot: dir.join("lib/libproot.so"),
            loader: dir.join("lib/libproot-loader.so"),
            loader32: dir.join("lib/libproot-loader32.so"),
            containerised: true,
        }
    }

    #[test]
    fn desktop_commands_are_not_wrapped() {
        let dir = tempfile::tempdir().unwrap();
        let guest = GuestEnv {
            rootfs: PathBuf::new(),
            proot: PathBuf::new(),
            loader: PathBuf::new(),
            loader32: PathBuf::new(),
            containerised: false,
        };
        let spec = guest
            .command(
                &paths(dir.path()),
                "wine",
                "wine",
                &["Vortex.exe".to_string()],
                base_env(),
            )
            .unwrap();
        assert_eq!(spec.program, Path::new("wine"));
        assert_eq!(spec.args, vec!["Vortex.exe"]);
    }

    #[test]
    fn preflight_names_the_missing_piece() {
        let dir = tempfile::tempdir().unwrap();
        let guest = android_guest(dir.path());

        let err = guest.preflight().unwrap_err();
        assert!(err.to_string().contains("PRoot"), "{err}");

        std::fs::create_dir_all(dir.path().join("lib")).unwrap();
        std::fs::write(dir.path().join("lib/libproot.so"), b"x").unwrap();
        let err = guest.preflight().unwrap_err();
        assert!(err.to_string().contains("loader"), "{err}");

        std::fs::write(dir.path().join("lib/libproot-loader.so"), b"x").unwrap();
        let err = guest.preflight().unwrap_err();
        assert!(
            err.to_string().contains("guest filesystem"),
            "should now complain about the rootfs, got: {err}"
        );

        std::fs::create_dir_all(dir.path().join("data/runtime/rootfs/usr/bin")).unwrap();
        std::fs::write(dir.path().join("data/runtime/rootfs/usr/bin/env"), b"x").unwrap();
        guest.preflight().unwrap();
    }

    fn ready_guest(dir: &Path) -> GuestEnv {
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib/libproot.so"), b"x").unwrap();
        std::fs::write(dir.join("lib/libproot-loader.so"), b"x").unwrap();
        std::fs::write(dir.join("lib/libproot-loader32.so"), b"x").unwrap();
        std::fs::create_dir_all(dir.join("data/runtime/rootfs/usr/bin")).unwrap();
        std::fs::write(dir.join("data/runtime/rootfs/usr/bin/env"), b"x").unwrap();
        android_guest(dir)
    }

    #[test]
    fn android_commands_run_through_proot_with_the_loader_configured() {
        let dir = tempfile::tempdir().unwrap();
        let guest = ready_guest(dir.path());
        let p = paths(dir.path());

        let spec = guest
            .command(
                &p,
                "wine",
                "/usr/bin/wine",
                &["Vortex.exe".into()],
                base_env(),
            )
            .unwrap();

        assert_eq!(spec.program, dir.path().join("lib/libproot.so"));
        // The loader path is what lets PRoot start guest binaries without
        // exec()ing them, so it must always be set.
        assert_eq!(
            spec.env.get("PROOT_LOADER").map(String::as_str),
            Some(dir.path().join("lib/libproot-loader.so").to_str().unwrap())
        );
        let joined = spec.args.join(" ");
        assert!(joined.contains("-r "), "rootfs flag missing: {joined}");
        assert!(joined.contains("/usr/bin/wine"), "{joined}");
        assert!(joined.ends_with("Vortex.exe"), "{joined}");
    }

    #[test]
    fn the_wine_prefix_and_games_dir_are_bound_into_the_guest() {
        let dir = tempfile::tempdir().unwrap();
        let guest = ready_guest(dir.path());
        let p = paths(dir.path());
        let spec = guest
            .command(&p, "t", "/bin/true", &[], base_env())
            .unwrap();
        let joined = spec.args.join(" ");
        assert!(
            joined.contains(":/home/tempest/.wine"),
            "prefix not bound: {joined}"
        );
        assert!(joined.contains(":/games"), "games dir not bound: {joined}");
        assert!(
            joined.contains(":/vortex"),
            "vortex dir not bound: {joined}"
        );
        assert!(joined.contains("/proc:/proc"), "proc not bound: {joined}");
    }

    #[test]
    fn guest_environment_is_passed_via_env_i_not_inherited() {
        let dir = tempfile::tempdir().unwrap();
        let guest = ready_guest(dir.path());
        let p = paths(dir.path());
        let mut env = base_env();
        env.insert("WINEPREFIX".into(), guest.guest_prefix(&p));
        let spec = guest.command(&p, "t", "/bin/true", &[], env).unwrap();

        let i = spec.args.iter().position(|a| a == "/usr/bin/env").unwrap();
        assert_eq!(spec.args[i + 1], "-i", "guest env must start empty");
        assert!(spec
            .args
            .contains(&"WINEPREFIX=/home/tempest/.wine".to_string()));
    }

    #[test]
    fn shell_helper_passes_arguments_positionally_never_by_interpolation() {
        let dir = tempfile::tempdir().unwrap();
        let guest = ready_guest(dir.path());
        let p = paths(dir.path());
        let hostile = "; rm -rf /".to_string();
        let spec = guest
            .shell(
                &p,
                "t",
                "echo \"$1\"",
                std::slice::from_ref(&hostile),
                base_env(),
            )
            .unwrap();

        // The script and the argument must be separate argv entries: the
        // hostile value can only ever be read as `$1`.
        assert!(spec.args.contains(&"echo \"$1\"".to_string()));
        assert!(spec.args.contains(&hostile));
        assert!(
            !spec
                .args
                .iter()
                .any(|a| a.contains("echo") && a.contains("rm -rf")),
            "argument was interpolated into the script"
        );
    }

    #[test]
    fn wine_env_reflects_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let guest = ready_guest(dir.path());
        let mut cfg = Config::default();
        cfg.graphics.vulkan_driver = VulkanDriver::Lavapipe;
        cfg.graphics.dxvk_hud = Some("fps".into());
        cfg.launcher.use_fsync = true;

        let env = wine_env(&cfg, &guest, &paths(dir.path()));
        assert_eq!(
            env.get("WINEPREFIX").map(String::as_str),
            Some("/home/tempest/.wine")
        );
        assert_eq!(env.get("DXVK_HUD").map(String::as_str), Some("fps"));
        assert_eq!(env.get("WINEFSYNC").map(String::as_str), Some("1"));
        assert!(env.get("VK_ICD_FILENAMES").unwrap().contains("lvp"));
        assert_eq!(env.get("DISPLAY").map(String::as_str), Some(":0"));
    }

    #[test]
    fn user_env_overrides_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let guest = ready_guest(dir.path());
        let mut cfg = Config::default();
        cfg.wine.env.insert("WINEDEBUG".into(), "-all".into());
        assert_eq!(
            wine_env(&cfg, &guest, &paths(dir.path()))
                .get("WINEDEBUG")
                .map(String::as_str),
            Some("-all")
        );
    }

    #[test]
    fn the_prefix_path_differs_between_the_container_and_the_desktop() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());

        // In the container the prefix is the bind-mount target...
        assert_eq!(
            ready_guest(dir.path()).guest_prefix(&p),
            "/home/tempest/.wine"
        );

        // ...and on the desktop it is the real host path, because there is no
        // container to map it into.
        let desktop = GuestEnv {
            rootfs: PathBuf::new(),
            proot: PathBuf::new(),
            loader: PathBuf::new(),
            loader32: PathBuf::new(),
            containerised: false,
        };
        assert_eq!(
            desktop.guest_prefix(&p),
            p.wine_prefix().display().to_string()
        );
    }

    #[test]
    fn noise_filter_matches_upstream_patterns_and_the_new_ones() {
        assert!(is_noise("fixme:d3d:whatever"));
        assert!(is_noise("libEGL warning: DRI2"));
        assert!(is_noise("proot info: vpid 1: terminated"));
        assert!(is_noise(
            "proot warning: can't sanitize binding \"/data/...\""
        ));
        assert!(!is_noise(
            "err:module:import_dll Library d3d11.dll not found"
        ));
        assert!(!is_noise("wine: Unhandled page fault"));
    }
}
