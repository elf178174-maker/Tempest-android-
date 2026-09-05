//! The catalogue of runtime components.
//!
//! Nothing here points at a random file-host: every entry resolves to an
//! official upstream release, and every entry that has a stable URL carries a
//! pinned SHA-256. The catalogue is data, not code, so a version bump is a
//! one-line change that CI can verify.
//!
//! The chain these components build, on an ARM64 Android phone:
//!
//! ```text
//!   Android ARM64 app process
//!     └─ PRoot          unprivileged chroot; the only executable binary,
//!        (in the APK)   because Android forbids exec() from app data
//!         └─ Ubuntu Base 24.04 arm64 (glibc guest filesystem)
//!             └─ Hangover Wine (native ARM64 Wine, LGPL-2.1+)
//!                 └─ FEX / wowbox64  x86-64 and x86 PE -> ARM64
//!                     └─ Vortex.exe, then the game
//!                         └─ DXVK (native ARM64 PE build) -> Vulkan
//!                             └─ Mesa Turnip or lavapipe -> Adreno / CPU
//! ```

use serde::{Deserialize, Serialize};

/// Stable identifier for a component. Used as a map key in persisted state, so
/// the string values must not change once released.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComponentId {
    /// Ubuntu Base ARM64 filesystem the guest runs in.
    Rootfs,
    /// Hangover: ARM64 Wine plus the FEX and Box64 WoW64 emulators, and a
    /// matching DXVK built as native ARM64 PE.
    Hangover,
    /// Mesa Vulkan drivers (lavapipe software rasteriser, plus Turnip when the
    /// build supports Adreno KGSL).
    Mesa,
    /// DXVK, when the user wants a version other than the one Hangover ships.
    Dxvk,
    /// vkd3d-proton, for Direct3D 12 titles. Optional.
    Vkd3d,
    /// Vortex.exe itself, from playvortex.io.
    Vortex,
}

impl ComponentId {
    pub fn as_str(self) -> &'static str {
        match self {
            ComponentId::Rootfs => "rootfs",
            ComponentId::Hangover => "hangover",
            ComponentId::Mesa => "mesa",
            ComponentId::Dxvk => "dxvk",
            ComponentId::Vkd3d => "vkd3d",
            ComponentId::Vortex => "vortex",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "rootfs" => ComponentId::Rootfs,
            "hangover" => ComponentId::Hangover,
            "mesa" => ComponentId::Mesa,
            "dxvk" => ComponentId::Dxvk,
            "vkd3d" => ComponentId::Vkd3d,
            "vortex" => ComponentId::Vortex,
            _ => return None,
        })
    }

    pub fn all() -> &'static [ComponentId] {
        &[
            ComponentId::Rootfs,
            ComponentId::Hangover,
            ComponentId::Mesa,
            ComponentId::Dxvk,
            ComponentId::Vkd3d,
            ComponentId::Vortex,
        ]
    }
}

/// How a component's download URL is determined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A fixed URL with a pinned digest. Preferred: reproducible and verifiable.
    Pinned {
        url: &'static str,
        sha256: &'static str,
    },
    /// Resolved at install time from a GitHub release. Used where upstream does
    /// not publish stable per-version URLs we can pin ahead of time; the digest
    /// of whatever arrives is recorded and shown in the UI.
    GithubLatest {
        repo: &'static str,
        asset_suffix: &'static str,
    },
    /// Fetched from Vortex itself, authenticated with the user's session.
    VortexDownload { url: &'static str },
    /// Installed with `apt-get` *inside* the guest filesystem.
    ///
    /// Used where a component has a dependency graph we should not try to
    /// resolve by hand: Mesa's software rasteriser alone pulls in LLVM, libdrm
    /// and a dozen more. Unpacking a single `.deb` would produce a library that
    /// fails to load at runtime with a confusing error, so apt does the work.
    GuestPackages { packages: &'static [&'static str] },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Necessity {
    /// Without it nothing can run.
    Required,
    /// Improves things or covers specific titles.
    Optional,
    /// Does not apply to this host at all.
    NotApplicable,
}

#[derive(Debug, Clone)]
pub struct ComponentSpec {
    pub id: ComponentId,
    pub display_name: &'static str,
    pub version: &'static str,
    pub source: Source,
    pub archive_name: &'static str,
    pub format: super::archive::Format,
    /// Path components stripped during extraction.
    pub strip_components: usize,
    /// How badly this is needed, per host. A desktop already has Wine and a
    /// glibc userland from its distribution, so most of the Android stack is
    /// simply not applicable there — and downloading an ARM64 Ubuntu image onto
    /// an x86-64 desktop would be actively wrong.
    pub necessity_android: Necessity,
    pub necessity_linux: Necessity,
    /// Approximate download size, so the UI can warn before a 300 MB pull on
    /// mobile data.
    pub approx_bytes: u64,
    pub license: &'static str,
    pub upstream: &'static str,
    /// One-line explanation shown next to the component in Settings.
    pub purpose: &'static str,
    /// A path, relative to the guest root, that must exist for the component to
    /// count as installed.
    pub sentinel: &'static str,
}

/// Ubuntu Base is published with a signed `SHA256SUMS` alongside the images;
/// this digest is the `ubuntu-base-24.04.3-base-arm64.tar.gz` line from it.
const ROOTFS_SHA256: &str = "7b2dced6dd56ad5e4a813fa25c8de307b655fdabc6ea9213175a92c48dabb048";

/// SHA-256 of `hangover_11.16_ubuntu2404_noble_arm64.tar` as published on the
/// Hangover release page, verified when the pin was made.
const HANGOVER_SHA256: &str = "94611dad2b23978d35825a018c81b3947caaec174aa04c052849d37bc6a39e72";

pub fn catalogue() -> Vec<ComponentSpec> {
    use super::archive::Format;
    vec![
        ComponentSpec {
            id: ComponentId::Rootfs,
            display_name: "Ubuntu Base 24.04 (arm64)",
            version: "24.04.3",
            source: Source::Pinned {
                url: "https://cdimage.ubuntu.com/ubuntu-base/releases/24.04/release/ubuntu-base-24.04.3-base-arm64.tar.gz",
                sha256: ROOTFS_SHA256,
            },
            archive_name: "ubuntu-base-24.04.3-base-arm64.tar.gz",
            format: Format::TarGz,
            strip_components: 0,
            // A desktop already is a glibc Linux system.
            necessity_android: Necessity::Required,
            necessity_linux: Necessity::NotApplicable,
            approx_bytes: 30 * 1024 * 1024,
            license: "Various (see /usr/share/doc inside the image); Ubuntu is redistributable",
            upstream: "https://cdimage.ubuntu.com/ubuntu-base/releases/24.04/release/",
            purpose: "The glibc Linux filesystem Wine runs inside. Android's own \
                      C library (Bionic) cannot load Wine, so a small Linux \
                      userland is unpacked into app storage.",
            sentinel: "usr/bin/env",
        },
        ComponentSpec {
            id: ComponentId::Hangover,
            display_name: "Hangover (ARM64 Wine + FEX)",
            version: "11.16",
            source: Source::Pinned {
                url: "https://github.com/AndreRH/hangover/releases/download/hangover-11.16/hangover_11.16_ubuntu2404_noble_arm64.tar",
                sha256: HANGOVER_SHA256,
            },
            archive_name: "hangover-11.16-noble-arm64.tar",
            format: Format::Tar,
            strip_components: 0,
            // On a desktop, Wine comes from the distribution's package
            // manager; these builds are ARM64 and would not even run.
            necessity_android: Necessity::Required,
            necessity_linux: Necessity::NotApplicable,
            approx_bytes: 293 * 1024 * 1024,
            license: "Wine: LGPL-2.1-or-later; FEX: MIT; Box64: MIT; DXVK: Zlib",
            upstream: "https://github.com/AndreRH/hangover",
            purpose: "Wine built natively for ARM64, plus the FEX and Box64 \
                      emulators that run x86-64 and x86 Windows code, plus a \
                      DXVK build compiled as native ARM64 so the graphics \
                      translation itself is not emulated.",
            sentinel: "usr/bin/wine",
        },
        ComponentSpec {
            id: ComponentId::Mesa,
            display_name: "Vulkan loader and Mesa drivers",
            version: "from Ubuntu noble",
            source: Source::GuestPackages {
                packages: &["libvulkan1", "mesa-vulkan-drivers", "vulkan-tools"],
            },
            archive_name: "",
            format: Format::Deb,
            strip_components: 0,
            // Desktops get Mesa and the Vulkan loader from their distribution.
            necessity_android: Necessity::Optional,
            necessity_linux: Necessity::NotApplicable,
            approx_bytes: 120 * 1024 * 1024,
            license: "MIT (Mesa); Apache-2.0 (Vulkan-Loader)",
            upstream: "https://gitlab.freedesktop.org/mesa/mesa",
            purpose: "Provides lavapipe, a software Vulkan implementation. Slow, \
                      but it works on any device and proves the rest of the \
                      chain. Hardware Vulkan on Adreno needs a Turnip build with \
                      the KGSL backend, which is installed separately.",
            sentinel: "usr/lib/aarch64-linux-gnu/libvulkan.so.1",
        },
        ComponentSpec {
            id: ComponentId::Dxvk,
            display_name: "DXVK (standalone)",
            version: "latest release",
            source: Source::GithubLatest {
                repo: "doitsujin/dxvk",
                asset_suffix: ".tar.gz",
            },
            archive_name: "dxvk.tar.gz",
            format: Format::TarGz,
            strip_components: 1,
            // Hangover already bundles a DXVK built for ARM64, so a separate
            // one is an override on Android but the only source on a desktop.
            necessity_android: Necessity::Optional,
            necessity_linux: Necessity::Required,
            approx_bytes: 20 * 1024 * 1024,
            license: "Zlib",
            upstream: "https://github.com/doitsujin/dxvk",
            purpose: "Direct3D 9/10/11 to Vulkan — what makes almost every \
                      game render. On Android this is only needed to override \
                      the DXVK that Hangover already ships, whose ARM64 build \
                      is usually the faster choice.",
            sentinel: "opt/dxvk/x64/d3d11.dll",
        },
        ComponentSpec {
            id: ComponentId::Vkd3d,
            display_name: "vkd3d-proton",
            version: "latest release",
            source: Source::GithubLatest {
                repo: "HansKristian-Work/vkd3d-proton",
                asset_suffix: ".tar.zst",
            },
            archive_name: "vkd3d-proton.tar.zst",
            format: Format::TarZst,
            strip_components: 1,
            necessity_android: Necessity::Optional,
            necessity_linux: Necessity::Optional,
            approx_bytes: 12 * 1024 * 1024,
            license: "LGPL-2.1-or-later",
            upstream: "https://github.com/HansKristian-Work/vkd3d-proton",
            purpose: "Direct3D 12 to Vulkan. Vortex itself does not need it; \
                      install it only for a game that requires D3D12.",
            sentinel: "opt/vkd3d/x64/d3d12.dll",
        },
        ComponentSpec {
            id: ComponentId::Vortex,
            display_name: "Vortex client",
            version: "current",
            source: Source::VortexDownload {
                url: "https://playvortex.io/download/windows",
            },
            archive_name: "vortex-windows.zip",
            format: super::archive::Format::Zip,
            strip_components: 0,
            necessity_android: Necessity::Required,
            necessity_linux: Necessity::Required,
            approx_bytes: 80 * 1024 * 1024,
            license: "Proprietary — downloaded from Vortex, never redistributed by Tempest",
            upstream: "https://playvortex.io/",
            purpose: "The official Windows Vortex client that Tempest launches.",
            sentinel: "",
        },
    ]
}

impl ComponentSpec {
    pub fn necessity(&self, host: crate::platform::HostKind) -> Necessity {
        match host {
            crate::platform::HostKind::Android => self.necessity_android,
            crate::platform::HostKind::LinuxDesktop => self.necessity_linux,
        }
    }

    pub fn applies_to(&self, host: crate::platform::HostKind) -> bool {
        self.necessity(host) != Necessity::NotApplicable
    }
}

/// The components relevant to a given host, in installation order.
pub fn catalogue_for(host: crate::platform::HostKind) -> Vec<ComponentSpec> {
    catalogue()
        .into_iter()
        .filter(|c| c.applies_to(host))
        .collect()
}

pub fn spec(id: ComponentId) -> ComponentSpec {
    catalogue()
        .into_iter()
        .find(|c| c.id == id)
        .expect("catalogue covers every ComponentId")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_id_has_exactly_one_spec() {
        let all = catalogue();
        for id in ComponentId::all() {
            assert_eq!(
                all.iter().filter(|c| c.id == *id).count(),
                1,
                "{} is not covered exactly once",
                id.as_str()
            );
        }
        assert_eq!(all.len(), ComponentId::all().len());
    }

    #[test]
    fn id_strings_round_trip() {
        for id in ComponentId::all() {
            assert_eq!(ComponentId::parse(id.as_str()), Some(*id));
        }
        assert_eq!(ComponentId::parse("nonsense"), None);
    }

    #[test]
    fn every_component_documents_its_licence_and_upstream() {
        for c in catalogue() {
            assert!(!c.license.is_empty(), "{} has no licence", c.display_name);
            assert!(
                c.upstream.starts_with("https://"),
                "{} upstream is not an https URL",
                c.display_name
            );
            assert!(
                !c.purpose.is_empty(),
                "{} has no purpose text",
                c.display_name
            );
        }
    }

    #[test]
    fn downloads_use_official_upstream_hosts() {
        const ALLOWED: &[&str] = &[
            "cdimage.ubuntu.com",
            "ports.ubuntu.com",
            "github.com",
            "playvortex.io",
        ];
        for c in catalogue() {
            let url = match c.source {
                Source::Pinned { url, .. } | Source::VortexDownload { url } => url,
                Source::GithubLatest { .. } | Source::GuestPackages { .. } => continue,
            };
            let host = url::Url::parse(url)
                .unwrap()
                .host_str()
                .unwrap()
                .to_string();
            assert!(
                ALLOWED.contains(&host.as_str()),
                "{} downloads from an unapproved host: {host}",
                c.display_name
            );
        }
    }

    #[test]
    fn required_components_that_are_pinned_carry_a_digest() {
        for c in catalogue() {
            if c.necessity_android != Necessity::Required {
                continue;
            }
            if let Source::Pinned { sha256, url } = c.source {
                assert_eq!(
                    sha256.len(),
                    64,
                    "{} is required and pinned but has no SHA-256 for {url}",
                    c.display_name
                );
                assert!(
                    sha256.chars().all(|ch| ch.is_ascii_hexdigit()),
                    "{} has a malformed digest",
                    c.display_name
                );
            }
        }
    }

    #[test]
    fn the_catalogue_is_filtered_per_host() {
        use crate::platform::HostKind;

        let android = catalogue_for(HostKind::Android);
        let linux = catalogue_for(HostKind::LinuxDesktop);

        // Everything applies on Android; the desktop stack is much smaller.
        assert_eq!(android.len(), ComponentId::all().len());
        assert!(
            linux.len() < android.len(),
            "desktop should not need the whole stack"
        );

        // Downloading an ARM64 Ubuntu image onto an x86-64 desktop would be
        // nonsense, and Wine there comes from the distribution.
        for id in [
            ComponentId::Rootfs,
            ComponentId::Hangover,
            ComponentId::Mesa,
        ] {
            assert!(
                !spec(id).applies_to(HostKind::LinuxDesktop),
                "{} must not be offered on a desktop",
                id.as_str()
            );
            assert!(spec(id).applies_to(HostKind::Android));
        }

        // The Vortex client is needed everywhere.
        assert_eq!(
            spec(ComponentId::Vortex).necessity(HostKind::Android),
            Necessity::Required
        );
        assert_eq!(
            spec(ComponentId::Vortex).necessity(HostKind::LinuxDesktop),
            Necessity::Required
        );
    }

    #[test]
    fn dxvk_is_required_on_a_desktop_but_only_an_override_on_android() {
        use crate::platform::HostKind;
        // Hangover ships a DXVK built for ARM64, so a separate one is an
        // override there; a desktop has no other source.
        assert_eq!(
            spec(ComponentId::Dxvk).necessity(HostKind::Android),
            Necessity::Optional
        );
        assert_eq!(
            spec(ComponentId::Dxvk).necessity(HostKind::LinuxDesktop),
            Necessity::Required
        );
    }

    #[test]
    fn the_two_large_required_downloads_are_flagged_as_large() {
        // The UI warns before pulling these on mobile data; if a bump makes one
        // small this test is a prompt to re-check the estimate.
        let rootfs = spec(ComponentId::Rootfs);
        let hangover = spec(ComponentId::Hangover);
        assert!(rootfs.approx_bytes > 10 * 1024 * 1024);
        assert!(hangover.approx_bytes > 100 * 1024 * 1024);
    }

    #[test]
    fn required_components_declare_a_sentinel_or_are_handled_specially() {
        for c in catalogue() {
            if c.necessity_android == Necessity::Required && c.id != ComponentId::Vortex {
                assert!(
                    !c.sentinel.is_empty(),
                    "{} needs a sentinel path to verify its install",
                    c.display_name
                );
                assert!(
                    !c.sentinel.starts_with('/'),
                    "{} sentinel must be relative to the guest root",
                    c.display_name
                );
            }
        }
    }
}
