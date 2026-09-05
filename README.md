# Tempest Android

An **unofficial** Android port of [Tempest](https://github.com/solomon-gleeson/tempest),
a community-built launcher for Vortex. It signs you in to your Vortex account,
lists your games, installs a Windows compatibility runtime into the app's own
storage, and launches games through it.

This project is not affiliated with, endorsed by, or connected to the developers
or operators of Vortex or playvortex.io.

> **Read this before you start.** Running Windows games on an Android phone means
> stacking a Linux container, an ARM64 build of Wine, an x86-to-ARM translator
> and a Vulkan driver on top of each other. Sign-in, the game list, deep links
> and runtime installation all work. Whether a *particular* game runs on *your*
> device is not something anyone can promise, and one known blocker remains —
> see [Current status](#current-status).

---

## What it does

- Sign in with your normal Vortex username and password
- Browse and search your games
- Download and install the compatibility runtime, with progress and checksums
- Handle `vortex://` links from the Vortex website, or from a pasted link
- Launch games, and keep them running when you switch apps
- Diagnose what is wrong when something fails, in specific terms
- Copy a log that has been stripped of anything sensitive

Nothing is faked. There is no mock login, no placeholder game list, and no
pretend launch: every screen is driven by the same Rust core the desktop CLI
uses, talking to the real Vortex API.

---

## Getting the APK

You do **not** need Android Studio, the Android SDK, Gradle, Java or Rust.
GitHub Actions builds everything.

1. Open the [**Actions**](../../actions) tab of this repository.
2. Click the most recent successful **Android** run.
3. Scroll to **Artifacts** and download **`tempest-android-debug`**.
4. Unzip it. Inside is **`tempest-android-debug.apk`**.

There is also a versioned copy, `tempest-android-<version>-<commit>-debug.apk`,
so you can tell two builds apart.

To build from a change you pushed: push to any branch and the workflow runs
automatically. To get an unsigned release APK as well, start the workflow
manually from the Actions tab with **Also build an unsigned release APK** ticked.

---

## Installing it

The APK is signed with Android's standard debug key, so your phone will treat it
as coming from an unknown source.

1. Copy the `.apk` to your phone, or download it there directly.
2. Open it with the Files app.
3. Android will ask whether to allow installs from this source. Allow it.
4. Install, and open Tempest.

Requirements: **Android 10 or newer**, a **64-bit ARM** device, and a working
**Vulkan** driver. Any recent Snapdragon phone qualifies.

If installation is refused, see
[docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md).

---

## Signing in

Tap **Sign in** and enter your normal Vortex username and password. They go
straight to `playvortex.io` — the same request the desktop client makes.

What Tempest keeps afterwards is the session cookie Vortex returns, encrypted
with an AES-256-GCM key generated inside your phone's hardware keystore. The key
never leaves the keystore, and the password is never stored at all. Nothing is
written to a config file, and the token is stripped from every log line.

---

## Installing the runtime

Go to **Runtime** and tap **Install everything required**. This downloads roughly
400 MB, so use Wi-Fi.

Four components are installed:

| Component | Size | What it is for |
|---|---|---|
| Ubuntu Base 24.04 | ~30 MB | Wine is a glibc program; Android's C library is not glibc, so a small Linux userland has to exist for Wine to live in. |
| Hangover | ~293 MB | Wine built for ARM64, plus the FEX and Box64 emulators that run x86 Windows code, plus a DXVK compiled for ARM64. |
| Mesa and the Vulkan loader | ~120 MB | The Vulkan library Wine needs, plus a software renderer as a fallback. |
| Vortex client | ~80 MB | The Windows Vortex client itself, from playvortex.io. |

Each one shows where it came from, its licence, and the SHA-256 of what was
actually installed. Downloads are verified before anything is unpacked.

### You also need an X server

Wine draws through X11, which Android does not have. Install
**[Termux:X11](https://github.com/termux/termux-x11/releases)** — a separate,
open-source app — open it, and leave it running in the background before you
launch a game.

It is a separate app on purpose: it is GPL-3.0, and it is better maintained by
people who specialise in it than it would be if this project reimplemented it.

---

## Launching a game

From the **Games** list, tap **Play**.

From the Vortex website, tapping Play should open Tempest directly through the
`vortex://` link. Some browsers refuse to hand custom schemes to an app; if that
happens, copy the link and paste it into **Session → Open a Vortex link**.

While a game runs, Tempest shows a notification and keeps a foreground service
alive, so switching to Termux:X11 to actually see the game does not get the game
killed.

---

## Current status

| Works | |
|---|---|
| Building the APK in CI, from a clean checkout | ✅ |
| Signing in to Vortex | ✅ |
| Listing and searching games | ✅ |
| `vortex://` deep links, and pasted links | ✅ |
| Downloading and verifying runtime components | ✅ |
| Creating the Wine prefix and installing DXVK | ✅ |
| Keeping a session alive in the background | ✅ |
| Diagnostics and redacted log export | ✅ |

| Needs a real device to confirm | |
|---|---|
| `Vortex.exe` starting under the compatibility stack | ⏳ |
| A game actually running | ⏳ |

**One known blocker: hardware Vulkan on Adreno.** Wine needs a *glibc* Vulkan
driver inside the container, and Android's own driver is a Bionic library that a
glibc process cannot load. Ubuntu's Mesa provides **lavapipe**, a software
renderer — fine for proving the stack works, far too slow to play on. Hardware
acceleration needs a **Mesa Turnip build with the KGSL backend**, which no
upstream publishes in a form this project can pin. It is documented in full in
[docs/ANDROID_PORT.md](docs/ANDROID_PORT.md#known-blocker-hardware-vulkan-on-adreno)
rather than papered over.

If you run this on a device, the **Diagnostics** screen and **Copy logs** button
produce exactly what is needed to move the remaining items forward. Both are
already stripped of credentials.

---

## Documentation

- [docs/ANDROID_PORT.md](docs/ANDROID_PORT.md) — the audit of upstream, every
  Linux assumption found, and the architecture that replaced them
- [docs/BUILDING.md](docs/BUILDING.md) — how CI builds this, and how to build
  locally if you want to
- [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) — what to do when something
  fails, and what to send back
- [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md) — every component, its
  licence, and how it reaches you

---

## Licence

MIT OR Apache-2.0, at your option — the same as upstream Tempest.
See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).

Components this project ships or downloads carry their own licences; see
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md).
