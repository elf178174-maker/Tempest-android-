//! Getting Wine a window to draw in.
//!
//! Wine talks X11. Android has no X server, so one has to be supplied.
//!
//! The Termux:X11 project provides the only maintained ARM64 Android X server,
//! and it is easy to misread how it is built. `com.termux.x11` — the app the
//! user installs — is **not** the server. It is the viewer: a `SurfaceView`
//! that renders whatever the server hands it over a Binder. The server itself
//! is `com.termux.x11.CmdEntryPoint`, a Java entry point *inside* that APK,
//! designed to be started with `app_process` by whichever process wants a
//! display. It runs as that process's uid, in that process's mount namespace,
//! and creates its socket at `$TMPDIR/.X11-unix/X<n>`.
//!
//! That last detail is the whole reason this module exists. Earlier versions of
//! Tempest told the user to "start Termux:X11 and leave it running", then
//! looked for the socket. It never appeared, and could not have: a server
//! started from Termux puts its socket in Termux's private storage, and Android
//! gives no app access to another app's files. The fix is not better detection.
//! It is for Tempest to start the server itself, with `TMPDIR` pointing at the
//! guest rootfs's own `/tmp` — so the socket lands *inside the container*,
//! where Wine, running under PRoot, sees it at the ordinary `/tmp/.X11-unix/X0`.
//!
//! Pointing `TMPDIR` at the rootfs has a second benefit that the Termux:X11
//! code was written for on purpose: it derives the font and keymap paths from
//! `dirname($TMPDIR)`, so the server picks up the Ubuntu tree's own
//! `usr/share/X11/xkb` and fonts instead of needing a Termux installation.
//!
//! Tempest never modifies, repackages or redistributes Termux:X11. It executes
//! an already-installed app's public entry point, the same way its own
//! `termux-x11` script does. Termux:X11 is GPL-3.0; keeping it as a separate
//! user-installed app, invoked at arm's length, is what keeps that licence
//! boundary clean. See THIRD_PARTY_LICENSES.md.

use crate::platform::{DisplayProvider, ProcessSpec, TempestPaths};
use crate::{Result, TempestError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The Android runtime launcher. Always on the read-only system partition.
const APP_PROCESS: &str = "/system/bin/app_process";

/// Package that provides the X server. Only ever used as a fixed string.
pub const X11_PACKAGE: &str = "com.termux.x11";

/// Entry point inside that package's APK.
const X11_ENTRY_CLASS: &str = "com.termux.x11.CmdEntryPoint";

/// Environment `app_process` cannot start without, beyond the `ANDROID_*`
/// group. These name the boot class path and the ART/i18n/tzdata APEX roots;
/// the zygote puts them in every app process, so Tempest inherits them and
/// passes them on. Everything else is deliberately dropped: the process
/// backend clears the environment, and the X server has no business seeing
/// Tempest's own `LD_LIBRARY_PATH`, `PATH` or anything else.
const RUNTIME_ENV_KEYS: &[&str] = &["BOOTCLASSPATH", "DEX2OATBOOTCLASSPATH"];

/// Pick out the variables `app_process` needs from a process environment.
fn runtime_env<I, K, V>(vars: I) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<String>,
{
    vars.into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .filter(|(k, _)| k.starts_with("ANDROID_") || RUNTIME_ENV_KEYS.contains(&k.as_str()))
        .collect()
}

/// Everything needed to find, start and check one X display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayPlan {
    /// The `DISPLAY` value Wine will be given, e.g. `:0`.
    pub display: String,
    /// `$TMPDIR` for the server: the guest's `/tmp`, seen from the host.
    pub tmp_dir: PathBuf,
    /// The socket the server will create, as the *host* sees it.
    pub socket: PathBuf,
}

/// Parse the display number out of a `DISPLAY` value.
///
/// Accepts `:0`, `:0.0` and `0`. Anything with a host part (`host:0`) is a
/// remote display that Tempest neither starts nor probes.
pub fn display_number(display: &str) -> Option<u32> {
    let rest = match display.strip_prefix(':') {
        Some(r) => r,
        // A bare "0" is tolerated; "host:0" is not ours to manage.
        None if !display.contains(':') => display,
        None => return None,
    };
    let head = rest.split('.').next()?;
    if head.is_empty() {
        return None;
    }
    head.parse().ok()
}

/// Work out where the server's socket belongs for a given display.
///
/// `guest_rootfs` is `None` on the desktop, where `/tmp` is simply `/tmp`.
pub fn plan(display: &str, guest_rootfs: Option<&Path>) -> Option<DisplayPlan> {
    let n = display_number(display)?;
    let tmp_dir = match guest_rootfs {
        Some(root) => root.join("tmp"),
        None => PathBuf::from("/tmp"),
    };
    Some(DisplayPlan {
        display: display.to_string(),
        socket: tmp_dir.join(".X11-unix").join(format!("X{n}")),
        tmp_dir,
    })
}

impl DisplayPlan {
    pub fn for_paths(display: &str, paths: &TempestPaths, containerised: bool) -> Option<Self> {
        plan(
            display,
            containerised.then(|| paths.guest_rootfs()).as_deref(),
        )
    }

    /// Whether an X server is actually accepting connections on this socket.
    ///
    /// A bare `exists()` check is not enough: the socket file outlives a
    /// crashed server, and connecting is the only way to tell the difference.
    pub fn is_listening(&self) -> bool {
        std::os::unix::net::UnixStream::connect(&self.socket).is_ok()
    }

    /// Create `.X11-unix` so the server does not have to, and so a stale
    /// socket from a previous run cannot be mistaken for a live one.
    fn prepare(&self) -> Result<()> {
        let dir = self.socket.parent().expect("socket always has a parent");
        std::fs::create_dir_all(dir)
            .map_err(|e| TempestError::other(format!("could not create {}: {e}", dir.display())))?;
        if self.socket.exists() && !self.is_listening() {
            // Nothing is behind it; the server will refuse to bind over it.
            std::fs::remove_file(&self.socket).ok();
        }
        Ok(())
    }

    /// The command that starts the server.
    ///
    /// Note what is *not* here: no shell, no string interpolation, and no value
    /// that came from a URI, a game name or a server response. The only
    /// variable parts are paths Tempest itself owns and the display number,
    /// which has already been through [`display_number`].
    pub fn spawn_spec(&self, provider: &DisplayProvider) -> Result<ProcessSpec> {
        let apk = match provider {
            DisplayProvider::TermuxX11 { apk } => apk,
            DisplayProvider::HostNative => {
                return Err(TempestError::other(
                    "this host runs its own X server; Tempest does not start one",
                ))
            }
            DisplayProvider::Unavailable { reason } => {
                return Err(TempestError::missing("an X server", reason.clone()))
            }
        };

        let n = display_number(&self.display).ok_or_else(|| {
            TempestError::other(format!("{} is not a local display", self.display))
        })?;

        Ok(ProcessSpec::new("x11", APP_PROCESS)
            .args([
                // Skip AOT verification of the boot image: this is a one-class
                // launch and dex2oat here costs seconds for nothing.
                "-Xnoimage-dex2oat".to_string(),
                // app_process wants a parent directory for the VM; "/" is what
                // Termux:X11's own launcher passes.
                "/".to_string(),
                "--nice-name=tempest-x11".to_string(),
                X11_ENTRY_CLASS.to_string(),
                format!(":{n}"),
            ])
            .envs(runtime_env(std::env::vars()))
            .env("CLASSPATH", apk.display().to_string())
            .env("TMPDIR", self.tmp_dir.display().to_string())
            // The server resolves fonts and keymaps relative to dirname($TMPDIR),
            // which is the rootfs — but it only looks there if it is not told
            // otherwise, so leave XKB_CONFIG_ROOT unset.
            .working_dir(&self.tmp_dir))
    }
}

/// Start the server if nothing is listening, and wait for it to come up.
///
/// Returns the handle so the caller can stop the server when the session ends;
/// `None` means a server was already listening and is not ours to manage.
pub fn ensure_running(
    platform: &crate::platform::PlatformRef,
    plan: &DisplayPlan,
    timeout: Duration,
) -> Result<Option<Box<dyn crate::platform::ProcessHandle>>> {
    if plan.is_listening() {
        crate::logging::info(
            "display",
            format!("an X server is already listening on {}", plan.display),
        );
        return Ok(None);
    }

    plan.prepare()?;
    let spec = plan.spawn_spec(&platform.display_provider())?;
    crate::logging::info(
        "display",
        format!("starting X server: {}", spec.redacted_display()),
    );

    let mut handle = platform.process().spawn(spec)?;

    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if plan.is_listening() {
            crate::logging::info("display", format!("X server is up on {}", plan.display));
            return Ok(Some(handle));
        }
        if let Ok(status) = handle.poll() {
            if !status.is_running() {
                let output = handle.drain_output().join("\n");
                handle.terminate().ok();
                return Err(TempestError::other(format!(
                    "the X server exited immediately ({}).{}",
                    status.explain(),
                    if output.trim().is_empty() {
                        String::new()
                    } else {
                        format!(" It said: {}", output.trim())
                    }
                )));
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let output = handle.drain_output().join("\n");
    handle.terminate().ok();
    Err(TempestError::other(format!(
        "the X server did not start listening on {} within {} seconds.{}",
        plan.display,
        timeout.as_secs(),
        if output.trim().is_empty() {
            String::new()
        } else {
            format!(" It said: {}", output.trim())
        }
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_numbers_are_parsed_the_way_x_clients_parse_them() {
        assert_eq!(display_number(":0"), Some(0));
        assert_eq!(display_number(":1"), Some(1));
        assert_eq!(display_number(":0.0"), Some(0));
        assert_eq!(display_number("0"), Some(0));
        assert_eq!(display_number(":12"), Some(12));
        // A remote display is not ours to start or probe.
        assert_eq!(display_number("host:0"), None);
        assert_eq!(display_number("192.168.1.2:0"), None);
        assert_eq!(display_number(":"), None);
        assert_eq!(display_number(""), None);
        assert_eq!(display_number(":abc"), None);
    }

    #[test]
    fn the_socket_lands_inside_the_container_not_on_the_host() {
        let root = Path::new("/data/user/0/io.tempest/files/tempest/runtime/rootfs");
        let p = plan(":0", Some(root)).unwrap();
        assert_eq!(p.socket, root.join("tmp/.X11-unix/X0"));
        assert_eq!(p.tmp_dir, root.join("tmp"));

        // On the desktop there is no container, so it is the ordinary path.
        let d = plan(":1", None).unwrap();
        assert_eq!(d.socket, Path::new("/tmp/.X11-unix/X1"));
    }

    #[test]
    fn a_listening_socket_is_detected_and_a_stale_file_is_not() {
        let dir = std::env::temp_dir().join(format!("tempest-x11-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".X11-unix")).unwrap();
        let plan = DisplayPlan {
            display: ":0".into(),
            tmp_dir: dir.clone(),
            socket: dir.join(".X11-unix/X0"),
        };

        // Nothing there at all.
        assert!(!plan.is_listening());

        // A plain file sitting where the socket goes is not a live server.
        std::fs::write(&plan.socket, b"stale").unwrap();
        assert!(!plan.is_listening());

        // prepare() clears it so a real server can bind.
        plan.prepare().unwrap();
        assert!(!plan.socket.exists());

        // A real listener is detected.
        let listener = std::os::unix::net::UnixListener::bind(&plan.socket).unwrap();
        assert!(plan.is_listening());
        drop(listener);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_spawn_command_names_the_apk_and_never_builds_a_shell_string() {
        let apk = PathBuf::from("/data/app/~~abc/com.termux.x11-1/base.apk");
        let p = plan(":0", Some(Path::new("/rootfs"))).unwrap();
        let spec = p
            .spawn_spec(&DisplayProvider::TermuxX11 { apk: apk.clone() })
            .unwrap();
        let rendered = spec.redacted_display();
        assert!(rendered.contains("/system/bin/app_process"), "{rendered}");
        assert!(rendered.contains(X11_ENTRY_CLASS), "{rendered}");
        assert!(rendered.contains(":0"), "{rendered}");
        // argv, not a command line: no shell metacharacters anywhere.
        assert!(
            !rendered.contains("&&") && !rendered.contains(';'),
            "{rendered}"
        );
    }

    #[test]
    fn app_process_inherits_the_android_runtime_environment_and_nothing_else() {
        // Without BOOTCLASSPATH and the APEX roots, app_process aborts before
        // it reaches any Java code; with anything else it would be inheriting
        // Tempest's own linker settings, which are not its business.
        let env = runtime_env([
            ("ANDROID_ROOT", "/system"),
            ("ANDROID_DATA", "/data"),
            ("ANDROID_ART_ROOT", "/apex/com.android.art"),
            ("ANDROID_TZDATA_ROOT", "/apex/com.android.tzdata"),
            ("BOOTCLASSPATH", "/apex/com.android.art/javalib/core-oj.jar"),
            (
                "DEX2OATBOOTCLASSPATH",
                "/apex/com.android.art/javalib/core-oj.jar",
            ),
            ("LD_LIBRARY_PATH", "/data/app/io.tempest/lib/arm64"),
            ("PATH", "/usr/bin"),
            ("HOME", "/data/user/0/io.tempest"),
        ]);
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"ANDROID_ROOT"));
        assert!(keys.contains(&"ANDROID_ART_ROOT"));
        assert!(keys.contains(&"BOOTCLASSPATH"));
        assert!(keys.contains(&"DEX2OATBOOTCLASSPATH"));
        assert!(!keys.contains(&"LD_LIBRARY_PATH"), "{keys:?}");
        assert!(!keys.contains(&"PATH"), "{keys:?}");
        assert!(!keys.contains(&"HOME"), "{keys:?}");
    }

    #[test]
    fn an_absent_x11_app_produces_an_actionable_error_not_a_silent_failure() {
        let p = plan(":0", Some(Path::new("/rootfs"))).unwrap();
        let err = p
            .spawn_spec(&DisplayProvider::Unavailable {
                reason: "the Termux:X11 app is not installed".into(),
            })
            .unwrap_err();
        assert_eq!(err.kind(), "missing");
        assert!(err.to_string().contains("Termux:X11"), "{err}");
    }

    #[test]
    fn the_desktop_never_has_its_x_server_started_for_it() {
        let p = plan(":0", None).unwrap();
        assert!(p.spawn_spec(&DisplayProvider::HostNative).is_err());
    }
}
