//! Platform abstraction.
//!
//! The upstream (Linux desktop) Tempest reached for `dirs::config_dir()`,
//! `std::process::Command`, `pgrep`, `xdg-mime` and `/etc/os-release` directly
//! from feature code. On Android none of those exist or behave the same way, so
//! every one of those capabilities now sits behind a trait here:
//!
//! * [`TempestPaths`]  — where things live on disk
//! * [`ProcessBackend`] — how child processes are started and observed
//! * [`SecretStore`]   — where the session token is kept at rest
//! * [`Platform`]      — ties the three together plus platform metadata
//!
//! `linux.rs` implements the desktop behaviour (preserving what upstream did);
//! `android.rs` implements the Android behaviour. Feature code in `auth`,
//! `games`, `runtime`, `session` etc. only ever sees the traits.

pub mod paths;
pub mod process;
pub mod secrets;
pub mod unix_process;

#[cfg(all(unix, not(target_os = "android")))]
pub mod linux;

// Compiled on every Unix target, not just Android. The module holds no
// Android-only API calls — the device facts and paths are injected by the
// caller — so building it everywhere lets the desktop CI runner exercise the
// Android path handling, exec policy and secret plumbing under `cargo test`.
#[cfg(unix)]
pub mod android;

pub use paths::TempestPaths;
pub use process::{ProcessBackend, ProcessHandle, ProcessSpec, ProcessStatus};
pub use secrets::SecretStore;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Which host the core is running on. Used for diagnostics and for deciding
/// which runtime components are relevant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostKind {
    LinuxDesktop,
    Android,
}

impl HostKind {
    pub fn as_str(self) -> &'static str {
        match self {
            HostKind::LinuxDesktop => "linux",
            HostKind::Android => "android",
        }
    }
}

/// Static description of the host, surfaced in diagnostics and the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformInfo {
    pub kind: HostKind,
    /// e.g. "Fedora 41" or "Android 15 (API 35)".
    pub os_description: String,
    /// e.g. "aarch64", "x86_64".
    pub cpu_arch: String,
    /// Device model where the platform can report one.
    pub device_model: Option<String>,
    /// True when this platform can execute Windows binaries only through an
    /// instruction-set translation layer (i.e. ARM64 host, x86 guest).
    pub needs_x86_translation: bool,
}

/// How a `vortex://` deep link reaches the app on this platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UriRegistration {
    /// Registered by writing a freedesktop .desktop file (Linux).
    Desktop,
    /// Declared statically in AndroidManifest.xml; nothing to do at runtime.
    ManifestDeclared,
}

/// The capability bundle every platform must provide.
pub trait Platform: Send + Sync + 'static {
    fn paths(&self) -> &TempestPaths;
    fn process(&self) -> &dyn ProcessBackend;
    fn secrets(&self) -> &dyn SecretStore;
    fn info(&self) -> PlatformInfo;

    /// Make `vortex://` links reach this application.
    ///
    /// On Android this is a no-op that reports [`UriRegistration::ManifestDeclared`],
    /// because the intent filter is declared in the manifest at install time.
    fn register_uri_handler(&self) -> crate::Result<UriRegistration>;

    /// Report how `vortex://` links currently reach the app, **without
    /// changing anything**. Diagnostics must not have side effects: running
    /// `tempest doctor` should never silently re-register a handler.
    fn uri_handler_status(&self) -> crate::Result<UriRegistration>;

    /// Where Wine can find an X server on this host.
    ///
    /// Desktop Linux already has one; Android has none, and Tempest has to
    /// start one itself. See [`DisplayProvider`].
    fn display_provider(&self) -> DisplayProvider {
        DisplayProvider::HostNative
    }
}

/// How an X display is obtained on a given host.
///
/// Android has no X server, and the obvious candidate — the Termux:X11 app —
/// is not one either: `com.termux.x11` is only the *viewer*. The server is a
/// Java entry point inside that APK which is meant to be run by whichever
/// process wants the display, using `app_process`. That matters, because the
/// server creates its socket in `$TMPDIR` **as the process that started it**.
/// A server started by Termux puts its socket inside Termux's private storage,
/// which no other app can reach; Tempest has to start its own so the socket
/// lands inside Tempest's own container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayProvider {
    /// The host runs its own X server and Tempest should not interfere.
    HostNative,
    /// Tempest starts the server from the Termux:X11 APK at this path.
    TermuxX11 { apk: PathBuf },
    /// No display is obtainable; `reason` says why, for the user.
    Unavailable { reason: String },
}

impl DisplayProvider {
    /// Whether Tempest is responsible for starting the server.
    pub fn is_managed(&self) -> bool {
        matches!(self, Self::TermuxX11 { .. })
    }
}

/// Shared handle used throughout the core.
pub type PlatformRef = Arc<dyn Platform>;
