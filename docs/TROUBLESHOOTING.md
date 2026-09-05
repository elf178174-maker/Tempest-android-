# Troubleshooting

Start with **Settings → Run diagnostics**. Every check that fails names what was
tested, what was found, and what to do about it. If you are reporting a problem,
that screen plus **Settings → Logs → Copy logs** is what to send — both are
already stripped of your session token.

---

## Installing

### "App not installed" or "Package appears to be invalid"

- **Wrong architecture.** The APK contains `arm64-v8a` only. Check
  Settings → About phone; if your device is 32-bit ARM or x86, it cannot run
  this — and would not be able to run the compatibility stack anyway.
- **Android too old.** Android 10 or newer is required. See
  [why](BUILDING.md#why-minsdk-is-29).
- **A different signature is already installed.** If you previously installed a
  build from elsewhere, uninstall it first. Debug builds use the package id
  `io.tempest.android.debug`, so they can sit alongside a release build.

### "Blocked by Play Protect"

Expected for an app signed with a debug key and installed from a file. Tap
**More details → Install anyway**.

---

## Starting up

### "The native Tempest library is missing from this build"

The APK was built without `libtempest_jni.so`. Download the artifact from a
successful **Android** workflow run rather than building by hand; CI verifies the
native libraries are present before uploading.

### The app opens to an error card instead of the sign-in screen

The Rust core could not start. The message says why. Most often the app's data
directory is unwritable because the device is completely out of storage.

---

## Signing in

### "Incorrect username or password"

That is Vortex's own answer. Check the credentials on the website first.

### "The server accepted the login but did not return a session cookie"

Vortex changed its login flow. Nothing on the device can fix this; it needs a
change to `crates/tempest-core/src/auth.rs`. Please open an issue.

### "The secure credential store could not be used"

The Android Keystore destroyed the encryption key. This happens when the device
screen lock is changed or removed — it is the keystore working as designed.
Sign in again; a new key is created.

### Signed in, but "your Vortex session has expired" later

Session cookies do expire. Sign out and back in from **Settings → Account**.

---

## The game list

### Empty, with no error

The account has no games, or the walk found none. Tempest discovers games by
querying ids upward, because Vortex has no "list my games" endpoint. Pull to
refresh, and check **Logs** for which ids were probed.

### Slow to load

Every id is a separate HTTPS request, batched eight at a time. The result is
cached, so it is instant afterwards and works offline.

---

## Installing the runtime

### A download fails partway

Tap Install again. Downloads resume by re-verifying: a file that already matches
its pinned SHA-256 is reused rather than re-fetched.

### "Integrity check failed"

The downloaded file did not match its expected SHA-256. **Do not work around
this.** It means either a corrupted transfer, or something between you and the
publisher altered the file. Retry on a different network. If it persists, open
an issue — the pinned digest may need updating after an upstream re-release.

### "Not enough space"

The full runtime needs roughly 1.5 GB unpacked. Free space, or move game storage
to a removable volume in **Settings → Storage**. Note that Tempest must be
restarted for a storage change to take effect; the app says so when you change
it.

### Hangover installation fails while "installing Wine"

`apt` inside the container could not resolve Wine's dependencies. Almost always
a network problem inside the container. Check **Logs** for the `apt` output. If
DNS is failing, the container uses `1.1.1.1` and `8.8.8.8`; a network that blocks
those will break it.

---

## Launching

### "These still need to be installed: …"

Exactly what it says. **Settings → Runtime → Install everything required**.

### "PRoot is not in the APK"

A broken build. Install an APK from a successful CI run.

### "Wine could not reach an X server"

The most common failure, and the easiest to fix.

Wine draws through X11, which Android does not have. Install
**[Termux:X11](https://github.com/termux/termux-x11/releases)**, open it, and
leave it running in the background *before* launching a game. Then try again.

If Termux:X11 is running and this still appears, check that
**Settings → Graphics → display** is `:0`.

### "No usable Vulkan device was found in the container"

This is the known blocker. See
[docs/ANDROID_PORT.md](ANDROID_PORT.md#known-blocker-hardware-vulkan-on-adreno)
for the full explanation.

Short version: Wine needs a *glibc* Vulkan driver inside the container, and
Android's own driver is a Bionic library that a glibc process cannot load.

- To confirm the rest of the stack works, set
  **Settings → Graphics → Vulkan driver** to **Lavapipe**. That is software
  rendering: it will be far too slow to play, but if Vortex's window appears,
  everything except hardware acceleration is functioning.
- Hardware acceleration on an Adreno GPU needs a Mesa Turnip build with the
  KGSL backend. Ubuntu's stock Mesa enables only the DRM backend and will not
  find the GPU.

### "The x86 translation layer failed to initialise"

Reinstall Hangover from **Settings → Runtime**. If it recurs, try switching the
emulator: FEX is the default; Hangover also ships Box64 as `wowbox64.dll`. Set
`HODLL=wowbox64.dll` under Wine environment variables.

### "A Direct3D library failed to load"

Reinstall DXVK from **Settings → Runtime**, or turn DXVK off in
**Settings → Graphics** to see whether the game starts with Wine's own
Direct3D — much slower, but a useful test.

### Exit code 126

A binary could not be executed. On Android this means something tried to
`execve()` a file outside `nativeLibraryDir`, which the platform forbids. It is a
bug in Tempest; please report it with the log.

### Exit code 127

A program or library was not found inside the container — the runtime install is
incomplete. Remove the Ubuntu base image from **Settings → Runtime** and install
everything again.

### "Killed by SIGKILL"

Android's low-memory killer reclaimed the process. Close other apps. Make sure
**Settings → Performance → Keep games running in the background** is on, and that
Tempest is exempt from battery optimisation in Android's own settings.

### The game dies as soon as you switch apps

The foreground service is not running. Grant the notification permission — the
service needs a visible notification to be allowed to keep running — and check
that **Keep games running in the background** is on.

---

## Performance

- **fsync** is faster than esync but needs `futex_waitv` in the kernel. If games
  stop launching after enabling it, turn it back off.
- **Shader caching** makes second and later launches much faster. Leave it on.
- **The FPS overlay** (Settings → Graphics) is the quickest way to tell whether
  you are on hardware or software rendering. Single-digit numbers mean lavapipe.

---

## Reporting a problem

Please include:

1. **Settings → Run diagnostics**, copied.
2. **Settings → Logs → Copy logs**.
3. Your device model and Android version (the diagnostics output has both).
4. What you did, what you expected, and what happened.

Both exports are redacted: `token=`, `session_token=`, cookies, `Authorization`
headers and JSON password fields are masked before anything reaches the log,
including in Wine's own output — which does echo the command line it was given.
It is safe to paste them into an issue.

If a game gets as far as starting and then fails, the **last 40 lines of guest
output** on the Session screen are usually the most informative part.
