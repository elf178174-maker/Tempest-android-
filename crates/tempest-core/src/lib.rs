//! # Tempest Core
//!
//! The platform-independent half of Tempest: Vortex authentication and game
//! discovery, `vortex://` link handling, runtime component management, Wine
//! prefix setup and game session lifecycle.
//!
//! Everything that differs between a Linux desktop and an Android phone lives
//! behind [`platform::Platform`]. Feature code never touches `$HOME`,
//! `dirs::config_dir()`, `pgrep`, `sudo`, `xdg-mime` or a package manager; it
//! asks the platform instead. That is what lets the same code drive both the
//! `tempest` CLI and the Android app.
//!
//! ```text
//!                       tempest-core
//!                            |
//!            +---------------+---------------+
//!            |                               |
//!     LinuxPlatform                   AndroidPlatform
//!     XDG paths                       Context-supplied paths
//!     unrestricted exec               exec only from nativeLibraryDir
//!     encrypted key file              Android Keystore
//!     .desktop registration           manifest intent filter
//! ```

pub mod api;
pub mod auth;
pub mod config;
pub mod crypto;
pub mod diagnostics;
pub mod error;
pub mod games;
pub mod logging;
pub mod net;
pub mod platform;
pub mod runtime;
pub mod session;
pub mod uri;

pub use error::{Result, TempestError};
pub use platform::{Platform, PlatformRef};

/// Version of the core, surfaced in diagnostics and the about screen.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
