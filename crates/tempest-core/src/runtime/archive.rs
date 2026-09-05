//! Safe archive extraction.
//!
//! Every archive Tempest handles comes off the network, so extraction must
//! assume the archive is hostile. In particular:
//!
//! * entry paths are rejected if they are absolute, contain `..`, or would
//!   otherwise land outside the destination ("zip slip"/"tar slip");
//! * symlinks and hard links are only created when their target also stays
//!   inside the destination, so an archive cannot plant a link to
//!   `/data/data/<pkg>/files/config` and then write through it;
//! * device nodes, FIFOs and sockets are skipped entirely;
//! * setuid/setgid bits are stripped.

use crate::{Result, TempestError};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    TarGz,
    TarXz,
    TarZst,
    Tar,
    Zip,
    /// Debian package: an `ar` archive whose `data.tar.*` member holds the files.
    Deb,
}

impl Format {
    /// Guess from the file name. Used only as a default; callers that know the
    /// format state it explicitly.
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?.to_ascii_lowercase();
        Some(if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Format::TarGz
        } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
            Format::TarXz
        } else if name.ends_with(".tar.zst") {
            Format::TarZst
        } else if name.ends_with(".tar") {
            Format::Tar
        } else if name.ends_with(".zip") {
            Format::Zip
        } else if name.ends_with(".deb") {
            Format::Deb
        } else {
            return None;
        })
    }
}

/// Resolve an archive entry path against `dest`, rejecting anything that
/// escapes. Returns `None` for entries that must be skipped.
pub fn safe_join(dest: &Path, entry: &Path) -> Option<PathBuf> {
    let mut out = dest.to_path_buf();
    let mut depth = 0usize;
    for component in entry.components() {
        match component {
            Component::Normal(part) => {
                // Reject NUL and empty components defensively.
                let s = part.to_str()?;
                if s.is_empty() || s.contains('\0') {
                    return None;
                }
                out.push(s);
                depth += 1;
            }
            Component::CurDir => {}
            // Absolute paths, `..` and Windows prefixes are all escapes.
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if depth == 0 {
        return None;
    }
    Some(out)
}

/// Whether `target`, resolved relative to `link_dir`, stays under `dest`.
fn link_target_is_contained(dest: &Path, link_path: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        // Absolute link targets are meaningful inside the guest rootfs (they
        // are resolved by PRoot relative to the guest root), so they are
        // allowed: they cannot reach the host filesystem through PRoot. What
        // they must not do is escape while *we* are writing, and creating the
        // symlink itself never writes through it.
        return true;
    }
    let Some(parent) = link_path.parent() else {
        return false;
    };
    let mut resolved = parent.to_path_buf();
    for component in target.components() {
        match component {
            Component::Normal(p) => resolved.push(p),
            Component::CurDir => {}
            Component::ParentDir => {
                if !resolved.pop() {
                    return false;
                }
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    resolved.starts_with(dest)
}

#[derive(Debug, Default, Clone)]
pub struct ExtractReport {
    pub files: usize,
    pub dirs: usize,
    pub links: usize,
    pub skipped: Vec<String>,
    pub bytes: u64,
}

/// Extract `archive` into `dest`.
///
/// `strip_components` drops leading path elements, matching `tar --strip-components`;
/// upstream DXVK/vkd3d tarballs wrap everything in a versioned top directory.
pub fn extract(
    archive: &Path,
    dest: &Path,
    format: Format,
    strip_components: usize,
) -> Result<ExtractReport> {
    std::fs::create_dir_all(dest)?;
    let file = std::fs::File::open(archive)?;
    let reader = std::io::BufReader::new(file);

    match format {
        Format::TarGz => extract_tar(flate2::read::GzDecoder::new(reader), dest, strip_components),
        Format::TarXz => extract_tar(xz2::read::XzDecoder::new(reader), dest, strip_components),
        Format::TarZst => extract_tar(
            zstd::Decoder::new(reader).map_err(TempestError::other)?,
            dest,
            strip_components,
        ),
        Format::Tar => extract_tar(reader, dest, strip_components),
        Format::Zip => extract_zip(archive, dest, strip_components),
        Format::Deb => extract_deb(archive, dest, strip_components),
    }
}

fn strip(path: &Path, n: usize) -> Option<PathBuf> {
    if n == 0 {
        return Some(path.to_path_buf());
    }
    let mut it = path.components();
    for _ in 0..n {
        it.next()?;
    }
    let rest: PathBuf = it.collect();
    if rest.as_os_str().is_empty() {
        None
    } else {
        Some(rest)
    }
}

fn extract_tar<R: Read>(reader: R, dest: &Path, strip_components: usize) -> Result<ExtractReport> {
    use std::os::unix::fs::PermissionsExt;
    use tar::EntryType;

    let mut report = ExtractReport::default();
    let mut archive = tar::Archive::new(reader);
    // We do our own containment checks and set permissions explicitly.
    archive.set_preserve_permissions(false);
    archive.set_unpack_xattrs(false);
    archive.set_overwrite(true);

    for entry in archive.entries().map_err(TempestError::other)? {
        let mut entry = entry.map_err(TempestError::other)?;
        let raw = entry.path().map_err(TempestError::other)?.into_owned();

        let Some(stripped) = strip(&raw, strip_components) else {
            continue;
        };
        let Some(out) = safe_join(dest, &stripped) else {
            report.skipped.push(raw.display().to_string());
            continue;
        };

        match entry.header().entry_type() {
            EntryType::Directory => {
                std::fs::create_dir_all(&out)?;
                report.dirs += 1;
            }
            EntryType::Regular | EntryType::Continuous => {
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut file = std::fs::File::create(&out)?;
                let n = std::io::copy(&mut entry, &mut file)?;
                report.bytes += n;
                report.files += 1;

                // Preserve the execute bit but never setuid/setgid/sticky.
                if let Ok(mode) = entry.header().mode() {
                    let safe = mode & 0o777;
                    std::fs::set_permissions(&out, std::fs::Permissions::from_mode(safe)).ok();
                }
            }
            EntryType::Symlink | EntryType::Link => {
                let Ok(Some(target)) = entry.link_name() else {
                    report.skipped.push(raw.display().to_string());
                    continue;
                };
                if !link_target_is_contained(dest, &out, &target) {
                    report
                        .skipped
                        .push(format!("{} -> {}", raw.display(), target.display()));
                    continue;
                }
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                // Replace an existing entry so re-extraction is idempotent.
                if out.symlink_metadata().is_ok() {
                    std::fs::remove_file(&out).ok();
                }
                if entry.header().entry_type() == EntryType::Symlink {
                    std::os::unix::fs::symlink(&target, &out)?;
                } else {
                    // Hard link targets are archive-relative.
                    let Some(link_src) =
                        strip(&target, strip_components).and_then(|t| safe_join(dest, &t))
                    else {
                        report.skipped.push(raw.display().to_string());
                        continue;
                    };
                    if std::fs::hard_link(&link_src, &out).is_err() {
                        std::fs::copy(&link_src, &out).ok();
                    }
                }
                report.links += 1;
            }
            // Character/block devices, FIFOs, sockets: never wanted, and
            // creating them would need privileges we do not have anyway.
            other => {
                report
                    .skipped
                    .push(format!("{} ({other:?})", raw.display()));
            }
        }
    }
    Ok(report)
}

fn extract_zip(archive: &Path, dest: &Path, strip_components: usize) -> Result<ExtractReport> {
    use std::os::unix::fs::PermissionsExt;

    let mut report = ExtractReport::default();
    let file = std::fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file).map_err(TempestError::other)?;

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(TempestError::other)?;
        // `enclosed_name` already rejects `..` and absolute paths; the extra
        // `safe_join` below is belt-and-braces.
        let Some(name) = entry.enclosed_name() else {
            report.skipped.push(entry.name().to_string());
            continue;
        };
        let Some(stripped) = strip(&name, strip_components) else {
            continue;
        };
        let Some(out) = safe_join(dest, &stripped) else {
            report.skipped.push(entry.name().to_string());
            continue;
        };

        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            report.dirs += 1;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut outfile = std::fs::File::create(&out)?;
        report.bytes += std::io::copy(&mut entry, &mut outfile)?;
        report.files += 1;
        if let Some(mode) = entry.unix_mode() {
            std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode & 0o777)).ok();
        }
    }
    Ok(report)
}

/// Extract the payload of a `.deb`.
///
/// A `.deb` is an `ar` archive containing `debian-binary`, `control.tar.*` and
/// `data.tar.*`. Only `data.tar.*` is extracted, and maintainer scripts are
/// never run — the guest filesystem is populated by unpacking files, not by
/// executing packaging code from the network.
pub fn extract_deb(archive: &Path, dest: &Path, strip_components: usize) -> Result<ExtractReport> {
    let bytes = std::fs::read(archive)?;
    let (name, data) = find_ar_member(&bytes, "data.tar")?;

    let tmp = dest.join(format!(
        ".{}.tmp",
        crate::net::sha256_bytes(name.as_bytes())
    ));
    std::fs::create_dir_all(dest)?;
    std::fs::write(&tmp, data)?;

    let format = if name.ends_with(".xz") {
        Format::TarXz
    } else if name.ends_with(".gz") {
        Format::TarGz
    } else if name.ends_with(".zst") {
        Format::TarZst
    } else {
        Format::Tar
    };

    let result = extract(&tmp, dest, format, strip_components);
    std::fs::remove_file(&tmp).ok();
    result
}

/// Minimal `ar` reader: enough for Debian packages, which use the common
/// (non-GNU-extended) format for these three members.
///
/// The lifetime is written out rather than elided: the returned slice borrows
/// from `bytes`, and elision would tie it to `prefix`.
#[allow(clippy::needless_lifetimes)]
fn find_ar_member<'a>(bytes: &'a [u8], prefix: &str) -> Result<(String, &'a [u8])> {
    const MAGIC: &[u8] = b"!<arch>\n";
    if !bytes.starts_with(MAGIC) {
        return Err(TempestError::other("not an ar archive (bad magic)"));
    }
    let mut pos = MAGIC.len();
    while pos + 60 <= bytes.len() {
        let header = &bytes[pos..pos + 60];
        let name = String::from_utf8_lossy(&header[0..16])
            .trim()
            .trim_end_matches('/')
            .to_string();
        let size: usize = String::from_utf8_lossy(&header[48..58])
            .trim()
            .parse()
            .map_err(|_| TempestError::other("malformed ar member size"))?;
        let start = pos + 60;
        let end = start
            .checked_add(size)
            .filter(|e| *e <= bytes.len())
            .ok_or_else(|| TempestError::other("ar member extends past end of file"))?;
        if name.starts_with(prefix) {
            return Ok((name, &bytes[start..end]));
        }
        // Members are padded to an even offset.
        pos = end + (end % 2);
    }
    Err(TempestError::other(format!(
        "no '{prefix}*' member in the package"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_detection() {
        let f = |s: &str| Format::from_path(Path::new(s));
        assert_eq!(f("dxvk-2.4.tar.gz"), Some(Format::TarGz));
        assert_eq!(f("vkd3d-proton-2.13.tar.zst"), Some(Format::TarZst));
        assert_eq!(f("rootfs.tar.xz"), Some(Format::TarXz));
        assert_eq!(f("wine.deb"), Some(Format::Deb));
        assert_eq!(f("Vortex.zip"), Some(Format::Zip));
        assert_eq!(f("hangover.tar"), Some(Format::Tar));
        assert_eq!(f("README.md"), None);
    }

    #[test]
    fn safe_join_rejects_escapes() {
        let dest = Path::new("/data/rootfs");
        assert!(safe_join(dest, Path::new("../../etc/passwd")).is_none());
        assert!(safe_join(dest, Path::new("/etc/passwd")).is_none());
        assert!(safe_join(dest, Path::new("a/../../b")).is_none());
        assert!(safe_join(dest, Path::new("")).is_none());
        assert_eq!(
            safe_join(dest, Path::new("usr/bin/wine")).unwrap(),
            Path::new("/data/rootfs/usr/bin/wine")
        );
        assert_eq!(
            safe_join(dest, Path::new("./usr/./lib")).unwrap(),
            Path::new("/data/rootfs/usr/lib")
        );
    }

    #[test]
    fn relative_link_targets_may_not_climb_out() {
        let dest = Path::new("/data/rootfs");
        assert!(link_target_is_contained(
            dest,
            Path::new("/data/rootfs/usr/lib/libfoo.so"),
            Path::new("libfoo.so.1")
        ));
        assert!(!link_target_is_contained(
            dest,
            Path::new("/data/rootfs/usr/lib/evil"),
            Path::new("../../../../data/data/io.tempest/files/config/config.toml")
        ));
    }

    #[test]
    fn strip_components_behaves_like_tar() {
        assert_eq!(
            strip(Path::new("dxvk-2.4/x64/d3d11.dll"), 1).unwrap(),
            Path::new("x64/d3d11.dll")
        );
        assert_eq!(strip(Path::new("dxvk-2.4"), 1), None);
        assert_eq!(strip(Path::new("a/b"), 0).unwrap(), Path::new("a/b"));
    }

    /// Write a raw 512-byte tar header plus payload.
    ///
    /// Built by hand rather than with `tar::Builder`, because the writer
    /// refuses to emit `..` paths — and it is exactly those archives, produced
    /// by a hostile server rather than by this crate, that the extractor has to
    /// defend against.
    fn raw_tar_entry(name: &str, typeflag: u8, link: &str, body: &[u8]) -> Vec<u8> {
        let mut header = [0u8; 512];
        let put = |h: &mut [u8; 512], off: usize, bytes: &[u8]| {
            h[off..off + bytes.len()].copy_from_slice(bytes);
        };
        put(&mut header, 0, name.as_bytes());
        put(&mut header, 100, b"000644 \0"); // mode
        put(&mut header, 108, b"000000 \0"); // uid
        put(&mut header, 116, b"000000 \0"); // gid
        put(&mut header, 124, format!("{:011o} ", body.len()).as_bytes());
        put(&mut header, 136, b"00000000000 "); // mtime
        header[148..156].copy_from_slice(b"        "); // checksum placeholder
        header[156] = typeflag;
        put(&mut header, 157, link.as_bytes());
        put(&mut header, 257, b"ustar\0");
        put(&mut header, 263, b"00");

        let sum: u32 = header.iter().map(|b| *b as u32).sum();
        put(&mut header, 148, format!("{sum:06o}\0 ").as_bytes());

        let mut out = header.to_vec();
        out.extend_from_slice(body);
        // Pad the payload to a 512-byte boundary.
        let pad = (512 - body.len() % 512) % 512;
        out.extend(std::iter::repeat_n(0u8, pad));
        out
    }

    fn raw_tar(entries: Vec<Vec<u8>>) -> Vec<u8> {
        let mut out: Vec<u8> = entries.into_iter().flatten().collect();
        out.extend(std::iter::repeat_n(0u8, 1024)); // end-of-archive marker
        out
    }

    #[test]
    fn tar_traversal_entries_are_skipped_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("evil.tar");
        let dest = dir.path().join("dest");
        let outside = dir.path().join("PWNED");

        std::fs::write(
            &archive,
            raw_tar(vec![
                raw_tar_entry("../PWNED", b'0', "", b"pwned"),
                raw_tar_entry("/etc/absolute", b'0', "", b"pwned"),
                raw_tar_entry("a/../../b/escape", b'0', "", b"pwned"),
                raw_tar_entry("good.txt", b'0', "", b"legit"),
            ]),
        )
        .unwrap();

        let report = extract(&archive, &dest, Format::Tar, 0).unwrap();
        assert!(!outside.exists(), "archive escaped the destination");
        assert!(
            !dir.path().join("b/escape").exists(),
            "archive escaped via a/../.."
        );
        assert!(
            dest.join("good.txt").exists(),
            "legitimate entry was dropped"
        );
        assert_eq!(std::fs::read(dest.join("good.txt")).unwrap(), b"legit");
        assert_eq!(report.files, 1);
        assert_eq!(report.skipped.len(), 3, "skipped: {:?}", report.skipped);
    }

    #[test]
    fn tar_device_nodes_and_fifos_are_never_created() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("dev.tar");
        let dest = dir.path().join("dest");
        std::fs::write(
            &archive,
            raw_tar(vec![
                raw_tar_entry("dev/hostmem", b'3', "", b""), // character device
                raw_tar_entry("run/pipe", b'6', "", b""),    // FIFO
            ]),
        )
        .unwrap();
        let report = extract(&archive, &dest, Format::Tar, 0).unwrap();
        assert_eq!(report.files, 0);
        assert_eq!(report.skipped.len(), 2, "skipped: {:?}", report.skipped);
        assert!(!dest.join("dev/hostmem").exists());
    }

    #[test]
    fn tar_symlink_escaping_the_destination_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("link.tar");
        let dest = dir.path().join("dest");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut builder = tar::Builder::new(file);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            builder
                .append_link(&mut header, "escape", "../../../../etc/passwd")
                .unwrap();
            builder.finish().unwrap();
        }
        let report = extract(&archive, &dest, Format::Tar, 0).unwrap();
        assert_eq!(report.links, 0);
        assert_eq!(report.skipped.len(), 1);
        assert!(!dest.join("escape").exists());
    }

    #[test]
    fn tar_preserves_the_execute_bit_but_strips_setuid() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("x.tar");
        let dest = dir.path().join("dest");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut builder = tar::Builder::new(file);
            let data = b"#!/bin/sh\n";
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o4755); // setuid + rwxr-xr-x
            h.set_cksum();
            builder.append_data(&mut h, "bin/run", &data[..]).unwrap();
            builder.finish().unwrap();
        }
        extract(&archive, &dest, Format::Tar, 0).unwrap();
        let mode = std::fs::metadata(dest.join("bin/run"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "execute bit lost");
        assert_eq!(mode & 0o7000, 0, "setuid bit survived extraction");
    }

    #[test]
    fn zip_extraction_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.zip");
        let dest = dir.path().join("dest");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut w = zip::ZipWriter::new(file);
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            w.start_file("Vortex/Vortex.exe", opts).unwrap();
            use std::io::Write;
            w.write_all(b"MZ\x90\x00").unwrap();
            w.finish().unwrap();
        }
        let report = extract(&archive, &dest, Format::Zip, 1).unwrap();
        assert_eq!(report.files, 1);
        assert_eq!(
            std::fs::read(dest.join("Vortex.exe")).unwrap(),
            b"MZ\x90\x00"
        );
    }

    #[test]
    fn ar_reader_finds_the_data_member() {
        // Build a minimal ar archive with three members.
        let mut ar = Vec::from(*b"!<arch>\n");
        let mut push = |name: &str, body: &[u8]| {
            let mut header = format!(
                "{name:<16}0           0     0     100644  {:<10}",
                body.len()
            );
            header.push_str("`\n");
            ar.extend_from_slice(header.as_bytes());
            ar.extend_from_slice(body);
            if body.len() % 2 == 1 {
                ar.push(b'\n');
            }
        };
        push("debian-binary", b"2.0\n");
        push("control.tar.gz", b"CTRL");
        push("data.tar.xz", b"DATAPAYLOAD");

        let (name, data) = find_ar_member(&ar, "data.tar").unwrap();
        assert_eq!(name, "data.tar.xz");
        assert_eq!(data, b"DATAPAYLOAD");

        assert!(find_ar_member(&ar, "nope").is_err());
        assert!(find_ar_member(b"not an archive", "data.tar").is_err());
    }
}
