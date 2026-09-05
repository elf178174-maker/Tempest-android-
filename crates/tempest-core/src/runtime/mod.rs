//! Runtime component management: install, verify, remove, report.

pub mod archive;
pub mod dll;
pub mod guest;
pub mod manifest;

use crate::net::CancelToken;
use crate::platform::{PlatformRef, ProcessStatus};
use crate::{Result, TempestError};
use manifest::{ComponentId, ComponentSpec, Necessity, Source};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// What is happening to a component right now, for the progress UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum InstallPhase {
    Queued,
    Downloading { done: u64, total: Option<u64> },
    Verifying,
    Extracting,
    Configuring { step: String },
    Done,
    Failed { error: String, kind: String },
}

/// Emitted as installation proceeds.
pub type ProgressSink = std::sync::Arc<dyn Fn(ComponentId, InstallPhase) + Send + Sync>;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InstalledComponent {
    pub version: String,
    /// SHA-256 of the archive this came from, so the UI can show provenance.
    pub sha256: String,
    pub installed_at: u64,
    pub source_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuntimeState {
    pub components: BTreeMap<String, InstalledComponent>,
}

impl RuntimeState {
    pub fn load(platform: &PlatformRef) -> Self {
        std::fs::read_to_string(platform.paths().runtime_state_file())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, platform: &PlatformRef) -> Result<()> {
        let path = platform.paths().runtime_state_file();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self).map_err(TempestError::other)?)?;
        Ok(())
    }

    pub fn get(&self, id: ComponentId) -> Option<&InstalledComponent> {
        self.components.get(id.as_str())
    }

    pub fn record(&mut self, id: ComponentId, entry: InstalledComponent) {
        self.components.insert(id.as_str().to_string(), entry);
    }

    pub fn forget(&mut self, id: ComponentId) {
        self.components.remove(id.as_str());
    }
}

/// A component's status as shown in Settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentStatus {
    pub id: String,
    pub display_name: String,
    pub required: bool,
    pub installed: bool,
    pub installed_version: Option<String>,
    pub available_version: String,
    pub approx_bytes: u64,
    pub license: String,
    pub upstream: String,
    pub purpose: String,
    pub sha256: Option<String>,
}

pub struct RuntimeManager {
    platform: PlatformRef,
}

impl RuntimeManager {
    pub fn new(platform: PlatformRef) -> Self {
        Self { platform }
    }

    /// Whether a component's files are actually on disk, not merely recorded.
    pub fn is_installed(&self, spec: &ComponentSpec) -> bool {
        let paths = self.platform.paths();
        match spec.id {
            ComponentId::Vortex => paths.vortex_exe().exists(),
            ComponentId::Dxvk => paths.runtime_dir().join("dxvk").is_dir(),
            ComponentId::Vkd3d => paths.runtime_dir().join("vkd3d").is_dir(),
            _ => {
                if spec.sentinel.is_empty() {
                    false
                } else {
                    paths.guest_rootfs().join(spec.sentinel).exists()
                }
            }
        }
    }

    pub fn status(&self) -> Vec<ComponentStatus> {
        let state = RuntimeState::load(&self.platform);
        manifest::catalogue()
            .into_iter()
            .map(|spec| {
                let recorded = state.get(spec.id);
                ComponentStatus {
                    id: spec.id.as_str().to_string(),
                    display_name: spec.display_name.to_string(),
                    required: spec.necessity == Necessity::Required,
                    installed: self.is_installed(&spec),
                    installed_version: recorded.map(|r| r.version.clone()),
                    available_version: spec.version.to_string(),
                    approx_bytes: spec.approx_bytes,
                    license: spec.license.to_string(),
                    upstream: spec.upstream.to_string(),
                    purpose: spec.purpose.to_string(),
                    sha256: recorded.map(|r| r.sha256.clone()).filter(|s| !s.is_empty()),
                }
            })
            .collect()
    }

    /// Required components that are not yet present.
    pub fn missing_required(&self) -> Vec<ComponentId> {
        manifest::catalogue()
            .into_iter()
            .filter(|s| s.necessity == Necessity::Required && !self.is_installed(s))
            .map(|s| s.id)
            .collect()
    }

    /// Whether everything needed to launch is in place.
    pub fn is_ready(&self) -> bool {
        self.missing_required().is_empty()
    }

    async fn resolve_url(&self, spec: &ComponentSpec, token: Option<&str>) -> Result<(String, Option<String>)> {
        Ok(match &spec.source {
            Source::Pinned { url, sha256 } => (
                (*url).to_string(),
                if sha256.is_empty() { None } else { Some((*sha256).to_string()) },
            ),
            Source::VortexDownload { url } => {
                let _ = token;
                ((*url).to_string(), None)
            }
            Source::GithubLatest { repo, asset_suffix } => {
                (github_latest_asset(repo, asset_suffix).await?, None)
            }
            Source::GuestPackages { .. } => {
                return Err(TempestError::other(
                    "guest packages are installed with apt, not downloaded directly",
                ))
            }
        })
    }

    /// Install one component.
    pub async fn install(
        &self,
        id: ComponentId,
        progress: Option<&ProgressSink>,
        cancel: &CancelToken,
    ) -> Result<()> {
        let spec = manifest::spec(id);
        let emit = |phase: InstallPhase| {
            if let Some(p) = progress {
                p(id, phase);
            }
        };
        emit(InstallPhase::Queued);

        let result = self.install_inner(&spec, progress, cancel).await;
        match &result {
            Ok(()) => emit(InstallPhase::Done),
            Err(e) => {
                crate::logging::error("runtime", format!("{}: {e}", spec.display_name));
                emit(InstallPhase::Failed {
                    error: e.to_string(),
                    kind: e.kind().to_string(),
                });
            }
        }
        result
    }

    async fn install_inner(
        &self,
        spec: &ComponentSpec,
        progress: Option<&ProgressSink>,
        cancel: &CancelToken,
    ) -> Result<()> {
        let paths = self.platform.paths();
        paths.ensure_all()?;

        if let Source::GuestPackages { packages } = &spec.source {
            if let Some(p) = progress {
                p(spec.id, InstallPhase::Configuring { step: "installing packages".into() });
            }
            self.apt_install(packages)?;
            self.record(spec, String::new(), "apt".to_string())?;
            return Ok(());
        }

        let token = crate::auth::stored_token(&self.platform)?;
        let (url, expected) = self.resolve_url(spec, token.as_deref()).await?;

        let archive_path = paths.cache_dir().join(spec.archive_name);
        let id = spec.id;
        let progress_fn: Option<crate::net::ProgressFn> = progress.map(|p| {
            let p = std::sync::Arc::clone(p);
            Box::new(move |done: u64, total: Option<u64>| {
                p(id, InstallPhase::Downloading { done, total });
            }) as crate::net::ProgressFn
        });

        let client = crate::net::client()?;

        // Vortex's download endpoint needs the session cookie.
        let downloaded = if let (Source::VortexDownload { .. }, Some(tok)) = (&spec.source, &token) {
            download_authenticated(&client, &url, tok, &archive_path, progress_fn.as_ref(), cancel).await?
        } else {
            crate::net::download_verified(
                &client,
                &url,
                &archive_path,
                expected.as_deref(),
                progress_fn.as_ref(),
                cancel,
            )
            .await?
        };

        if let Some(p) = progress {
            p(spec.id, InstallPhase::Verifying);
        }
        let digest = crate::net::sha256_file(&downloaded)?;

        if let Some(p) = progress {
            p(spec.id, InstallPhase::Extracting);
        }
        self.place(spec, &downloaded, progress)?;

        let config = crate::config::Config::load_or_default(paths);
        if config.storage.prune_archives_after_install && spec.id != ComponentId::Vortex {
            std::fs::remove_file(&downloaded).ok();
        }

        self.record(spec, digest, url)?;
        Ok(())
    }

    fn record(&self, spec: &ComponentSpec, digest: String, url: String) -> Result<()> {
        let mut state = RuntimeState::load(&self.platform);
        state.record(
            spec.id,
            InstalledComponent {
                version: spec.version.to_string(),
                sha256: digest,
                installed_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                source_url: crate::net::sanitize_url_for_log(&url),
            },
        );
        state.save(&self.platform)
    }

    /// Unpack a downloaded archive into its final home.
    fn place(
        &self,
        spec: &ComponentSpec,
        downloaded: &std::path::Path,
        progress: Option<&ProgressSink>,
    ) -> Result<()> {
        let paths = self.platform.paths();
        match spec.id {
            ComponentId::Rootfs => {
                let root = paths.guest_rootfs();
                std::fs::create_dir_all(&root)?;
                let report = archive::extract(downloaded, &root, spec.format, spec.strip_components)?;
                crate::logging::info(
                    "runtime",
                    format!(
                        "rootfs: {} files, {} dirs, {} links, {} skipped",
                        report.files, report.dirs, report.links, report.skipped.len()
                    ),
                );
                self.prepare_rootfs()?;
                Ok(())
            }
            ComponentId::Hangover => {
                // The archive holds .deb packages plus a DXVK tarball. Unpack it
                // into the guest's staging directory and let apt resolve Wine's
                // dependency graph, which is far too large to hand-resolve.
                let staging = paths.guest_rootfs().join(GUEST_STAGING_REL);
                if staging.exists() {
                    std::fs::remove_dir_all(&staging).ok();
                }
                std::fs::create_dir_all(&staging)?;
                archive::extract(downloaded, &staging, spec.format, spec.strip_components)?;

                if let Some(p) = progress {
                    p(spec.id, InstallPhase::Configuring { step: "installing Wine".into() });
                }
                self.install_staged_debs()?;

                // Hangover bundles a DXVK matched to its Wine, including native
                // ARM64 builds; unpack it so the prefix setup can use it.
                if let Some(tarball) = find_file(&staging, "dxvk-", ".tar.gz") {
                    let dest = paths.runtime_dir().join("dxvk");
                    std::fs::create_dir_all(&dest)?;
                    archive::extract(&tarball, &dest, archive::Format::TarGz, 1)?;
                }
                std::fs::remove_dir_all(&staging).ok();
                Ok(())
            }
            ComponentId::Dxvk => {
                let dest = paths.runtime_dir().join("dxvk");
                if dest.exists() {
                    std::fs::remove_dir_all(&dest).ok();
                }
                archive::extract(downloaded, &dest, spec.format, spec.strip_components)?;
                Ok(())
            }
            ComponentId::Vkd3d => {
                let dest = paths.runtime_dir().join("vkd3d");
                if dest.exists() {
                    std::fs::remove_dir_all(&dest).ok();
                }
                archive::extract(downloaded, &dest, spec.format, spec.strip_components)?;
                Ok(())
            }
            ComponentId::Vortex => self.place_vortex(downloaded),
            ComponentId::Mesa => Ok(()),
        }
    }

    /// Extract Vortex.exe (and receiver.exe when present) from the client zip.
    fn place_vortex(&self, zip_path: &std::path::Path) -> Result<()> {
        let paths = self.platform.paths();
        let staging = paths.tmp_dir().join("vortex-unpack");
        if staging.exists() {
            std::fs::remove_dir_all(&staging).ok();
        }
        archive::extract(zip_path, &staging, archive::Format::Zip, 0)?;

        let mut found_main = false;
        for (needle, dest) in [
            ("vortex.exe", paths.vortex_exe()),
            ("receiver.exe", paths.receiver_exe()),
        ] {
            if let Some(src) = find_named(&staging, needle) {
                if !dll::is_pe(&src) {
                    return Err(TempestError::runtime(
                        "Vortex",
                        format!("{} is not a Windows executable", src.display()),
                    ));
                }
                std::fs::create_dir_all(paths.vortex_dir())?;
                std::fs::copy(&src, &dest)?;
                crate::logging::info("runtime", format!("installed {}", dest.display()));
                if needle == "vortex.exe" {
                    found_main = true;
                }
            }
        }

        // Anything else in the archive (data files, DLLs Vortex ships) goes
        // alongside the exe, since Vortex expects its own layout.
        copy_tree(&staging, paths.vortex_dir())?;
        std::fs::remove_dir_all(&staging).ok();

        if !found_main {
            return Err(TempestError::runtime(
                "Vortex",
                "the downloaded client archive contained no Vortex.exe",
            ));
        }
        Ok(())
    }

    /// Make a freshly unpacked Ubuntu image usable: DNS, hosts, a home
    /// directory, and apt configuration that does not try to use features the
    /// container cannot provide.
    fn prepare_rootfs(&self) -> Result<()> {
        let root = self.platform.paths().guest_rootfs();

        let write = |rel: &str, contents: &str| -> Result<()> {
            let path = root.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, contents)?;
            Ok(())
        };

        // Android does not expose /etc/resolv.conf to apps, so use public
        // resolvers rather than leaving the guest with no DNS at all.
        write("etc/resolv.conf", "nameserver 1.1.1.1\nnameserver 8.8.8.8\n")?;
        write(
            "etc/hosts",
            "127.0.0.1 localhost\n::1 localhost ip6-localhost ip6-loopback\n",
        )?;
        // dpkg and apt refuse to run some operations without these.
        write("etc/passwd", "root:x:0:0:root:/root:/bin/sh\ntempest:x:1000:1000:tempest:/home/tempest:/bin/sh\n")?;
        write("etc/group", "root:x:0:\ntempest:x:1000:\n")?;
        write("etc/hostname", "tempest\n")?;
        // PRoot cannot provide the mount namespace apt sandboxing wants, and
        // fsync on app storage is slow enough to matter on a phone.
        write(
            "etc/apt/apt.conf.d/99tempest",
            "APT::Sandbox::User \"root\";\n\
             Acquire::Retries \"3\";\n\
             DPkg::Options {\"--force-confdef\";\"--force-confold\";};\n\
             Dpkg::Use-Pty \"false\";\n",
        )?;
        write(
            "etc/dpkg/dpkg.cfg.d/99tempest",
            "force-unsafe-io\npath-exclude=/usr/share/man/*\npath-exclude=/usr/share/doc/*\n",
        )?;

        for dir in ["home/tempest", "tmp", "run", "var/tmp", "shadercache", "games", "vortex"] {
            std::fs::create_dir_all(root.join(dir))?;
        }
        Ok(())
    }

    /// `apt-get install` inside the guest.
    fn apt_install(&self, packages: &[&str]) -> Result<()> {
        let names = packages.join(" ");
        self.run_in_guest(
            "apt",
            "set -e; apt-get update -qq; apt-get install -y --no-install-recommends $@",
            &packages.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            600,
        )
        .map(|_| ())
        .map_err(|e| TempestError::runtime(format!("packages: {names}"), e))
    }

    /// Install every `.deb` staged in the guest, letting apt pull dependencies.
    fn install_staged_debs(&self) -> Result<()> {
        self.run_in_guest(
            "dpkg",
            "set -e; cd \"$1\"; apt-get update -qq; apt-get install -y --no-install-recommends ./*.deb",
            &[GUEST_STAGING.to_string()],
            1800,
        )
        .map(|_| ())
        .map_err(|e| TempestError::runtime("Hangover Wine", e))
    }

    /// Run a fixed shell template in the guest and wait for it.
    pub fn run_in_guest(
        &self,
        label: &str,
        script: &str,
        args: &[String],
        timeout_secs: u64,
    ) -> Result<Vec<String>> {
        let paths = self.platform.paths();
        let g = guest::GuestEnv::new(&self.platform);
        let spec = g.shell(paths, label, script, args, guest::base_env())?;

        let mut handle = self.platform.process().spawn(spec)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        loop {
            match handle.poll()? {
                ProcessStatus::Running => {
                    if std::time::Instant::now() > deadline {
                        handle.terminate().ok();
                        return Err(TempestError::Process(format!(
                            "{label} did not finish within {timeout_secs}s"
                        )));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                done => {
                    let output = handle.drain_output();
                    if let ProcessStatus::Exited(0) = done {
                        return Ok(output);
                    }
                    let tail = output
                        .iter()
                        .rev()
                        .take(15)
                        .rev()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n");
                    return Err(TempestError::Process(format!(
                        "{label} {}\n{tail}",
                        done.explain()
                    )));
                }
            }
        }
    }

    /// Remove a component's files and forget it.
    pub fn uninstall(&self, id: ComponentId) -> Result<()> {
        let paths = self.platform.paths();
        match id {
            ComponentId::Rootfs => {
                let root = paths.guest_rootfs();
                if root.exists() {
                    std::fs::remove_dir_all(&root)?;
                }
            }
            ComponentId::Dxvk => {
                let d = paths.runtime_dir().join("dxvk");
                if d.exists() {
                    std::fs::remove_dir_all(d)?;
                }
            }
            ComponentId::Vkd3d => {
                let d = paths.runtime_dir().join("vkd3d");
                if d.exists() {
                    std::fs::remove_dir_all(d)?;
                }
            }
            ComponentId::Vortex => {
                if paths.vortex_dir().exists() {
                    std::fs::remove_dir_all(paths.vortex_dir())?;
                }
            }
            // Hangover and Mesa live inside the rootfs; removing them alone
            // would leave apt's database inconsistent, so the rootfs is the
            // unit of removal. Say so rather than pretending it worked.
            ComponentId::Hangover | ComponentId::Mesa => {
                return Err(TempestError::other(
                    "this component is installed inside the Linux filesystem; \
                     remove the Ubuntu base image to remove it",
                ));
            }
        }
        let mut state = RuntimeState::load(&self.platform);
        state.forget(id);
        state.save(&self.platform)
    }

    /// Delete cached downloads. Returns the bytes reclaimed.
    pub fn clear_cache(&self) -> Result<u64> {
        let cache = self.platform.paths().cache_dir();
        let mut freed = 0u64;
        if let Ok(entries) = std::fs::read_dir(cache) {
            for entry in entries.flatten() {
                let path = entry.path();
                // Keep the game catalogue: it is what makes the app usable offline.
                if path.file_name().is_some_and(|n| n == "games.json") {
                    continue;
                }
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                let removed = if path.is_dir() {
                    std::fs::remove_dir_all(&path).is_ok()
                } else {
                    std::fs::remove_file(&path).is_ok()
                };
                if removed {
                    freed += size;
                }
            }
        }
        Ok(freed)
    }
}

/// `GUEST_STAGING` without the leading slash, for joining onto the rootfs path.
const GUEST_STAGING_REL: &str = "tmp/tempest-staging";
const GUEST_STAGING: &str = guest::GUEST_STAGING;

async fn download_authenticated(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    dest: &std::path::Path,
    progress: Option<&crate::net::ProgressFn>,
    cancel: &CancelToken,
) -> Result<std::path::PathBuf> {
    use futures_util::StreamExt;
    use std::io::Write;

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let resp = client
        .get(url)
        .header("Cookie", crate::auth::session_cookie(token))
        .send()
        .await?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(TempestError::Auth(
            "Vortex refused the download — sign in again".into(),
        ));
    }
    if !status.is_success() {
        return Err(TempestError::Network(format!(
            "HTTP {status} downloading the Vortex client"
        )));
    }

    let total = resp.content_length();
    let part = dest.with_extension("part");
    let mut file = std::fs::File::create(&part)?;
    let mut written = 0u64;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        if cancel.is_cancelled() {
            drop(file);
            std::fs::remove_file(&part).ok();
            return Err(TempestError::Cancelled);
        }
        let chunk = chunk.map_err(TempestError::from)?;
        file.write_all(&chunk)?;
        written += chunk.len() as u64;
        if let Some(cb) = progress {
            cb(written, total);
        }
    }
    file.flush()?;
    drop(file);
    std::fs::rename(&part, dest)?;
    Ok(dest.to_path_buf())
}

/// Resolve the download URL of the newest release asset matching a suffix.
async fn github_latest_asset(repo: &str, asset_suffix: &str) -> Result<String> {
    let client = crate::net::client()?;
    let body: serde_json::Value = client
        .get(format!("https://api.github.com/repos/{repo}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .json()
        .await?;

    body["assets"]
        .as_array()
        .ok_or_else(|| TempestError::runtime(repo, "release has no assets"))?
        .iter()
        .find_map(|a| {
            let name = a["name"].as_str()?;
            // Skip debug/source archives that share the suffix.
            if name.ends_with(asset_suffix) && !name.contains("debug") && !name.contains("source") {
                a["browser_download_url"].as_str().map(str::to_string)
            } else {
                None
            }
        })
        .ok_or_else(|| {
            TempestError::runtime(repo, format!("no release asset ending in '{asset_suffix}'"))
        })
}

fn find_file(dir: &std::path::Path, prefix: &str, suffix: &str) -> Option<std::path::PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let name = e.file_name().to_string_lossy().to_string();
        (name.starts_with(prefix) && name.ends_with(suffix)).then(|| e.path())
    })
}

/// Case-insensitive recursive search for a file by name.
fn find_named(dir: &std::path::Path, name_lower: &str) -> Option<std::path::PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            subdirs.push(path);
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(name_lower))
        {
            return Some(path);
        }
    }
    subdirs.iter().find_map(|d| find_named(d, name_lower))
}

fn copy_tree(src: &std::path::Path, dest: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)?.flatten() {
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else if !to.exists() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::secrets::MemorySecretStore;
    use crate::platform::{
        paths::TempestPaths, process::ProcessBackend, secrets::SecretStore,
        unix_process::UnixProcessBackend, HostKind, Platform, PlatformInfo, UriRegistration,
    };
    use std::sync::Arc;

    struct FakePlatform {
        paths: TempestPaths,
        process: UnixProcessBackend,
        secrets: MemorySecretStore,
    }

    impl Platform for FakePlatform {
        fn paths(&self) -> &TempestPaths { &self.paths }
        fn process(&self) -> &dyn ProcessBackend { &self.process }
        fn secrets(&self) -> &dyn SecretStore { &self.secrets }
        fn info(&self) -> PlatformInfo {
            PlatformInfo {
                kind: HostKind::LinuxDesktop,
                os_description: "test".into(),
                cpu_arch: "x86_64".into(),
                device_model: None,
                needs_x86_translation: false,
            }
        }
        fn register_uri_handler(&self) -> Result<UriRegistration> {
            Ok(UriRegistration::Desktop)
        }
    }

    fn fake(dir: &std::path::Path) -> PlatformRef {
        let paths = TempestPaths::with_root(dir.join("data"), dir.join("lib"));
        paths.ensure_all().unwrap();
        Arc::new(FakePlatform {
            paths,
            process: UnixProcessBackend::permissive(),
            secrets: MemorySecretStore::default(),
        })
    }

    #[test]
    fn nothing_is_installed_on_a_fresh_profile() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = RuntimeManager::new(fake(dir.path()));
        assert!(!mgr.is_ready());
        let missing = mgr.missing_required();
        assert!(missing.contains(&ComponentId::Rootfs));
        assert!(missing.contains(&ComponentId::Hangover));
        assert!(missing.contains(&ComponentId::Vortex));
    }

    #[test]
    fn status_reports_every_component_with_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = RuntimeManager::new(fake(dir.path()));
        let status = mgr.status();
        assert_eq!(status.len(), ComponentId::all().len());
        for s in &status {
            assert!(!s.license.is_empty());
            assert!(s.upstream.starts_with("https://"));
            assert!(!s.installed);
        }
    }

    #[test]
    fn installation_is_detected_from_the_sentinel_file() {
        let dir = tempfile::tempdir().unwrap();
        let platform = fake(dir.path());
        let mgr = RuntimeManager::new(Arc::clone(&platform));
        let spec = manifest::spec(ComponentId::Rootfs);
        assert!(!mgr.is_installed(&spec));

        let sentinel = platform.paths().guest_rootfs().join(spec.sentinel);
        std::fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
        std::fs::write(&sentinel, b"x").unwrap();
        assert!(mgr.is_installed(&spec));
    }

    #[test]
    fn state_survives_a_round_trip_and_records_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let platform = fake(dir.path());
        let mut state = RuntimeState::load(&platform);
        state.record(
            ComponentId::Hangover,
            InstalledComponent {
                version: "11.16".into(),
                sha256: "abc".into(),
                installed_at: 42,
                source_url: "https://github.com/AndreRH/hangover".into(),
            },
        );
        state.save(&platform).unwrap();

        let loaded = RuntimeState::load(&platform);
        let entry = loaded.get(ComponentId::Hangover).unwrap();
        assert_eq!(entry.version, "11.16");
        assert_eq!(entry.sha256, "abc");

        let mut loaded = loaded;
        loaded.forget(ComponentId::Hangover);
        loaded.save(&platform).unwrap();
        assert!(RuntimeState::load(&platform).get(ComponentId::Hangover).is_none());
    }

    #[test]
    fn uninstalling_a_guest_installed_component_explains_why_it_cannot() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = RuntimeManager::new(fake(dir.path()));
        let err = mgr.uninstall(ComponentId::Hangover).unwrap_err();
        assert!(err.to_string().contains("Linux filesystem"), "{err}");
    }

    #[test]
    fn clear_cache_frees_downloads_but_keeps_the_game_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        let platform = fake(dir.path());
        let cache = platform.paths().cache_dir();
        std::fs::write(cache.join("big.tar"), vec![7u8; 5000]).unwrap();
        std::fs::write(cache.join("games.json"), b"{}").unwrap();

        let mgr = RuntimeManager::new(platform.clone());
        let freed = mgr.clear_cache().unwrap();
        assert_eq!(freed, 5000);
        assert!(!cache.join("big.tar").exists());
        assert!(cache.join("games.json").exists(), "offline catalogue was destroyed");
    }

    #[test]
    fn find_named_is_case_insensitive_and_recursive() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("Vortex.EXE"), b"MZ").unwrap();
        let found = find_named(dir.path(), "vortex.exe").unwrap();
        assert_eq!(found, deep.join("Vortex.EXE"));
        assert!(find_named(dir.path(), "absent.exe").is_none());
    }

    #[test]
    fn vortex_install_rejects_an_archive_without_the_client() {
        let dir = tempfile::tempdir().unwrap();
        let platform = fake(dir.path());
        let mgr = RuntimeManager::new(Arc::clone(&platform));

        let zip_path = dir.path().join("bad.zip");
        {
            let f = std::fs::File::create(&zip_path).unwrap();
            let mut w = zip::ZipWriter::new(f);
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            w.start_file("readme.txt", opts).unwrap();
            use std::io::Write;
            w.write_all(b"nothing here").unwrap();
            w.finish().unwrap();
        }
        let err = mgr.place_vortex(&zip_path).unwrap_err();
        assert!(err.to_string().contains("no Vortex.exe"), "{err}");
    }

    #[test]
    fn rootfs_preparation_writes_working_dns_and_apt_config() {
        let dir = tempfile::tempdir().unwrap();
        let platform = fake(dir.path());
        let mgr = RuntimeManager::new(Arc::clone(&platform));
        mgr.prepare_rootfs().unwrap();

        let root = platform.paths().guest_rootfs();
        let resolv = std::fs::read_to_string(root.join("etc/resolv.conf")).unwrap();
        assert!(resolv.contains("nameserver"), "guest would have no DNS");
        assert!(root.join("etc/passwd").exists());
        assert!(root.join("home/tempest").is_dir());
        let apt = std::fs::read_to_string(root.join("etc/apt/apt.conf.d/99tempest")).unwrap();
        assert!(apt.contains("Sandbox::User"), "apt would fail under PRoot");
    }

    #[test]
    fn guest_run_reports_a_failing_command_with_its_output() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = RuntimeManager::new(fake(dir.path()));
        // Desktop platform: runs directly on the host, no PRoot needed.
        let err = mgr
            .run_in_guest("probe", "echo something-broke >&2; exit 4", &[], 30)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("exit"), "{msg}");
        assert!(msg.contains("something-broke"), "output not surfaced: {msg}");
    }

    #[test]
    fn guest_run_returns_output_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = RuntimeManager::new(fake(dir.path()));
        let out = mgr.run_in_guest("probe", "echo \"$1\"", &["hello".into()], 30).unwrap();
        assert!(out.iter().any(|l| l == "hello"), "{out:?}");
    }

    #[test]
    fn guest_run_times_out_rather_than_hanging_forever() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = RuntimeManager::new(fake(dir.path()));
        let err = mgr.run_in_guest("sleeper", "sleep 30", &[], 1).unwrap_err();
        assert!(err.to_string().contains("did not finish"), "{err}");
    }
}
