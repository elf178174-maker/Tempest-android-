//! DLL placement inside the Wine prefix.
//!
//! Carried over from upstream, with two changes: the PE check no longer shells
//! out to `file(1)` (absent on Android), and the registry override is written
//! through the guest abstraction instead of assuming a host `wine` on `$PATH`.

use crate::{Result, TempestError};
use std::io::Read;
use std::path::Path;

pub const DXVK_DLLS: &[&str] = &["d3d8.dll", "d3d9.dll", "d3d10core.dll", "d3d11.dll", "dxgi.dll"];
pub const VKD3D_DLLS: &[&str] = &["d3d12.dll", "d3d12core.dll"];

pub const DXVK_OVERRIDES: &[(&str, &str)] = &[
    ("d3d8", "native,builtin"),
    ("d3d9", "native,builtin"),
    ("d3d10core", "native,builtin"),
    ("d3d11", "native,builtin"),
    ("dxgi", "native,builtin"),
];
pub const VKD3D_OVERRIDES: &[(&str, &str)] = &[("d3d12", "native"), ("d3d12core", "native")];

/// Which machine a PE file targets. Getting this right matters on ARM64: a
/// DXVK built for x86-64 works but runs under emulation, while the ARM64 build
/// Hangover ships runs natively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeMachine {
    I386,
    Amd64,
    Arm64,
    Other(u16),
}

impl PeMachine {
    pub fn describe(self) -> String {
        match self {
            PeMachine::I386 => "x86 (32-bit, emulated on ARM64)".into(),
            PeMachine::Amd64 => "x86-64 (emulated on ARM64)".into(),
            PeMachine::Arm64 => "ARM64 (native)".into(),
            PeMachine::Other(m) => format!("unknown machine 0x{m:04x}"),
        }
    }
}

/// Read a PE header and report its target machine.
///
/// Returns `None` when the file is not a PE image at all, which is how a
/// truncated or HTML-error-page "download" gets caught before it is installed
/// into the prefix.
pub fn pe_machine(path: &Path) -> Option<PeMachine> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut dos = [0u8; 64];
    file.read_exact(&mut dos).ok()?;
    if &dos[0..2] != b"MZ" {
        return None;
    }
    let pe_offset = u32::from_le_bytes([dos[60], dos[61], dos[62], dos[63]]) as u64;
    // A sane PE header offset; guards against a hostile e_lfanew.
    if pe_offset > 1024 * 1024 {
        return None;
    }
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(pe_offset)).ok()?;
    let mut head = [0u8; 6];
    file.read_exact(&mut head).ok()?;
    if &head[0..4] != b"PE\0\0" {
        return None;
    }
    Some(match u16::from_le_bytes([head[4], head[5]]) {
        0x014c => PeMachine::I386,
        0x8664 => PeMachine::Amd64,
        0xaa64 => PeMachine::Arm64,
        other => PeMachine::Other(other),
    })
}

pub fn is_pe(path: &Path) -> bool {
    pe_machine(path).is_some()
}

/// Copy `src` over `dest`, keeping one backup of whatever was there.
pub fn install_dll(src: &Path, dest: &Path) -> Result<()> {
    if !is_pe(src) {
        return Err(TempestError::runtime(
            src.file_name().unwrap_or_default().to_string_lossy(),
            format!("{} is not a Windows DLL — the download is corrupt", src.display()),
        ));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Back up the builtin only once: re-running an install must not overwrite
    // the original Wine DLL with a previously installed DXVK one.
    let backup = dest.with_extension("dll.bak");
    if dest.exists() && !backup.exists() {
        std::fs::copy(dest, &backup)?;
    }
    std::fs::copy(src, dest)?;
    Ok(())
}

/// Install every DLL in `names` found under `src_dir` into `dest_dir`.
/// Returns the names actually installed.
pub fn install_dlls_from(src_dir: &Path, dest_dir: &Path, names: &[&str]) -> Result<Vec<String>> {
    if !src_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut installed = Vec::new();
    for name in names {
        let src = src_dir.join(name);
        if src.exists() {
            install_dll(&src, &dest_dir.join(name))?;
            installed.push((*name).to_string());
        }
    }
    Ok(installed)
}

/// Pick the best available DLL flavour for this host.
///
/// Hangover's DXVK tarball carries `aarch64`, `arm64ec`, `x64` and `x32`
/// directories. On ARM64 the native builds avoid running the translation layer
/// for every Direct3D call, so they are preferred; on x86-64 desktop only `x64`
/// and `x32` exist.
pub fn preferred_dll_dir(root: &Path, sixty_four_bit: bool, host_is_arm64: bool) -> Option<std::path::PathBuf> {
    let candidates: &[&str] = match (sixty_four_bit, host_is_arm64) {
        (true, true) => &["arm64ec", "aarch64", "x64"],
        (true, false) => &["x64"],
        (false, true) => &["x32", "x86"],
        (false, false) => &["x32", "x86"],
    };
    candidates
        .iter()
        .map(|d| root.join(d))
        .find(|p| p.is_dir())
}

/// The registry fragment that sets DLL overrides.
///
/// Applied by importing a `.reg` file with `wine regedit`, rather than by
/// running `wine reg add` once per DLL: one guest process instead of seven,
/// which on an emulated stack is the difference between instant and slow.
pub fn overrides_reg(entries: &[(&str, &str)]) -> String {
    let mut out = String::from("REGEDIT4\r\n\r\n[HKEY_CURRENT_USER\\Software\\Wine\\DllOverrides]\r\n");
    for (name, mode) in entries {
        out.push_str(&format!("\"{name}\"=\"{mode}\"\r\n"));
    }
    out.push_str("\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal but structurally valid PE file for `machine`.
    fn write_pe(path: &Path, machine: u16) {
        let mut buf = vec![0u8; 0x100];
        buf[0] = b'M';
        buf[1] = b'Z';
        let pe_off: u32 = 0x80;
        buf[60..64].copy_from_slice(&pe_off.to_le_bytes());
        let o = pe_off as usize;
        buf[o..o + 4].copy_from_slice(b"PE\0\0");
        buf[o + 4..o + 6].copy_from_slice(&machine.to_le_bytes());
        std::fs::write(path, buf).unwrap();
    }

    #[test]
    fn recognises_pe_machines_without_shelling_out() {
        let dir = tempfile::tempdir().unwrap();
        for (machine, expected) in [
            (0x014cu16, PeMachine::I386),
            (0x8664, PeMachine::Amd64),
            (0xaa64, PeMachine::Arm64),
        ] {
            let p = dir.path().join(format!("{machine}.dll"));
            write_pe(&p, machine);
            assert_eq!(pe_machine(&p), Some(expected));
        }
    }

    #[test]
    fn rejects_non_pe_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("notadll.dll");
        // The realistic failure: an HTML error page saved as a .dll.
        std::fs::write(&p, b"<!DOCTYPE html><html><body>404</body></html>").unwrap();
        assert!(!is_pe(&p));

        let short = dir.path().join("short.dll");
        std::fs::write(&short, b"MZ").unwrap();
        assert!(!is_pe(&short), "truncated file must not pass");

        // MZ header but a bogus PE offset.
        let bogus = dir.path().join("bogus.dll");
        let mut buf = vec![0u8; 64];
        buf[0] = b'M';
        buf[1] = b'Z';
        buf[60..64].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        std::fs::write(&bogus, buf).unwrap();
        assert!(!is_pe(&bogus));
    }

    #[test]
    fn install_refuses_a_corrupt_download() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("d3d11.dll");
        std::fs::write(&src, b"not a dll").unwrap();
        let err = install_dll(&src, &dir.path().join("dest/d3d11.dll")).unwrap_err();
        assert_eq!(err.kind(), "runtime");
        assert!(err.to_string().contains("corrupt"), "{err}");
    }

    #[test]
    fn install_backs_up_the_builtin_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let system32 = dir.path().join("system32");
        std::fs::create_dir_all(&system32).unwrap();
        let dest = system32.join("d3d11.dll");
        std::fs::write(&dest, b"ORIGINAL WINE BUILTIN").unwrap();

        let src_a = dir.path().join("a.dll");
        write_pe(&src_a, 0x8664);
        install_dll(&src_a, &dest).unwrap();

        let src_b = dir.path().join("b.dll");
        write_pe(&src_b, 0xaa64);
        install_dll(&src_b, &dest).unwrap();

        let backup = std::fs::read(system32.join("d3d11.dll.bak")).unwrap();
        assert_eq!(
            backup, b"ORIGINAL WINE BUILTIN",
            "the second install clobbered the original backup"
        );
        assert_eq!(pe_machine(&dest), Some(PeMachine::Arm64));
    }

    #[test]
    fn install_dlls_from_reports_what_it_found() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("x64");
        std::fs::create_dir_all(&src).unwrap();
        write_pe(&src.join("d3d11.dll"), 0x8664);
        write_pe(&src.join("dxgi.dll"), 0x8664);
        // d3d9.dll deliberately absent.

        let dest = dir.path().join("system32");
        let installed = install_dlls_from(&src, &dest, DXVK_DLLS).unwrap();
        assert_eq!(installed, vec!["d3d11.dll", "dxgi.dll"]);
        assert!(dest.join("d3d11.dll").exists());
        assert!(!dest.join("d3d9.dll").exists());

        // A missing source directory is not an error.
        assert!(install_dlls_from(&dir.path().join("nope"), &dest, DXVK_DLLS).unwrap().is_empty());
    }

    #[test]
    fn arm64_hosts_prefer_native_dll_builds() {
        let dir = tempfile::tempdir().unwrap();
        for d in ["x64", "x32", "aarch64", "arm64ec"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        assert_eq!(
            preferred_dll_dir(dir.path(), true, true).unwrap(),
            dir.path().join("arm64ec")
        );
        assert_eq!(
            preferred_dll_dir(dir.path(), true, false).unwrap(),
            dir.path().join("x64")
        );
        assert_eq!(
            preferred_dll_dir(dir.path(), false, true).unwrap(),
            dir.path().join("x32")
        );
    }

    #[test]
    fn falls_back_to_x64_when_no_native_build_is_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("x64")).unwrap();
        assert_eq!(
            preferred_dll_dir(dir.path(), true, true).unwrap(),
            dir.path().join("x64")
        );
        assert!(preferred_dll_dir(dir.path(), false, true).is_none());
    }

    #[test]
    fn registry_fragment_is_well_formed() {
        let reg = overrides_reg(DXVK_OVERRIDES);
        assert!(reg.starts_with("REGEDIT4\r\n"));
        assert!(reg.contains(r"[HKEY_CURRENT_USER\Software\Wine\DllOverrides]"));
        assert!(reg.contains("\"d3d11\"=\"native,builtin\""));
        assert!(reg.contains("\"dxgi\"=\"native,builtin\""));
        // Every line is CRLF-terminated, as .reg files must be.
        assert!(!reg.contains('\n') || reg.matches('\n').count() == reg.matches("\r\n").count());
        assert!(reg.ends_with("\r\n\r\n"));
    }
}
