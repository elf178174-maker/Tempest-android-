# Third-party components

Tempest Android is licensed under MIT OR Apache-2.0. It combines several
independent open-source projects. This file records what each one is, where it
comes from, its licence, and **how it reaches the user** — because that last
point is what the licence obligations actually attach to.

Two distribution categories are used below:

- **In the APK** — the file is compiled into or packaged inside the APK that
  GitHub Actions produces, and is therefore distributed by this project.
- **Downloaded at runtime** — the app fetches it from the original publisher,
  on the user's device, at the user's request. This project never redistributes
  it. This is the deliberate choice for anything large, anything whose licence
  is easier to satisfy by pointing at the source, and anything proprietary.

---

## In the APK

### PRoot

- **Upstream**: <https://github.com/termux/proot>
- **Licence**: GPL-2.0-or-later
- **Packaged as**: `lib/arm64-v8a/libproot.so`, `libproot-loader.so`,
  `libproot-loader32.so`
- **Built by**: `scripts/build-proot.sh`, from a pinned upstream tag, during CI.
  The exact revision is written to `proot-revision.txt` alongside the binaries.

PRoot is an **independent program executed as a subprocess**. It is not linked
into Tempest, does not share an address space with it, and communicates only
through the normal process interface. Shipping it in the same APK is *mere
aggregation* under GPL-2.0 section 2, which is the same arrangement every
PRoot-based Android application uses.

GPL-2.0 section 3 requires that the corresponding source be available. It is:
the upstream repository above, at the tag pinned in `scripts/build-proot.sh`,
plus that script itself, which is the complete set of instructions used to
produce the binaries. No patches are applied.

> **Note on the copyleft boundary.** Because PRoot is GPL-2.0 and Tempest is
> MIT/Apache-2.0, the two are kept strictly separate: no PRoot source is copied
> into this repository, no PRoot header is included by any Rust or Kotlin file,
> and the only coupling is a command line. If that boundary were ever crossed —
> by linking, or by vendoring code — the combined work would have to be GPL-2.0.

### Rust crates

The Rust core links a number of crates, all under permissive licences
(MIT, Apache-2.0, ISC, BSD-3-Clause, Zlib, or Unicode-3.0). The authoritative
list with exact versions is `Cargo.lock`; run

```bash
cargo install cargo-about && cargo about generate about.hbs
```

to regenerate a full attribution report. Notable ones:

| Crate | Licence | Used for |
|---|---|---|
| `tokio` | MIT | async runtime |
| `reqwest` | MIT OR Apache-2.0 | HTTP |
| `rustls`, `ring`, `webpki-roots` | Apache-2.0 / ISC / MPL-2.0 (roots) | TLS, with a bundled CA store because Android has no `/etc/ssl/certs` |
| `serde`, `serde_json`, `toml` | MIT OR Apache-2.0 | serialisation |
| `aes-gcm` | MIT OR Apache-2.0 | desktop token encryption |
| `sha2` | MIT OR Apache-2.0 | download verification |
| `zip`, `tar`, `flate2`, `zstd`, `xz2` | MIT / Apache-2.0 / BSD | archive extraction |
| `jni` | MIT OR Apache-2.0 | the Kotlin bridge |
| `url` | MIT OR Apache-2.0 | `vortex://` parsing |

### Android libraries

Jetpack Compose, Material 3, AndroidX lifecycle and activity libraries, and
kotlinx-coroutines / kotlinx-serialization are all **Apache-2.0**. Coil, used for
game artwork, is **Apache-2.0**.

---

## Downloaded at runtime

None of these are redistributed by this project. The app fetches each from its
original publisher, verifies it against a pinned SHA-256 where upstream provides
a stable URL, and records what it installed in Settings → Runtime.

### Ubuntu Base 24.04 (arm64)

- **Upstream**: <https://cdimage.ubuntu.com/ubuntu-base/releases/24.04/release/>
- **Licence**: a whole distribution; per-package licences are in
  `/usr/share/doc` inside the image. Ubuntu Base is freely redistributable.
- **Why**: Wine is a glibc program and Android's C library is Bionic, so a small
  Linux userland has to exist for Wine to run inside.
- **Verified**: SHA-256 pinned from Ubuntu's published `SHA256SUMS`.

### Hangover

- **Upstream**: <https://github.com/AndreRH/hangover>
- **Contains**:
  - **Wine**, built for ARM64 — **LGPL-2.1-or-later**
  - **FEX** (`libarm64ecfex.dll`, `libwow64fex.dll`) — **MIT**
  - **Box64** (`wowbox64.dll`) — **MIT**
  - **DXVK**, built as native ARM64 PE — **Zlib**
- **Why**: Wine that runs natively on ARM64, plus the emulators that run x86-64
  and x86 Windows code inside it.
- **Verified**: SHA-256 pinned against the published release asset.

Wine's LGPL-2.1 would permit bundling, but downloading keeps the APK small and
means the user receives Wine exactly as its own project published it.

### Mesa and the Vulkan loader

- **Upstream**: <https://gitlab.freedesktop.org/mesa/mesa>,
  <https://github.com/KhronosGroup/Vulkan-Loader>
- **Licence**: MIT (Mesa), Apache-2.0 (loader)
- **Installed with**: `apt-get` inside the container, from Ubuntu's own
  repositories, so the dependency graph is resolved correctly.
- **Why**: provides `libvulkan.so.1` and lavapipe, the software Vulkan
  implementation used as a fallback and a diagnostic.

### DXVK (standalone)

- **Upstream**: <https://github.com/doitsujin/dxvk> — **Zlib**
- Optional. Only needed to override the DXVK that Hangover already ships.

### vkd3d-proton

- **Upstream**: <https://github.com/HansKristian-Work/vkd3d-proton> —
  **LGPL-2.1-or-later**
- Optional, and off by default. Vortex itself does not need Direct3D 12.

### The Vortex client

- **Source**: `https://playvortex.io/download/windows`
- **Licence**: proprietary. **Not redistributed by this project.** It is
  downloaded on the user's device, using the user's own authenticated session,
  exactly as the Vortex website would deliver it.

---

## Companion apps, not bundled

### Termux:X11

- **Upstream**: <https://github.com/termux/termux-x11>
- **Licence**: GPL-3.0
- **How it is used**: installed separately by the user, from its own publisher.
  Tempest connects to it as an X client over the display socket.

It is deliberately *not* bundled. Keeping it a separate application avoids any
question about combining GPL-3.0 code with this project's licence, and leaves it
maintained by the people who specialise in it.

---

## Upstream Tempest

This project is a fork of <https://github.com/solomon-gleeson/tempest>
(**MIT OR Apache-2.0**), which its author archived. `LICENSE-MIT` and
`LICENSE-APACHE` are carried forward unchanged, and the Vortex API behaviour,
DXVK/vkd3d handling and Wine noise filter are derived from that work.

---

## Trademarks

Vortex and playvortex.io are the property of their respective owners. This is an
independent, community-developed project with no affiliation to, endorsement by,
or connection with the developers or operators of Vortex.
