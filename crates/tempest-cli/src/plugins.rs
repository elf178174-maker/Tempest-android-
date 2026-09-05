//! Optional performance plugins, carried over from upstream.
//!
//! These live in the CLI rather than in `tempest-core`, and deliberately so:
//! both are C programs that upstream compiles **on the host** with `cc` at
//! install time. There is no C compiler on a phone, and neither plugin would
//! do anything useful there —
//!
//! * `fps-unlocker` is a Vulkan layer registered through
//!   `VK_ADD_IMPLICIT_LAYER_PATH`, which the desktop Vulkan loader honours;
//!   inside the Android container Wine talks to whichever ICD is installed and
//!   there is no loader to insert a layer into.
//! * `vortex-optim` sets Mesa tunables (`mesa_glthread`, `MESA_NO_DITHER`) that
//!   apply to a desktop Mesa driver.
//!
//! So they remain available on Linux, exactly as before, and are simply not
//! offered on Android. The DXVK tunables the second one sets are reachable on
//! Android through `[wine.env]` in the config file, which is documented in
//! `docs/TROUBLESHOOTING.md`.

use colored::Colorize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempest_core::platform::PlatformRef;

const FPS_UNLOCKER_C: &[u8] = include_bytes!("../../../plugins/fps-unlocker/present_mode_layer.c");
const FPS_UNLOCKER_JSON: &[u8] =
    include_bytes!("../../../plugins/fps-unlocker/VkLayer_vortstrap_present_mode.json");
const OPTIMIZER_C: &[u8] = include_bytes!("../../../plugins/vortex-optim/optimizer.c");

pub const AVAILABLE: &[&str] = &["fps-unlocker", "vortex-optim"];

fn plugins_dir(platform: &PlatformRef) -> PathBuf {
    platform.paths().root().join("plugins")
}

fn plugin_dir(platform: &PlatformRef, name: &str) -> PathBuf {
    plugins_dir(platform).join(sanitize(name))
}

/// Plugin names come from the command line, so they are constrained to the
/// known set before ever being joined onto a path.
fn sanitize(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

pub fn installed_names(platform: &PlatformRef) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(plugins_dir(platform))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    names.sort();
    names
}

/// Whether a plugin's artefacts are actually present, not merely its directory.
pub fn is_complete(platform: &PlatformRef, name: &str) -> bool {
    let dir = plugin_dir(platform, name);
    match name {
        "fps-unlocker" => {
            dir.join("libVkLayer_vortstrap_present_mode.so").exists()
                && dir.join("VkLayer_vortstrap_present_mode.json").exists()
        }
        "vortex-optim" => dir.join("vortex-optim").exists(),
        _ => false,
    }
}

pub fn binary_path(platform: &PlatformRef, name: &str) -> Option<PathBuf> {
    if !is_complete(platform, name) {
        return None;
    }
    let path = plugin_dir(platform, name).join(sanitize(name));
    path.is_file().then_some(path)
}

/// Environment variables contributed by the installed plugins.
pub fn env_vars(platform: &PlatformRef) -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    for name in installed_names(platform) {
        if !is_complete(platform, &name) {
            continue;
        }
        let dir = plugin_dir(platform, &name);
        match name.as_str() {
            "fps-unlocker" => {
                vars.insert(
                    "VK_ADD_IMPLICIT_LAYER_PATH".into(),
                    dir.to_string_lossy().into_owned(),
                );
                vars.insert("VORTSTRAP_FORCE_PRESENT".into(), "1".into());
                vars.insert(
                    "VORTSTRAP_PRESENT_MODE".into(),
                    std::env::var("VORTSTRAP_PRESENT_MODE").unwrap_or_else(|_| "0".into()),
                );
            }
            "vortex-optim" => {
                vars.insert("DXVK_STATE_CACHE".into(), "1".into());
                vars.insert("mesa_glthread".into(), "true".into());
                vars.insert("MESA_NO_DITHER".into(), "1".into());
                let existing = std::env::var("DXVK_CONFIG").unwrap_or_default();
                let value = if existing.contains("dxvk.enableAsync") {
                    existing
                } else if existing.is_empty() {
                    "dxvk.enableAsync=true,dxvk.numCompilerThreads=2".to_string()
                } else {
                    format!("{existing},dxvk.enableAsync=true,dxvk.numCompilerThreads=2")
                };
                vars.insert("DXVK_CONFIG".into(), value);
            }
            _ => {}
        }
    }
    vars
}

pub fn run(platform: &PlatformRef, args: &[String]) -> tempest_core::Result<()> {
    match args {
        [] => {
            list(platform);
            Ok(())
        }
        [verb, name] if verb == "uninstall" => uninstall(platform, name),
        [name] => install(platform, name),
        _ => Err(tempest_core::TempestError::other(
            "usage: tempest plugin [<name>] | tempest plugin uninstall <name>",
        )),
    }
}

fn list(platform: &PlatformRef) {
    let installed = installed_names(platform);
    if installed.is_empty() {
        println!("No plugins installed.");
    } else {
        for name in &installed {
            let mark = if is_complete(platform, name) {
                "[installed] ".green()
            } else {
                "[incomplete]".yellow()
            };
            println!("  {mark} {name}");
        }
    }
    println!("  Available: {}", AVAILABLE.join(", "));
    println!("  Run {} to install one.", "tempest plugin <name>".cyan());
}

fn install(platform: &PlatformRef, name: &str) -> tempest_core::Result<()> {
    if !AVAILABLE.contains(&name) {
        return Err(tempest_core::TempestError::other(format!(
            "unknown plugin '{name}'; available: {}",
            AVAILABLE.join(", ")
        )));
    }

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let dir = plugin_dir(platform, name);
    std::fs::create_dir_all(&dir)?;

    let tmp = platform.paths().tmp_dir().join(format!("plugin-{name}"));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).ok();
    }
    std::fs::create_dir_all(&tmp)?;

    let result = match name {
        "fps-unlocker" => build_fps_unlocker(&cc, &tmp, &dir),
        "vortex-optim" => build_optimizer(&cc, &tmp, &dir),
        _ => unreachable!("name was checked against AVAILABLE"),
    };
    std::fs::remove_dir_all(&tmp).ok();

    result?;
    println!("{} installed {name}", "[DONE]".green());
    Ok(())
}

fn build_fps_unlocker(cc: &str, tmp: &Path, dest: &Path) -> tempest_core::Result<()> {
    let source = tmp.join("present_mode_layer.c");
    let object = tmp.join("libVkLayer_vortstrap_present_mode.so");
    std::fs::write(&source, FPS_UNLOCKER_C)?;

    compile(
        cc,
        &[
            "-I/usr/include",
            "-shared",
            "-fPIC",
            "-O2",
            "-fvisibility=hidden",
            "-Wall",
            "-Wextra",
        ],
        &source,
        &object,
        "the Vulkan headers (vulkan/vk_layer.h) are required; install your \
         distribution's vulkan-headers package",
    )?;

    std::fs::copy(&object, dest.join("libVkLayer_vortstrap_present_mode.so"))?;
    std::fs::write(
        dest.join("VkLayer_vortstrap_present_mode.json"),
        FPS_UNLOCKER_JSON,
    )?;
    Ok(())
}

fn build_optimizer(cc: &str, tmp: &Path, dest: &Path) -> tempest_core::Result<()> {
    let source = tmp.join("optimizer.c");
    let binary = tmp.join("vortex-optim");
    std::fs::write(&source, OPTIMIZER_C)?;
    compile(
        cc,
        &["-O2", "-std=c11", "-Wall", "-Wextra"],
        &source,
        &binary,
        "a working C compiler is required",
    )?;
    std::fs::copy(&binary, dest.join("vortex-optim"))?;
    Ok(())
}

/// Run the compiler, and report *why* it failed rather than just that it did.
fn compile(
    cc: &str,
    flags: &[&str],
    source: &Path,
    output: &Path,
    hint: &str,
) -> tempest_core::Result<()> {
    let out = Command::new(cc)
        .args(flags)
        .arg("-o")
        .arg(output)
        .arg(source)
        .output()
        .map_err(|e| {
            tempest_core::TempestError::missing(
                format!("C compiler ({cc})"),
                format!("{e}. Set $CC, or install a compiler. {hint}"),
            )
        })?;

    if !out.status.success() {
        // Upstream discarded the compiler's diagnostics and printed only
        // "compilation failed", which told the user nothing.
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(15).collect();
        return Err(tempest_core::TempestError::other(format!(
            "{cc} failed. {hint}\n{}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        )));
    }
    Ok(())
}

fn uninstall(platform: &PlatformRef, name: &str) -> tempest_core::Result<()> {
    let dir = plugin_dir(platform, name);
    if !dir.is_dir() {
        return Err(tempest_core::TempestError::other(format!(
            "plugin '{name}' is not installed"
        )));
    }
    std::fs::remove_dir_all(&dir)?;
    println!("{} removed {name}", "[DONE]".green());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_names_cannot_traverse_out_of_the_plugins_directory() {
        assert_eq!(sanitize("../../etc"), "etc");
        assert_eq!(sanitize("fps-unlocker"), "fps-unlocker");
        assert_eq!(sanitize("a/b"), "ab");
        assert!(!sanitize("../x").contains('.'));
    }

    #[test]
    fn compile_reports_the_compilers_own_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("bad.c");
        std::fs::write(&source, b"this is not valid C at all\n").unwrap();

        let err = compile(
            "cc",
            &["-O2"],
            &source,
            &dir.path().join("out"),
            "a hint for the user",
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("a hint for the user"), "{msg}");
        // The point of the change: the compiler's actual complaint is shown.
        assert!(msg.len() > 60, "diagnostics were discarded: {msg}");
    }

    #[test]
    fn a_missing_compiler_is_reported_as_missing_with_a_hint() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("x.c");
        std::fs::write(&source, b"int main(void){return 0;}\n").unwrap();

        let err = compile(
            "definitely-not-a-compiler",
            &[],
            &source,
            &dir.path().join("out"),
            "install one",
        )
        .unwrap_err();
        assert_eq!(err.kind(), "missing");
        assert!(err.to_string().contains("install one"));
    }
}
