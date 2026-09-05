# Porting Tempest to Android

This document is in two halves. The first is an audit of the upstream project —
what it does, and every place it assumes a Linux desktop. The second is the plan
that audit produced, and a record of what has actually been built.

Upstream: <https://github.com/solomon-gleeson/tempest> (archived by its author).

---

# Part 1 — Audit of the upstream project

## What Tempest is

A single Rust binary, 2,313 lines across 16 files, that makes the Windows Vortex
client run on Linux desktops. It is a *launcher*, not a game: it installs Wine,
creates a prefix, drops DXVK and vkd3d-proton into it, downloads `Vortex.exe`,
registers the `vortex://` URI scheme with the desktop, authenticates against
`playvortex.io`, and runs `wine Vortex.exe vortex://play?game=N&token=…`.

```
src/main.rs        87   clap CLI, tokio entry point
src/config.rs     184   config.toml, encrypted session token
src/auth.rs       134   login, play-page scraping
src/crypto.rs      80   AES-256-GCM over a key file
src/games.rs       88   game discovery
src/uri.rs         98   vortex:// parse + .desktop registration
src/launcher/     285   command construction, process supervision
src/setup/        579   distro detection, Wine install, DXVK, vkd3d
src/doctor.rs     360   diagnostics
src/plugin.rs     228   two optional C plugins, compiled on the host
src/updater.rs     97   Vortex.exe download and zip extraction
src/logger.rs      93   file logging
```

## Subsystem by subsystem

### Authentication (`src/auth.rs`)

A form POST to `https://playvortex.io/login` with `username`, `password`,
`fingerprint` and `fp_token` (the latter two empty). Redirects are disabled,
because the `session_token` cookie rides on the 302 itself. The token is then
stored in `config.toml`, encrypted.

The credential prompt is terminal-only: `libc::tcgetattr`/`tcsetattr` to clear
`ECHO`, then `stdin().read_line()`.

### Vortex API (`src/auth.rs`, `src/games.rs`)

Three endpoints, all authenticated with a `Cookie: session_token=…` header:

| endpoint | use |
|---|---|
| `POST /login` | obtain the session cookie |
| `GET /api/games/{id}` | JSON metadata for one game |
| `GET /games/{id}/play` | HTML page embedding the `vortex://` launch link |
| `GET /download/windows` | the Vortex client, as a zip |

There is no "list my games" endpoint. Discovery walks ids upward from 1 and
stops at the first response that is not a game.

### `vortex://` handling (`src/uri.rs`)

`parse_vortex_uri` accepts any URL whose scheme is `vortex` and pulls `game`
(parsed as `u32`) and `token` (any string) out of the query. Registration writes
`~/.local/share/applications/tempest-vortex.desktop` and then shells out to
`xdg-mime`, `gio` and `update-desktop-database`.

### Wine launching (`src/launcher/mod.rs`)

Builds a `std::process::Command` for `wine` (or `gamemoderun wine`), sets
`WINEPREFIX`, `WINEESYNC`, `WINEFSYNC`, `VKD3D_SHADER_CACHE_PATH` and any
user-configured variables, then runs `wine <vortex_exe> <uri>`. stdout and
stderr are read on two threads and filtered against a list of ten known-noisy
substrings. A `ctrlc` handler sends `SIGTERM` to the child.

### DXVK and vkd3d-proton (`src/setup/dxvk.rs`, `vkd3d.rs`, `dll.rs`)

Resolves the newest GitHub release, downloads the tarball, unpacks it, copies
the DLLs into `drive_c/windows/system32` and `syswow64`, and sets
`HKCU\Software\Wine\DllOverrides` with one `wine reg add` per DLL. PE files are
recognised by shelling out to `file(1)`, falling back to an `MZ` magic check.

### `receiver.exe` (`src/launcher/process.rs`)

A second Windows executable that ships beside `Vortex.exe`. Tempest starts it
under Wine before launching a game and kills it afterwards; the code comments it
as providing "in-game notifications". Whether it is already running is decided
by `pgrep -f receiver.exe`.

### Configuration (`src/config.rs`)

`~/.config/tempest/config.toml`, holding the encrypted session token, three
absolute paths, the Wine binary name and environment, and six launcher booleans.

### Process management

Everything is `std::process::Command`. `setup` builds shell command strings with
`format!` and runs them through `sh -c`. Privilege escalation is `sudo` in those
strings, with `libc::getuid() == 0` deciding whether to strip it.

## Every Linux-desktop assumption, and what it costs on Android

| # | Assumption | Where | Consequence on Android |
|---|---|---|---|
| 1 | `dirs::config_dir()`, `data_local_dir()`, `cache_dir()` | `config.rs`, `crypto.rs`, `launcher/mod.rs`, `plugin.rs` | Returns paths under a `$HOME` that does not exist. |
| 2 | Literal `~/.config`, `~/.local/share` fallbacks | `config.rs:79,85` | Written to `std::fs` verbatim — a directory literally named `~`. |
| 3 | `$HOME` | `launcher/mod.rs:55` | Unset. |
| 4 | `/etc/os-release` | `setup/mod.rs:34` | Absent. |
| 5 | `sudo` + `apt`/`dnf`/`pacman`/`zypper` | `setup/mod.rs:135-168`, `doctor.rs:307-359` | No package manager, no root, no meaningful advice to give. |
| 6 | `sh -c` with interpolated strings | `setup/mod.rs:174-187` | Command injection risk, and there is nothing to install anyway. |
| 7 | `pgrep -f receiver.exe` | `launcher/process.rs:73` | Not present; and since API 29 `/proc` shows only your own processes. |
| 8 | `which::which("wine")` | `setup`, `doctor` | No `PATH` with Wine on it. |
| 9 | `xdg-mime`, `gio`, `update-desktop-database`, `.desktop` files | `uri.rs:22-63` | Meaningless. Deep links come from the manifest. |
| 10 | `vulkaninfo`, `glxinfo` on `PATH` | `doctor.rs:87` | Not installed. |
| 11 | `/dev/nvidia0`, `/usr/share/vulkan/icd.d` | `doctor.rs:117,303` | Wrong vendor, wrong paths. |
| 12 | `file(1)` for PE detection | `setup/dll.rs:9` | Not in the Android toolbox. |
| 13 | `cc` on `PATH` to build plugins | `plugin.rs:60,102` | No compiler on a phone. |
| 14 | `gamemoderun` | `launcher/mod.rs:38` | Linux-desktop daemon. |
| 15 | Terminal prompts (`tcsetattr`, stdin) | `auth.rs:103-134`, `setup/mod.rs:189` | No terminal; the app would block forever. |
| 16 | `colored`, `indicatif` writing to a TTY | throughout | Output goes nowhere. |
| 17 | stdout/stderr as the user interface | throughout | Nobody sees it. |
| 18 | `std::env::temp_dir()` | `plugin.rs:50,92` | `/tmp` is not app-writable. |
| 19 | **x86-64 Wine and x86-64 Windows binaries** | the entire premise | The phone is ARM64. |
| 20 | **Executing a downloaded binary** | implied by all of the above | Forbidden since API 29. |

Items 19 and 20 are the ones that matter. The rest are a day of mechanical work.

## What actually prevents an `aarch64-linux-android` build

Compiling the crate for the target is the easy part. The dependency set is
almost entirely portable: `reqwest` needs `rustls` with bundled roots rather
than system roots (Android has no `/etc/ssl/certs`), and `zstd`/`xz2`/`ring`
need the NDK's clang, which `cargo-ndk` supplies. `which`, `colored`,
`indicatif`, `ctrlc` and `clap` compile but are useless.

The real blockers are behavioural, not compilational:

**A. Bionic is not glibc.** Wine is a glibc program. Android's C library is
Bionic. A glibc binary cannot be loaded by Android's dynamic linker at all.

**B. W^X.** An app targeting API 29 or later cannot `execve()` a file in its own
data directory. The SELinux policy for `untrusted_app_29` and later drops
`execute_no_trans` on `app_data_file`. Only `nativeLibraryDir` — populated by
the package installer from the APK, and read-only — remains executable. So
"download Wine and run it" is not a strategy; it is a permission error.

**C. The instruction set.** `Vortex.exe` and essentially every Windows game are
x86 or x86-64. The phone is ARM64.

**D. There is no display.** Wine's `winex11.drv` needs an X server. Android does
not have one.

---

# Part 2 — The plan, and what has been built

## Structure

The core is now a library with the platform-dependent decisions behind traits,
so the same code drives the desktop CLI and the Android app.

```
                        tempest-core
                             │
        ┌────────────────────┴────────────────────┐
        │                                         │
  LinuxPlatform                            AndroidPlatform
  XDG directories                          paths injected from Context
  unrestricted exec                        exec only from nativeLibraryDir
  encrypted key file                       Android Keystore
  .desktop registration                    manifest intent filter
        │                                         │
  tempest (CLI)                            tempest-jni → Kotlin app
```

The four traits are in `crates/tempest-core/src/platform/`:

- **`TempestPaths`** — every location, derived from one injected root. Nothing
  in the core may call `dirs::*`, read `$HOME`, or join a literal `/usr`,
  `~/.config` or `/data/user/0/…`; package data paths differ between users,
  work profiles, Android versions and OEM builds.
- **`ProcessBackend`** — spawn, observe, terminate. Carries an *exec policy*:
  the Android implementation refuses any path outside `nativeLibraryDir` with an
  explanation, rather than letting the caller see a bare `EACCES`.
- **`SecretStore`** — the session token at rest.
- **`Platform`** — the three above plus device facts and URI registration.

## The compatibility stack

Four techniques were considered for running Windows x86-64 code on ARM64
Android.

| Approach | Verdict |
|---|---|
| x86-64 Wine under Box64 | Works (this is classic Winlator), but *everything* is emulated, including Wine itself and DXVK. Slowest of the options. |
| Native ARM64 Wine + WoW64 emulation | Only the Windows PE code is emulated; Wine, the Vulkan driver and DXVK run natively. **Chosen.** |
| FEX-Emu running a whole x86-64 rootfs | Heavier, and needs a second full userland. |
| Writing our own translation layer | Explicitly out of scope, and would be worse than any of the above. |

The chosen chain:

```
Android ARM64 app process
  └─ PRoot                     unprivileged chroot; ships in the APK because it
     (GPL-2.0, in the APK)     is the only thing allowed to be executed
      └─ Ubuntu Base 24.04 arm64 (glibc userland, in app storage)
          └─ Hangover: Wine built natively for ARM64  (LGPL-2.1+)
              └─ FEX (libarm64ecfex.dll / libwow64fex.dll)  (MIT)
                 or Box64 (wowbox64.dll)                    (MIT)
                  └─ Vortex.exe, then the game (x86-64 PE)
                      └─ DXVK, built as native ARM64 PE     (Zlib)
                          └─ Vulkan
                              └─ Mesa Turnip (Adreno) or lavapipe (software)
```

### Why PRoot, specifically

This is the part that makes the whole thing legal on modern Android. PRoot
`ptrace()`s its children and rewrites path-related syscalls so the extracted
Ubuntu tree appears at `/`. Crucially, when a guest program is started **PRoot
does not hand the guest binary to `execve()`**. It `execve()`s its own small
loader — which lives in `nativeLibraryDir` and is therefore executable — and the
loader maps the guest ELF itself. Mapping executable pages out of app data is
exactly what `dlopen()` does and is permitted; only `execve()` is not.

So the design works *within* the platform's rule rather than trying to defeat
it. Three files ship in the APK, named `lib*.so` so the installer extracts them:
`libproot.so`, `libproot-loader.so`, `libproot-loader32.so`. They are built from
source in CI by `scripts/build-proot.sh`.

### Why Hangover

[Hangover](https://github.com/AndreRH/hangover) publishes, in one archive per
release, exactly the pieces this needs:

```
hangover-wine_11.16~noble_arm64.deb        Wine, built for ARM64
hangover-libarm64ecfex_11.16_arm64.deb     FEX, as the ARM64EC emulator (x86-64)
hangover-libwow64fex_11.16_arm64.deb       FEX, as the WoW64 emulator (x86)
hangover-wowbox64_11.16_arm64.deb          Box64, as an alternative emulator
dxvk-v2.7.1.tar.gz                         DXVK, with aarch64 and arm64ec builds
```

That last line is why this approach wins. DXVK compiled as a **native ARM64 PE**
means the Direct3D-to-Vulkan translation is not itself emulated — only the
game's own code is. Under Box64-everything, every DXVK call would pay the
translation cost too.

The `_ubuntu2404_noble_` build pairs exactly with Ubuntu Base 24.04, which is
published with a signed `SHA256SUMS` file.

### Component installation

Downloads are pinned by URL and SHA-256 where upstream publishes a stable one
(`crates/tempest-core/src/runtime/manifest.rs`), streamed to a `.part` file,
hashed, and only then moved into place.

Wine's dependency graph is *not* hand-resolved. The `.deb` packages are unpacked
into the guest and installed with `apt-get`, inside PRoot — hand-picking
transitive dependencies is how you end up with a library that fails to load at
runtime with a confusing error. Maintainer scripts from downloaded packages are
never executed outside the container.

### The display

Wine needs an X server. Rather than bundling one, Tempest connects to
**Termux:X11**, a mature open-source X server for Android that renders to a
`SurfaceView`. It stays a separate app the user installs: it is GPL-3.0, and
keeping it separate avoids any licence question about the APK while giving the
user a component that is maintained by people who specialise in it.

`DISPLAY` defaults to `:0` and is configurable.

## Known blocker: hardware Vulkan on Adreno

**This is the one part that is not solved, and it is documented rather than
hidden.**

Wine's `winevulkan` needs a *glibc* `libvulkan.so.1` inside the container.
Android's own Vulkan driver is a Bionic library and cannot be loaded by a glibc
process. Two options exist:

1. **lavapipe** (`libvulkan_lvp.so`, in Ubuntu's `mesa-vulkan-drivers`). Pure
   software rendering. Far too slow to play anything, but it runs anywhere and
   is the right tool for answering "is the rest of the chain working?". This is
   installed by the Mesa component and is selectable in Settings → Graphics.

2. **Mesa Turnip** — the open-source Adreno driver. Turnip supports two kernel
   backends: DRM (`msm`), used on Linux, and KGSL, used on Android. Ubuntu's
   stock build enables only `msm`, so it will not find the GPU through
   `/dev/kgsl-3d0`. A Turnip build with `-Dfreedreno-kmds=kgsl` is required.
   That build is not published by an upstream this project can pin, so it is
   currently a user-supplied component.

The diagnostics screen probes `vulkaninfo` inside the container and reports
which of these is in play, rather than letting a game fail with an unexplained
crash.

## Verifying the riskiest part without a game

The single mechanism everything else rests on is PRoot successfully ptrace-ing a
child, substituting its own loader, and mapping a guest ELF out of app storage.
Diagnostics therefore has a dedicated **Linux container** check that runs
`uname -m` inside the guest — no Wine, no Vulkan, no X server, no Vortex
account. It isolates that one question, so a device report can distinguish "the
container does not work here" from "Wine crashed" from "there is no GPU driver".

## Security decisions

- `vortex://` links are validated field by field and then **re-serialised** from
  the validated parts. What reaches Wine is built by Tempest, never echoed from
  the caller. Any exported Android component can be sent an Intent, so a link is
  untrusted input. Tokens containing whitespace, quotes or shell metacharacters
  are rejected outright.
- No shell string is ever built by interpolation. `ProcessSpec` takes argv as a
  vector; where a shell is genuinely needed, the script is a fixed template in
  this crate and variable values are passed as positional parameters.
- Archive extraction rejects entries that are absolute, contain `..`, or whose
  symlink targets escape the destination; device nodes and FIFOs are skipped;
  setuid bits are stripped.
- Every log line passes through a redactor covering `token=`, `session_token=`,
  cookies, `Authorization:` headers and JSON password fields — including lines
  that come from Wine's own output, which echoes the command line it was given.
- The session token never crosses back into Kotlin. `nativeParseUri` returns the
  game id and a redacted display string, so the token cannot reach a `Bundle`, a
  log, or a crash report.

## Bugs found in the upstream code, and fixed

Working through the port surfaced these; each has a regression test.

| Bug | Effect |
|---|---|
| `logger.rs` computed the date as `1970 + days/365` | Log timestamps drifted about a day per leap year. |
| `crypto.rs` used `.expect("encryption failed")` | An allocation failure would abort the whole launcher. |
| `config.rs` used `unwrap_or_default()` on a parse error | A single typo in `config.toml` silently reset every setting. |
| `games.rs` stopped at the first missing id | One delisted game hid every game after it. |
| `reqwest` errors were formatted with the URL included | Play-link tokens could reach the log. |
| `set_dll_override` ran `wine reg add` once per DLL | Seven guest processes where one `regedit` import does. |
| `install_dll` re-backed-up on every run | The second install overwrote the original Wine DLL with the first install's DXVK copy. |
| `doctor` called `register_uri_handler()` | Running diagnostics silently rewrote the user's desktop registration. |

## Milestones

| # | Milestone | State |
|---|---|---|
| 1 | Understand and build upstream | Done — audited above; the CLI still builds and keeps its command surface. |
| 2 | Android project and CI | Done — `.github/workflows/android.yml` produces `tempest-android-debug.apk`. |
| 3 | Port the core | Done — workspace with the platform abstraction; 162 Rust tests. |
| 4 | Android filesystem and config | Done — all paths injected from `Context`. |
| 5 | Vortex auth and game discovery | Done — same wire protocol, Keystore storage, concurrent walk with caching. |
| 6 | `vortex://` on Android | Done — intent filter, validating parser, paste fallback. |
| 7 | Runtime management | Done — pinned, checksummed catalogue with progress and provenance. |
| 8 | Wine + x86 translation | Implemented — needs device verification. |
| 9 | DXVK / Vulkan | Implemented — hardware Vulkan blocked on a Turnip KGSL build; software fallback works. |
| 10 | Launch `Vortex.exe` | Implemented — needs device verification. |
| 11 | Launch a game | Needs device verification. |
| 12 | Stability | Ongoing. |

Milestones 8, 10 and 11 cannot be verified in CI: they need a real ARM64 phone
with a GPU. See `docs/TROUBLESHOOTING.md` for what to send back.

## What the tests cover

190 automated tests, none of which assert `true`:

| Where | Count | What they pin down |
|---|---|---|
| `tempest-core` | 155 | URI validation (including hostile tokens), log redaction, path derivation, config round-trips, archive traversal and symlink escapes, PE inspection, the process backend's exec policy, the component catalogue, session failure explanations, diagnostics |
| `tempest-jni` | 4 | The JSON envelope, including that every error kind survives the crossing and that a message containing quotes or newlines stays valid JSON |
| `tempest` (CLI) | 3 | Plugin name sanitisation and compiler diagnostics |
| Android JVM | 20 | The wire format between Rust and Kotlin — every model, decoded from the literal JSON the core emits — plus UI filtering |
| Android instrumentation | 8 | The Keystore round trip, that the token is not stored in the clear, that `vortex://` resolves to this app, and that the native libraries are really in the installed APK |

The CI workflow additionally verifies the built APK contains all four native
libraries, because a build that silently drops them installs fine and then fails
on first launch.
