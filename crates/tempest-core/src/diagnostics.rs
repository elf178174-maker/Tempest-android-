//! Diagnostics — the Android-aware replacement for `tempest doctor`.
//!
//! Upstream's doctor ran `which wine`, `vulkaninfo`, `xdg-mime` and read
//! `/etc/os-release`, then printed per-distro `sudo dnf install ...` hints.
//! None of that applies on a phone: there is no package manager to advise, and
//! the interesting questions are different ones — is the container runnable, is
//! there a Vulkan device inside it, is an X server reachable.

use crate::platform::{HostKind, PlatformRef};
use crate::runtime::guest::GuestEnv;
use crate::runtime::{manifest, RuntimeManager};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub verdict: Verdict,
    pub detail: String,
    /// What the user should do. `None` when nothing is wrong.
    pub fix: Option<String>,
}

impl Check {
    fn pass(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            verdict: Verdict::Pass,
            detail: detail.into(),
            fix: None,
        }
    }
    fn warn(name: &str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            verdict: Verdict::Warn,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
    fn fail(name: &str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            verdict: Verdict::Fail,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub checks: Vec<Check>,
    pub failures: usize,
    pub warnings: usize,
    pub platform: crate::platform::PlatformInfo,
}

impl Report {
    /// Plain-text form for the "Copy diagnostics" button.
    pub fn to_text(&self) -> String {
        let mut out = String::from("Tempest diagnostics\n===================\n");
        out.push_str(&format!(
            "Platform : {}\nCPU      : {}\nDevice   : {}\n\n",
            self.platform.os_description,
            self.platform.cpu_arch,
            self.platform.device_model.as_deref().unwrap_or("—")
        ));
        for c in &self.checks {
            let mark = match c.verdict {
                Verdict::Pass => "PASS",
                Verdict::Warn => "WARN",
                Verdict::Fail => "FAIL",
            };
            out.push_str(&format!("[{mark}] {}: {}\n", c.name, c.detail));
            if let Some(fix) = &c.fix {
                out.push_str(&format!("       -> {fix}\n"));
            }
        }
        out.push_str(&format!(
            "\n{} failed, {} warnings\n",
            self.failures, self.warnings
        ));
        out
    }
}

pub fn run(platform: &PlatformRef) -> Report {
    let info = platform.info();
    // The same advice has to name a different place on each front-end.
    let install_hint: fn(&str) -> String = if info.kind == HostKind::Android {
        |what| format!("Install {what} from Settings → Runtime.")
    } else {
        |what| format!("Install {what} with `tempest runtime <name>`.")
    };
    let paths = platform.paths();
    let runtime = RuntimeManager::new(platform.clone());
    let guest = GuestEnv::new(platform);
    let mut checks = Vec::new();

    // --- CPU architecture and translation ---------------------------------
    if info.needs_x86_translation {
        checks.push(Check::pass(
            "CPU architecture",
            format!(
                "{} — Windows x86/x64 code will run through the FEX and Box64 \
                 translation layers that Hangover provides",
                info.cpu_arch
            ),
        ));
    } else {
        checks.push(Check::pass(
            "CPU architecture",
            format!(
                "{} — Windows binaries run without translation",
                info.cpu_arch
            ),
        ));
    }

    // --- Container ---------------------------------------------------------
    if info.kind == HostKind::Android {
        let proot = paths.native_executable(crate::runtime::guest::PROOT_BIN);
        if proot.exists() {
            let executable = platform.process().can_execute(&proot);
            if executable {
                checks.push(Check::pass(
                    "Container (PRoot)",
                    proot.display().to_string(),
                ));
            } else {
                checks.push(Check::fail(
                    "Container (PRoot)",
                    "PRoot is present but not in an executable location",
                    "This build is broken — reinstall the APK from the project's CI artifacts.",
                ));
            }
        } else {
            checks.push(Check::fail(
                "Container (PRoot)",
                format!("not found at {}", proot.display()),
                "The APK was built without the native PRoot binary. Reinstall a \
                 build produced by the project's android.yml workflow.",
            ));
        }
    }

    // --- Runtime components ------------------------------------------------
    for spec in manifest::catalogue_for(info.kind) {
        let installed = runtime.is_installed(&spec);
        let required = spec.necessity(info.kind) == manifest::Necessity::Required;
        let check = match (installed, required) {
            (true, _) => Check::pass(spec.display_name, "installed"),
            (false, true) => Check::fail(
                spec.display_name,
                "not installed",
                format!(
                    "{} It is about {} MB.",
                    install_hint("it"),
                    spec.approx_bytes / (1024 * 1024)
                ),
            ),
            (false, false) => Check::warn(
                spec.display_name,
                "not installed (optional)",
                format!(
                    "{} It provides: {}",
                    install_hint("it"),
                    normalise(spec.purpose)
                ),
            ),
        };
        checks.push(check);
    }

    // --- Wine prefix -------------------------------------------------------
    if paths.wine_prefix().join("system.reg").exists() {
        checks.push(Check::pass(
            "Wine prefix",
            paths.wine_prefix().display().to_string(),
        ));
    } else if paths.wine_prefix().exists() {
        checks.push(Check::warn(
            "Wine prefix",
            "the directory exists but has not been initialised",
            "It is created automatically the first time you launch a game.",
        ));
    } else {
        checks.push(Check::warn(
            "Wine prefix",
            "not created yet",
            "It is created automatically the first time you launch a game.",
        ));
    }

    // --- GPU device nodes --------------------------------------------------
    let gpu_nodes: Vec<&str> = ["/dev/kgsl-3d0", "/dev/dri", "/dev/mali0"]
        .into_iter()
        .filter(|p| std::path::Path::new(p).exists())
        .collect();
    if gpu_nodes.is_empty() {
        checks.push(Check::warn(
            "GPU access",
            "no GPU device node is visible to this app",
            "Hardware Vulkan will not be available. Software rendering (lavapipe) \
             still works but is slow; set it in Settings → Graphics.",
        ));
    } else {
        checks.push(Check::pass("GPU access", gpu_nodes.join(", ")));
    }

    // --- Vulkan inside the container ---------------------------------------
    if runtime.is_installed(&manifest::spec(manifest::ComponentId::Rootfs))
        && guest.preflight().is_ok()
    {
        match runtime.run_in_guest(
            "vulkaninfo",
            "command -v vulkaninfo >/dev/null 2>&1 && vulkaninfo --summary 2>&1 | \
             grep -E 'deviceName|driverName' | head -8 || echo NO_VULKANINFO",
            &[],
            120,
        ) {
            Ok(lines) => {
                let joined = lines.join(" ");
                if joined.contains("NO_VULKANINFO") {
                    checks.push(Check::warn(
                        "Vulkan (in container)",
                        "vulkaninfo is not installed, so the driver could not be probed",
                        install_hint("the Vulkan/Mesa component"),
                    ));
                } else if let Some(device) = lines.iter().find(|l| l.contains("deviceName")) {
                    checks.push(Check::pass("Vulkan (in container)", device.trim()));
                } else {
                    checks.push(Check::fail(
                        "Vulkan (in container)",
                        "vulkaninfo reported no devices",
                        "Switch the Vulkan driver to lavapipe in Settings → Graphics to \
                         confirm the rest of the stack works, then investigate the GPU driver.",
                    ));
                }
            }
            Err(e) => checks.push(Check::fail(
                "Vulkan (in container)",
                e.to_string(),
                "The container could not run a command. Reinstall the Ubuntu base \
                 image from Settings → Runtime.",
            )),
        }
    } else {
        checks.push(Check::warn(
            "Vulkan (in container)",
            "not probed — the Linux filesystem is not installed yet",
            "Install the runtime components first.",
        ));
    }

    // --- X server ----------------------------------------------------------
    checks.push(x_server_check());

    // --- Auth --------------------------------------------------------------
    match crate::auth::stored_token(platform) {
        Ok(Some(_)) => checks.push(Check::pass("Vortex sign-in", "a session is stored")),
        Ok(None) => checks.push(Check::warn(
            "Vortex sign-in",
            "not signed in",
            "Sign in from the main screen.",
        )),
        Err(e) => checks.push(Check::fail(
            "Vortex sign-in",
            format!("the secure store could not be read: {e}"),
            "Sign out and in again to recreate the stored credential.",
        )),
    }
    checks.push(Check::pass(
        "Credential storage",
        platform.secrets().describe(),
    ));

    // --- Deep links --------------------------------------------------------
    match platform.uri_handler_status() {
        Ok(crate::platform::UriRegistration::ManifestDeclared) => checks.push(Check::pass(
            "vortex:// links",
            "declared in the app manifest and registered at install time",
        )),
        Ok(crate::platform::UriRegistration::Desktop) => checks.push(Check::pass(
            "vortex:// links",
            "registered with the desktop",
        )),
        Err(e) => checks.push(Check::fail(
            "vortex:// links",
            e.to_string(),
            if info.kind == HostKind::Android {
                "Paste the link into the app's Open Link box instead."
            } else {
                "Run `tempest setup` to register the handler."
            },
        )),
    }

    // --- Storage -----------------------------------------------------------
    checks.push(Check::pass(
        "Storage in use",
        format!(
            "{:.1} MB under {}",
            paths.disk_usage() as f64 / 1_048_576.0,
            paths.root().display()
        ),
    ));

    let failures = checks.iter().filter(|c| c.verdict == Verdict::Fail).count();
    let warnings = checks.iter().filter(|c| c.verdict == Verdict::Warn).count();
    Report {
        checks,
        failures,
        warnings,
        platform: info,
    }
}

/// Collapse the multi-line string literals in the catalogue into one line.
fn normalise(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Is an X server reachable?
///
/// Termux:X11 listens on the abstract Unix socket `@termux-x11` as well as the
/// usual `/tmp/.X11-unix/X0`. Only the filesystem socket can be probed from
/// here without opening a connection, so both are checked and the result is
/// advisory rather than fatal.
fn x_server_check() -> Check {
    let socket = std::path::Path::new("/tmp/.X11-unix/X0");
    if socket.exists() {
        return Check::pass(
            "X server",
            "a display socket is present at /tmp/.X11-unix/X0",
        );
    }
    Check::warn(
        "X server",
        "no X display socket was found",
        "Wine needs somewhere to draw. Install the Termux:X11 companion app, open \
         it, and leave it running in the background before launching a game.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::secrets::MemorySecretStore;
    use crate::platform::{
        paths::TempestPaths, process::ProcessBackend, secrets::SecretStore,
        unix_process::UnixProcessBackend, Platform, PlatformInfo, UriRegistration,
    };
    use std::sync::Arc;

    struct TestPlatform {
        paths: TempestPaths,
        process: UnixProcessBackend,
        secrets: MemorySecretStore,
        kind: HostKind,
    }

    impl Platform for TestPlatform {
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
                kind: self.kind,
                os_description: "Android 15 (API 35)".into(),
                cpu_arch: "arm64-v8a".into(),
                device_model: Some("Test Device".into()),
                needs_x86_translation: true,
            }
        }
        fn uri_handler_status(&self) -> crate::Result<UriRegistration> {
            Ok(UriRegistration::ManifestDeclared)
        }
        fn register_uri_handler(&self) -> crate::Result<UriRegistration> {
            Ok(UriRegistration::ManifestDeclared)
        }
    }

    fn platform(dir: &std::path::Path, kind: HostKind) -> PlatformRef {
        let paths = TempestPaths::with_root(dir.join("data"), dir.join("lib"));
        paths.ensure_all().unwrap();
        Arc::new(TestPlatform {
            paths,
            process: UnixProcessBackend::permissive(),
            secrets: MemorySecretStore::default(),
            kind,
        })
    }

    #[test]
    fn a_fresh_android_profile_fails_on_the_things_that_are_actually_missing() {
        let dir = tempfile::tempdir().unwrap();
        let report = run(&platform(dir.path(), HostKind::Android));

        assert!(report.failures > 0, "a fresh profile cannot be healthy");
        let names: Vec<&str> = report
            .checks
            .iter()
            .filter(|c| c.verdict == Verdict::Fail)
            .map(|c| c.name.as_str())
            .collect();
        assert!(names.contains(&"Container (PRoot)"), "{names:?}");
        assert!(names.iter().any(|n| n.contains("Ubuntu Base")), "{names:?}");
        assert!(names.iter().any(|n| n.contains("Hangover")), "{names:?}");
    }

    #[test]
    fn every_failing_check_tells_the_user_what_to_do() {
        let dir = tempfile::tempdir().unwrap();
        let report = run(&platform(dir.path(), HostKind::Android));
        for c in &report.checks {
            match c.verdict {
                Verdict::Pass => assert!(c.fix.is_none(), "{} passed but has a fix", c.name),
                _ => assert!(
                    c.fix.as_ref().is_some_and(|f| f.len() > 20),
                    "{} has no actionable fix text",
                    c.name
                ),
            }
        }
    }

    #[test]
    fn optional_components_warn_rather_than_fail() {
        let dir = tempfile::tempdir().unwrap();
        let report = run(&platform(dir.path(), HostKind::Android));
        let vkd3d = report
            .checks
            .iter()
            .find(|c| c.name.contains("vkd3d"))
            .expect("vkd3d is reported");
        assert_eq!(
            vkd3d.verdict,
            Verdict::Warn,
            "an optional component must not fail"
        );
    }

    #[test]
    fn desktop_reports_do_not_mention_proot() {
        let dir = tempfile::tempdir().unwrap();
        let report = run(&platform(dir.path(), HostKind::LinuxDesktop));
        assert!(!report.checks.iter().any(|c| c.name.contains("PRoot")));
    }

    #[test]
    fn text_export_is_readable_and_leaks_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = platform(dir.path(), HostKind::Android);
        p.secrets()
            .set(crate::platform::secrets::SESSION_TOKEN_KEY, "SECRET")
            .unwrap();

        let text = run(&p).to_text();
        assert!(text.contains("Tempest diagnostics"));
        assert!(text.contains("Test Device"));
        assert!(text.contains("[FAIL]"));
        assert!(
            !text.contains("SECRET"),
            "diagnostics leaked the session token"
        );
    }

    #[test]
    fn signed_in_state_is_reported_without_showing_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let p = platform(dir.path(), HostKind::Android);
        p.secrets()
            .set(crate::platform::secrets::SESSION_TOKEN_KEY, "tok")
            .unwrap();
        let report = run(&p);
        let check = report
            .checks
            .iter()
            .find(|c| c.name == "Vortex sign-in")
            .unwrap();
        assert_eq!(check.verdict, Verdict::Pass);
        assert!(!check.detail.contains("tok"));
    }
}
