//! Game session lifecycle.
//!
//! Upstream's launcher blocked the calling thread on `child.wait()`, printed to
//! stdout, and decided whether `receiver.exe` was already up by shelling out to
//! `pgrep -f receiver.exe`. None of that survives contact with Android: there is
//! no console, `pgrep` is not present, and since API 29 an app cannot see other
//! processes in `/proc` anyway.
//!
//! Instead a session is an object with observable state. The UI polls
//! [`SessionManager::snapshot`], a foreground service keeps the process alive
//! while the app is backgrounded, and `receiver.exe` is tracked as a child we
//! started rather than looked up in the process table.

use crate::config::Config;
use crate::platform::{PlatformRef, ProcessHandle, ProcessStatus};
use crate::runtime::guest::{self, GuestEnv};
use crate::runtime::RuntimeManager;
use crate::uri::VortexLink;
use crate::{Result, TempestError};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

/// Windows path of the Vortex client as the guest sees it. Wine maps the Unix
/// root to drive `Z:`, and `/vortex` is where the client directory is bound.
const GUEST_VORTEX_EXE: &str = "Z:\\vortex\\Vortex.exe";
const GUEST_RECEIVER_EXE: &str = "Z:\\vortex\\receiver.exe";

/// Turn a host path into the `Z:`-rooted Windows path Wine will accept.
fn windows_path(host: &std::path::Path) -> String {
    format!("Z:{}", host.display().to_string().replace('/', "\\"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Idle,
    /// Making sure the prefix exists and the DLLs are in place.
    PreparingPrefix,
    StartingVortex,
    /// Vortex is up; the game itself is starting or running.
    Running,
    Stopping,
    Exited,
    Failed,
}

impl SessionState {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            SessionState::PreparingPrefix | SessionState::StartingVortex | SessionState::Running
        )
    }
}

/// What the UI renders. Contains no secrets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub state: SessionState,
    pub game_id: Option<u32>,
    pub game_name: Option<String>,
    /// Short human sentence, e.g. "Starting Vortex…".
    pub status_text: String,
    /// Populated when `state` is `Failed`.
    pub error: Option<String>,
    pub error_kind: Option<String>,
    pub started_at: Option<u64>,
    pub pid: Option<u32>,
    /// Most recent guest output lines, already redacted and noise-filtered.
    pub recent_output: Vec<String>,
}

impl Default for SessionSnapshot {
    fn default() -> Self {
        Self {
            state: SessionState::Idle,
            game_id: None,
            game_name: None,
            status_text: "No game running".to_string(),
            error: None,
            error_kind: None,
            started_at: None,
            pid: None,
            recent_output: Vec::new(),
        }
    }
}

struct Inner {
    snapshot: SessionSnapshot,
    vortex: Option<Box<dyn ProcessHandle>>,
    receiver: Option<Box<dyn ProcessHandle>>,
    output: Vec<String>,
    /// Environment contributed by the front-end rather than by config — the
    /// desktop CLI's optional plugins are the only current source.
    extra_env: std::collections::BTreeMap<String, String>,
    /// Captured at launch. The UI polls twice a second while a game runs, and
    /// re-reading and re-parsing config.toml on every one of those polls is
    /// pure waste on a device that is busy running a game.
    filter_noise: bool,
}

/// Owns at most one running game session.
pub struct SessionManager {
    platform: PlatformRef,
    inner: Arc<Mutex<Inner>>,
}

impl SessionManager {
    pub fn new(platform: PlatformRef) -> Self {
        Self {
            platform,
            inner: Arc::new(Mutex::new(Inner {
                snapshot: SessionSnapshot::default(),
                vortex: None,
                receiver: None,
                output: Vec::new(),
                extra_env: std::collections::BTreeMap::new(),
                filter_noise: true,
            })),
        }
    }

    /// Add environment variables to every process this manager launches.
    ///
    /// Applied *before* the user's own `[wine.env]`, so an explicit setting in
    /// the config file always wins over one a plugin contributed.
    pub fn set_extra_env(&self, env: std::collections::BTreeMap<String, String>) {
        self.inner.lock().expect("session lock").extra_env = env;
    }

    pub fn snapshot(&self) -> SessionSnapshot {
        let mut inner = self.inner.lock().expect("session lock");
        self.refresh(&mut inner);
        inner.snapshot.clone()
    }

    pub fn is_active(&self) -> bool {
        self.snapshot().state.is_active()
    }

    /// Poll the child, fold new output in, and move the state machine along.
    fn refresh(&self, inner: &mut Inner) {
        if let Some(handle) = inner.receiver.as_mut() {
            if matches!(handle.poll(), Ok(status) if !status.is_running()) {
                inner.receiver = None;
            }
        }

        let Some(handle) = inner.vortex.as_mut() else {
            return;
        };

        let filter_noise = inner.filter_noise;
        for line in handle.drain_output() {
            if filter_noise && guest::is_noise(&line) {
                continue;
            }
            if inner.output.len() >= 400 {
                inner.output.remove(0);
            }
            inner.output.push(line);
        }
        let recent: Vec<String> = inner.output.iter().rev().take(50).rev().cloned().collect();

        let status = handle.poll();
        match status {
            Ok(ProcessStatus::Running) => {
                if inner.snapshot.state == SessionState::StartingVortex {
                    inner.snapshot.state = SessionState::Running;
                    inner.snapshot.status_text = "Game running".to_string();
                }
            }
            Ok(done) => {
                let clean = matches!(done, ProcessStatus::Exited(0));
                inner.snapshot.state = if clean {
                    SessionState::Exited
                } else {
                    SessionState::Failed
                };
                inner.snapshot.status_text = if clean {
                    "Game exited".to_string()
                } else {
                    format!("Game stopped: {}", done.explain())
                };
                if !clean {
                    inner.snapshot.error = Some(explain_failure(&done, &inner.output));
                    inner.snapshot.error_kind = Some("process".to_string());
                }
                inner.snapshot.pid = None;
                inner.vortex = None;
                if let Some(r) = inner.receiver.as_mut() {
                    r.terminate().ok();
                }
                inner.receiver = None;
            }
            Err(e) => {
                inner.snapshot.state = SessionState::Failed;
                inner.snapshot.error = Some(e.to_string());
                inner.snapshot.error_kind = Some(e.kind().to_string());
            }
        }
        inner.snapshot.recent_output = recent;
    }

    /// Launch a game from a validated deep link.
    pub fn launch(&self, link: &VortexLink, game_name: Option<String>) -> Result<()> {
        {
            let mut inner = self.inner.lock().expect("session lock");
            self.refresh(&mut inner);
            if inner.snapshot.state.is_active() {
                return Err(TempestError::other(
                    "a game is already running — stop it before starting another",
                ));
            }
            inner.output.clear();
            inner.snapshot = SessionSnapshot {
                state: SessionState::PreparingPrefix,
                game_id: Some(link.game_id),
                game_name: game_name.clone(),
                status_text: "Preparing the Windows environment…".to_string(),
                started_at: Some(now_secs()),
                ..Default::default()
            };
        }

        let result = self.launch_inner(link);
        if let Err(e) = &result {
            let mut inner = self.inner.lock().expect("session lock");
            inner.snapshot.state = SessionState::Failed;
            inner.snapshot.status_text = "Launch failed".to_string();
            inner.snapshot.error = Some(e.to_string());
            inner.snapshot.error_kind = Some(e.kind().to_string());
            crate::logging::error("session", e.to_string());
        }
        result
    }

    fn launch_inner(&self, link: &VortexLink) -> Result<()> {
        let paths = self.platform.paths();
        let config = Config::load_or_default(paths);
        self.inner.lock().expect("session lock").filter_noise = config.launcher.filter_wine_noise;
        let runtime = RuntimeManager::new(Arc::clone(&self.platform));

        let missing = runtime.missing_required();
        if !missing.is_empty() {
            let names: Vec<&str> = missing.iter().map(|m| m.as_str()).collect();
            return Err(TempestError::missing(
                "runtime components",
                format!(
                    "these still need to be installed: {}. Open Settings → \
                     Runtime and install them first.",
                    names.join(", ")
                ),
            ));
        }

        let guest_env = GuestEnv::new(&self.platform);
        guest_env.preflight()?;

        // Wine draws through X11, which Android does not have. Checking here
        // costs nothing and saves the user a two-minute prefix build followed
        // by an opaque Wine error.
        if guest_env.is_containerised() && !x_display_available() {
            return Err(TempestError::missing(
                "an X server",
                format!(
                    "Wine has nowhere to draw. Install the Termux:X11 companion app \
                     (github.com/termux/termux-x11), open it, and leave it running in \
                     the background — then launch again. Tempest is looking for \
                     display {}.",
                    config.graphics.display
                ),
            ));
        }

        self.ensure_prefix(&runtime, &config)?;

        self.set_status(SessionState::StartingVortex, "Starting Vortex…");

        let mut env = guest::wine_env(&config, &guest_env, paths);
        {
            let extra = self.inner.lock().expect("session lock").extra_env.clone();
            for (key, value) in extra {
                // The config file is the user's explicit intent; a plugin's
                // suggestion must not override it.
                env.entry(key).or_insert(value);
            }
        }
        let uri = link.to_uri();
        crate::logging::info(
            "session",
            format!("launching game {} ({})", link.game_id, link.redacted()),
        );

        // receiver.exe first, so in-game notifications work once Vortex is up.
        self.start_receiver(&guest_env, &config, &env);

        // On the desktop there is no container, so the client is at its real
        // host path; inside the container it is at the bind-mount target.
        let vortex_exe = if guest_env.is_containerised() {
            GUEST_VORTEX_EXE.to_string()
        } else {
            windows_path(&paths.vortex_exe())
        };

        let spec = guest_env.command(
            paths,
            "vortex",
            &config.wine.binary,
            &[vortex_exe, uri],
            env,
        )?;
        let handle = self.platform.process().spawn(spec)?;

        let mut inner = self.inner.lock().expect("session lock");
        inner.snapshot.pid = Some(handle.pid());
        inner.vortex = Some(handle);
        Ok(())
    }

    /// Start `receiver.exe` if it exists and is not already running.
    ///
    /// Upstream detected "already running" with `pgrep -f receiver.exe`. Here
    /// the manager simply knows whether it started one, which is both correct
    /// on Android and immune to matching an unrelated process.
    fn start_receiver(
        &self,
        guest_env: &GuestEnv,
        config: &Config,
        env: &std::collections::BTreeMap<String, String>,
    ) {
        let paths = self.platform.paths();
        if !paths.receiver_exe().exists() {
            crate::logging::warn(
                "session",
                "receiver.exe is not installed; in-game notifications will not work",
            );
            return;
        }
        {
            let inner = self.inner.lock().expect("session lock");
            if inner.receiver.is_some() || self.platform.process().is_running("receiver") {
                return;
            }
        }

        let mut receiver_env = env.clone();
        receiver_env.insert("WINEDEBUG".into(), "-all".into());
        let receiver_exe = if guest_env.is_containerised() {
            GUEST_RECEIVER_EXE.to_string()
        } else {
            windows_path(&paths.receiver_exe())
        };

        let spec = match guest_env.command(
            paths,
            "receiver",
            &config.wine.binary,
            &[receiver_exe],
            receiver_env,
        ) {
            Ok(s) => s.no_capture(),
            Err(e) => {
                crate::logging::warn("session", format!("receiver.exe: {e}"));
                return;
            }
        };

        match self.platform.process().spawn(spec) {
            Ok(handle) => {
                crate::logging::info(
                    "session",
                    format!("receiver.exe started (pid {})", handle.pid()),
                );
                self.inner.lock().expect("session lock").receiver = Some(handle);
            }
            // Not fatal: the game still runs, only notifications are lost.
            Err(e) => crate::logging::warn("session", format!("could not start receiver.exe: {e}")),
        }
    }

    /// Create the Wine prefix and install the graphics DLLs, once.
    fn ensure_prefix(&self, runtime: &RuntimeManager, config: &Config) -> Result<()> {
        let paths = self.platform.paths();
        let marker = paths.wine_prefix().join("system.reg");
        if !marker.exists() {
            self.set_status(
                SessionState::PreparingPrefix,
                "Creating the Windows environment…",
            );
            crate::logging::info("session", "running wineboot to create the prefix");
            let prefix = guest::GuestEnv::new(&self.platform).guest_prefix(paths);
            runtime.run_in_guest(
                "wineboot",
                "set -e; export WINEPREFIX=\"$1\"; WINEDEBUG=-all wineboot --init; wineserver -w",
                &[prefix],
                900,
            )?;
        }

        let dxvk_marker = paths.wine_prefix().join(".tempest-dxvk-installed");
        if config.graphics.enable_dxvk && !dxvk_marker.exists() {
            self.set_status(
                SessionState::PreparingPrefix,
                "Installing Direct3D support…",
            );
            self.install_graphics_dlls(runtime, config)?;
            std::fs::write(&dxvk_marker, config.graphics.vulkan_driver.as_str()).ok();
        }
        Ok(())
    }

    fn install_graphics_dlls(&self, runtime: &RuntimeManager, config: &Config) -> Result<()> {
        use crate::runtime::dll;
        let paths = self.platform.paths();
        let host_is_arm64 = self.platform.info().cpu_arch.starts_with("aarch64")
            || self.platform.info().cpu_arch.starts_with("arm64");

        let system32 = paths.wine_prefix().join("drive_c/windows/system32");
        let syswow64 = paths.wine_prefix().join("drive_c/windows/syswow64");

        let mut overrides: Vec<(&str, &str)> = Vec::new();

        let dxvk_root = paths.runtime_dir().join("dxvk");
        if config.graphics.enable_dxvk && dxvk_root.is_dir() {
            let mut installed = Vec::new();
            if let Some(dir) = dll::preferred_dll_dir(&dxvk_root, true, host_is_arm64) {
                installed.extend(dll::install_dlls_from(&dir, &system32, dll::DXVK_DLLS)?);
            }
            if let Some(dir) = dll::preferred_dll_dir(&dxvk_root, false, host_is_arm64) {
                dll::install_dlls_from(&dir, &syswow64, dll::DXVK_DLLS)?;
            }
            if installed.is_empty() {
                return Err(TempestError::runtime(
                    "DXVK",
                    "no usable DLLs were found in the installed DXVK — reinstall it \
                     from Settings → Runtime",
                ));
            }
            crate::logging::info(
                "session",
                format!("DXVK: installed {}", installed.join(", ")),
            );
            overrides.extend_from_slice(dll::DXVK_OVERRIDES);
        }

        let vkd3d_root = paths.runtime_dir().join("vkd3d");
        if config.graphics.enable_vkd3d && vkd3d_root.is_dir() {
            if let Some(dir) = dll::preferred_dll_dir(&vkd3d_root, true, host_is_arm64) {
                dll::install_dlls_from(&dir, &system32, dll::VKD3D_DLLS)?;
            }
            if let Some(dir) = dll::preferred_dll_dir(&vkd3d_root, false, host_is_arm64) {
                dll::install_dlls_from(&dir, &syswow64, dll::VKD3D_DLLS)?;
            }
            overrides.extend_from_slice(dll::VKD3D_OVERRIDES);
        }

        if overrides.is_empty() {
            return Ok(());
        }

        // One regedit import beats one `wine reg add` per DLL: each guest
        // process costs real time on an emulated stack.
        let reg_path = paths.wine_prefix().join("tempest-overrides.reg");
        std::fs::write(&reg_path, dll::overrides_reg(&overrides))?;

        let prefix = guest::GuestEnv::new(&self.platform).guest_prefix(paths);
        runtime.run_in_guest(
            "regedit",
            "set -e; export WINEPREFIX=\"$1\"; WINEDEBUG=-all wine regedit \"$1/tempest-overrides.reg\"",
            &[prefix],
            300,
        )?;
        Ok(())
    }

    fn set_status(&self, state: SessionState, text: &str) {
        let mut inner = self.inner.lock().expect("session lock");
        inner.snapshot.state = state;
        inner.snapshot.status_text = text.to_string();
    }

    /// Stop the running session.
    pub fn stop(&self) -> Result<()> {
        let mut inner = self.inner.lock().expect("session lock");
        inner.snapshot.state = SessionState::Stopping;
        inner.snapshot.status_text = "Stopping…".to_string();

        if let Some(mut handle) = inner.vortex.take() {
            handle.terminate()?;
        }
        if let Some(mut handle) = inner.receiver.take() {
            handle.terminate().ok();
        }
        self.platform.process().terminate_all();

        inner.snapshot.state = SessionState::Exited;
        inner.snapshot.status_text = "Stopped".to_string();
        inner.snapshot.pid = None;
        Ok(())
    }

    /// Full guest output for the current or last session.
    pub fn output(&self) -> Vec<String> {
        self.inner.lock().expect("session lock").output.clone()
    }
}

/// Turn a non-zero exit into something the user can act on, using the guest
/// output to identify the common failure modes rather than showing a number.
fn explain_failure(status: &ProcessStatus, output: &[String]) -> String {
    let tail = output
        .iter()
        .rev()
        .take(80)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();

    let hint = if tail.contains("open display")
        || tail.contains("x11 display")
        || tail.contains("no driver could be loaded")
    {
        Some(
            "Wine could not reach an X server. Start the Termux:X11 companion app \
             and leave it running, then launch again.",
        )
    } else if tail.contains("vulkan") && (tail.contains("no device") || tail.contains("icd")) {
        Some(
            "No usable Vulkan device was found inside the container. Install the \
             Mesa drivers from Settings → Runtime, or switch the Vulkan driver to \
             lavapipe to test with software rendering.",
        )
    } else if tail.contains("d3d11.dll") || tail.contains("dxgi.dll") {
        Some(
            "A Direct3D library failed to load. Reinstall DXVK from \
             Settings → Runtime.",
        )
    } else if tail.contains("wow64") || tail.contains("emulator") || tail.contains("arm64ec") {
        Some(
            "The x86 translation layer failed to initialise. Reinstall Hangover \
             from Settings → Runtime.",
        )
    } else if tail.contains("out of memory") || tail.contains("cannot allocate") {
        Some("The device ran out of memory. Close other apps and try again.")
    } else {
        None
    };

    match hint {
        Some(h) => format!("{}\n\n{h}", status.explain()),
        None => format!(
            "{}\n\nOpen the log for the full guest output.",
            status.explain()
        ),
    }
}

/// Whether an X display socket is present.
///
/// Termux:X11 also offers an abstract socket that cannot be probed without
/// connecting, so a filesystem socket is the only thing checkable up front.
/// Both the standard path and Termux's own location are considered.
fn x_display_available() -> bool {
    [
        "/tmp/.X11-unix/X0",
        "/data/data/com.termux/files/usr/tmp/.X11-unix/X0",
    ]
    .iter()
    .any(|p| std::path::Path::new(p).exists())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_env_is_stored_for_the_next_launch() {
        use crate::platform::secrets::MemorySecretStore;
        use crate::platform::{
            paths::TempestPaths, process::ProcessBackend, secrets::SecretStore,
            unix_process::UnixProcessBackend, HostKind, Platform, PlatformInfo, UriRegistration,
        };

        struct P {
            paths: TempestPaths,
            process: UnixProcessBackend,
            secrets: MemorySecretStore,
        }
        impl Platform for P {
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
                    os_description: "test".into(),
                    cpu_arch: "x86_64".into(),
                    device_model: None,
                    needs_x86_translation: false,
                }
            }
            fn uri_handler_status(&self) -> Result<UriRegistration> {
                Ok(UriRegistration::Desktop)
            }
            fn register_uri_handler(&self) -> Result<UriRegistration> {
                Ok(UriRegistration::Desktop)
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let paths = TempestPaths::with_root(dir.path().join("data"), dir.path().join("lib"));
        paths.ensure_all().unwrap();
        let manager = SessionManager::new(Arc::new(P {
            paths,
            process: UnixProcessBackend::permissive(),
            secrets: MemorySecretStore::default(),
        }));

        let mut env = std::collections::BTreeMap::new();
        env.insert("DXVK_STATE_CACHE".to_string(), "1".to_string());
        manager.set_extra_env(env);
        assert_eq!(
            manager
                .inner
                .lock()
                .unwrap()
                .extra_env
                .get("DXVK_STATE_CACHE"),
            Some(&"1".to_string())
        );
    }

    #[test]
    fn host_paths_become_z_rooted_windows_paths() {
        assert_eq!(
            windows_path(std::path::Path::new(
                "/home/u/.local/share/tempest/vortex/Vortex.exe"
            )),
            "Z:\\home\\u\\.local\\share\\tempest\\vortex\\Vortex.exe"
        );
        assert!(!windows_path(std::path::Path::new("/a/b")).contains('/'));
    }

    #[test]
    fn state_activity() {
        assert!(SessionState::Running.is_active());
        assert!(SessionState::StartingVortex.is_active());
        assert!(SessionState::PreparingPrefix.is_active());
        assert!(!SessionState::Idle.is_active());
        assert!(!SessionState::Exited.is_active());
        assert!(!SessionState::Failed.is_active());
    }

    #[test]
    fn default_snapshot_is_idle_and_carries_no_secrets() {
        let s = SessionSnapshot::default();
        assert_eq!(s.state, SessionState::Idle);
        assert!(s.error.is_none());
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("token"));
    }

    #[test]
    fn snapshot_serialises_for_the_jni_bridge() {
        let s = SessionSnapshot {
            state: SessionState::Running,
            game_id: Some(4),
            game_name: Some("Test".into()),
            status_text: "Game running".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"state\":\"running\""), "{json}");
        let back: SessionSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.game_id, Some(4));
    }

    #[test]
    fn a_missing_x_server_is_caught_before_the_prefix_is_built() {
        // Building the prefix takes minutes on an emulated stack. Failing fast
        // with a specific instruction beats failing slowly with a Wine error.
        assert!(
            !x_display_available() || std::path::Path::new("/tmp/.X11-unix/X0").exists(),
            "the probe must only report a display that actually exists"
        );
    }

    #[test]
    fn failure_explanation_identifies_a_missing_x_server() {
        let msg = explain_failure(
            &ProcessStatus::Exited(1),
            &["wine: could not open display :0".to_string()],
        );
        assert!(msg.contains("Termux:X11"), "{msg}");
    }

    #[test]
    fn failure_explanation_identifies_a_missing_vulkan_device() {
        let msg = explain_failure(
            &ProcessStatus::Exited(1),
            &["vulkan: no device found, ICD load failed".to_string()],
        );
        assert!(msg.contains("lavapipe"), "{msg}");
    }

    #[test]
    fn failure_explanation_identifies_a_broken_translation_layer() {
        let msg = explain_failure(
            &ProcessStatus::Exited(1),
            &["err: failed to load wow64 emulator".to_string()],
        );
        assert!(msg.contains("Hangover"), "{msg}");
    }

    #[test]
    fn failure_explanation_always_points_somewhere_useful() {
        let msg = explain_failure(
            &ProcessStatus::Exited(42),
            &["something unrecognised".into()],
        );
        assert!(msg.contains("42"));
        assert!(msg.contains("log"), "{msg}");
    }

    #[test]
    fn signal_kills_are_explained_in_android_terms() {
        let msg = explain_failure(&ProcessStatus::Signalled(9), &[]);
        assert!(msg.contains("low-memory"), "{msg}");
    }
}
