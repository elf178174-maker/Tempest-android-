# Building

## You do not need a local Android setup

**Android Studio is not required. Neither is the Android SDK, the NDK, Gradle,
Java or Rust.** GitHub Actions installs and configures all of it.

To get an APK, all you need on your own machine is git and a browser:

1. Push a commit to any branch.
2. Open the **Actions** tab and wait for the **Android** workflow.
3. Download the **`tempest-android-debug`** artifact.
4. Install `tempest-android-debug.apk` on your phone.

That is the entire supported workflow. Everything below is optional.

---

## What CI does

`.github/workflows/android.yml` runs two jobs.

### `rust-tests`

Runs first so a logic error is reported in about a minute, rather than after a
ten-minute APK build.

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- builds the desktop CLI and uploads it as `tempest-linux-cli`

### `android`

1. JDK 17 and the Android SDK, via `android-actions/setup-android`.
2. NDK `27.2.12479018`, platform 35, build-tools 35.0.0.
3. Rust `1.88.0` with the `aarch64-linux-android` target, plus `cargo-ndk`.
4. **Builds the Rust core** as `libtempest_jni.so` into
   `android/app/src/main/jniLibs/arm64-v8a/`.
5. **Builds PRoot from source** into the same directory, as `libproot.so`,
   `libproot-loader.so` and `libproot-loader32.so`. This also builds talloc,
   PRoot's one dependency, statically — see below.
6. Gradle 8.11.1: `testDebugUnitTest`, `lintDebug`, `assembleDebug`.
7. **Verifies the APK actually contains all four native libraries.** A build
   that silently drops them produces an APK that installs and then fails on
   first launch, so this is checked rather than assumed.
8. Uploads `tempest-android-debug`.

### Why PRoot is built rather than downloaded

Nobody publishes a PRoot binary for Android that this project could pin and
verify, and PRoot is the one component that *must* be inside the APK — it is
the only thing Android will let the app execute. So it is built from a pinned
upstream tag, in the open, by `scripts/build-proot.sh`.

Two details in that script matter:

- **`PROOT_UNBUNDLE_LOADER`.** By default PRoot embeds its ELF loader in its own
  binary and extracts it to a temporary file at runtime. On Android that file
  would land in app data and be unexecutable. Setting this makes PRoot read
  `PROOT_LOADER` and `PROOT_LOADER_32` from the environment instead, so the
  loader can live in `nativeLibraryDir` where it can actually be run.
- **Static talloc.** PRoot links against Samba's talloc, whose waf build does
  not cross-compile cleanly for Android. `talloc.c` is compiled directly against
  a small stand-in for libreplace and linked statically, because Android only
  extracts files matching `lib*.so` and a shared `libtalloc.so.2` could not be
  shipped under its own SONAME.

The script verifies its own output: every binary must be aarch64, and
`libproot.so` must have no dynamic dependency on talloc.

### Two kinds of native library

These are not interchangeable, and the distinction is the whole reason the app
works on modern Android:

| File | How it is used |
|---|---|
| `libtempest_jni.so` | **Loaded** with `System.loadLibrary` |
| `libproot*.so` | **Executed** as a program |

Both must be in `nativeLibraryDir`, because an app targeting API 29 or later may
not `execve()` anything in its own data directory. `libproot*.so` is not really a
shared library at all — it is an executable, named `lib*.so` so the package
installer extracts it into the one directory Android permits execution from.
`android:extractNativeLibs="true"` and `useLegacyPackaging = true` exist for the
same reason: a library left compressed inside the APK can be mapped but not run.

### No secrets

The build needs no configured secret of any kind. The debug APK is signed with
AGP's auto-generated debug key; the release APK is left unsigned. Nothing in
this repository reads a token, a keystore or an API key, and nothing should be
added that does — authentication happens on the user's device, against Vortex.

---

## Building locally, if you want to

### The Rust core and the desktop CLI

Needs only a Rust toolchain (1.88 or newer):

```bash
cargo test --workspace
cargo build --release -p tempest
./target/release/tempest doctor
```

The CLI keeps upstream's command surface:

```
tempest setup                 install the runtime and register vortex://
tempest login                 sign in
tempest list [--cached]       list games
tempest play <id>             launch a game
tempest runtime [<component>] show or install runtime components
tempest doctor                diagnose the stack
tempest update                re-download the Vortex client
tempest uninstall             remove everything
```

### The Android app

Needs the Android SDK with NDK `27.2.12479018` and JDK 17.

```bash
# 1. The Rust core, cross-compiled
cargo install cargo-ndk
rustup target add aarch64-linux-android
cargo ndk --target arm64-v8a --platform 29 \
  --output-dir android/app/src/main/jniLibs \
  build --release -p tempest-jni

# 2. PRoot
export ANDROID_NDK_HOME=$ANDROID_SDK_ROOT/ndk/27.2.12479018
./scripts/build-proot.sh android/app/src/main/jniLibs/arm64-v8a

# 3. The APK
cd android && gradle assembleDebug
```

The APK lands in `android/app/build/outputs/apk/debug/app-debug.apk`.

---

## Layout

```
Cargo.toml                     workspace
crates/
  tempest-core/                platform-agnostic core
    src/platform/              the abstraction: paths, process, secrets
      linux.rs                 XDG, .desktop, encrypted key file
      android.rs               Context paths, exec policy, Keystore bridge
    src/runtime/               component catalogue, PRoot, archives, DLLs
    src/auth.rs  games.rs  uri.rs  session.rs  diagnostics.rs
  tempest-cli/                 the desktop binary
  tempest-jni/                 the Kotlin bridge (cdylib)
android/
  app/src/main/java/io/tempest/android/
    core/                      TempestBridge, models, SecureStore
    data/                      repository
    ui/                        Compose screens and the ViewModel
    service/                   the foreground service
scripts/build-proot.sh         builds PRoot for arm64 Android
.github/workflows/android.yml  the whole build
```

---

## Pinned versions

Bump these deliberately, not by drift:

| Tool | Version | Where |
|---|---|---|
| Rust | 1.88.0 | `.github/workflows/android.yml`, `Cargo.toml` (`rust-version`) |
| Android NDK | 27.2.12479018 | `.github/workflows/android.yml` |
| Android SDK | compile 35, min 29 | `android/app/build.gradle.kts` |
| Gradle | 8.11.1 | `.github/workflows/android.yml` |
| AGP | 8.7.3 | `android/gradle/libs.versions.toml` |
| Kotlin | 2.0.21 | `android/gradle/libs.versions.toml` |
| JDK | 17 | `.github/workflows/android.yml` |
| PRoot | v5.1.107.92 | `scripts/build-proot.sh` |
| talloc | 2.4.3 | `scripts/build-proot.sh` |

Runtime components have their own pinned versions and SHA-256 digests in
`crates/tempest-core/src/runtime/manifest.rs`.

### Why minSdk is 29

Not an arbitrary choice. Android 10 (API 29) is where W^X arrived: an app may no
longer `execve()` a file in its own data directory, only files in
`nativeLibraryDir`. The entire runtime design — shipping PRoot in the APK, and
letting it map guest binaries rather than exec them — exists to satisfy that
rule. Supporting older releases would mean carrying a second, untested execution
path. In practice the compatibility stack also needs a 64-bit ARM device with a
Vulkan 1.1 driver, which is Android 10 era anyway.
