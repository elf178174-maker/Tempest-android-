//! The facade both front-ends drive.
//!
//! Keeping this layer thin and coarse-grained matters for Android: every call
//! across the JNI boundary costs a transition and forces marshalling, so the
//! UI makes a handful of chunky calls (`refresh_games`, `launch`, `status`)
//! rather than chatting field by field.

use crate::config::Config;
use crate::games::{Game, GameCatalogue};
use crate::net::CancelToken;
use crate::platform::PlatformRef;
use crate::runtime::manifest::ComponentId;
use crate::runtime::{ComponentStatus, ProgressSink, RuntimeManager};
use crate::session::{SessionManager, SessionSnapshot};
use crate::uri::VortexLink;
use crate::{Result, TempestError};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub struct Tempest {
    platform: PlatformRef,
    session: SessionManager,
    runtime: RuntimeManager,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppStatus {
    pub version: String,
    pub signed_in: bool,
    pub username: Option<String>,
    pub runtime_ready: bool,
    pub missing_components: Vec<String>,
    pub session: SessionSnapshot,
    pub platform: crate::platform::PlatformInfo,
    pub storage_bytes: u64,
}

impl Tempest {
    pub fn new(platform: PlatformRef) -> Result<Self> {
        platform.paths().ensure_all()?;
        crate::logging::init_file(platform.paths().log_file());
        crate::logging::info(
            "core",
            format!(
                "Tempest {} starting on {}",
                crate::VERSION,
                platform.info().os_description
            ),
        );
        Ok(Self {
            session: SessionManager::new(Arc::clone(&platform)),
            runtime: RuntimeManager::new(Arc::clone(&platform)),
            platform,
        })
    }

    pub fn platform(&self) -> &PlatformRef {
        &self.platform
    }

    pub fn session(&self) -> &SessionManager {
        &self.session
    }

    pub fn runtime(&self) -> &RuntimeManager {
        &self.runtime
    }

    pub fn status(&self) -> AppStatus {
        let session = crate::auth::current_session(&self.platform).ok().flatten();
        AppStatus {
            version: crate::VERSION.to_string(),
            signed_in: session.is_some(),
            username: session.map(|s| s.username),
            runtime_ready: self.runtime.is_ready(),
            missing_components: self
                .runtime
                .missing_required()
                .iter()
                .map(|c| c.as_str().to_string())
                .collect(),
            session: self.session.snapshot(),
            platform: self.platform.info(),
            storage_bytes: self.platform.paths().disk_usage(),
        }
    }

    // --- authentication ----------------------------------------------------

    pub async fn login(&self, username: &str, password: &str) -> Result<String> {
        let s = crate::auth::login(&self.platform, username, password).await?;
        Ok(s.username)
    }

    pub fn logout(&self) -> Result<()> {
        crate::auth::logout(&self.platform)
    }

    // --- games -------------------------------------------------------------

    /// Cached catalogue, for instant display and offline use.
    pub fn cached_games(&self) -> GameCatalogue {
        GameCatalogue::load_cached(self.platform.paths()).unwrap_or_default()
    }

    /// Re-walk the Vortex catalogue and update the cache.
    pub async fn refresh_games(
        &self,
        cancel: &CancelToken,
        progress: Option<&(dyn Fn(usize, u32) + Send + Sync)>,
    ) -> Result<GameCatalogue> {
        let token = crate::auth::require_token(&self.platform)?;
        let catalogue = crate::games::discover(&token, cancel, progress).await?;
        catalogue.save(self.platform.paths())?;
        Ok(catalogue)
    }

    pub fn search(&self, query: &str) -> Vec<Game> {
        self.cached_games()
            .search(query)
            .into_iter()
            .cloned()
            .collect()
    }

    // --- runtime -----------------------------------------------------------

    pub fn component_status(&self) -> Vec<ComponentStatus> {
        self.runtime.status()
    }

    pub async fn install_component(
        &self,
        id: &str,
        progress: Option<&ProgressSink>,
        cancel: &CancelToken,
    ) -> Result<()> {
        let id = ComponentId::parse(id)
            .ok_or_else(|| TempestError::other(format!("unknown component '{id}'")))?;
        self.runtime.install(id, progress, cancel).await
    }

    /// Install every required component that is missing, in dependency order.
    pub async fn install_required(
        &self,
        progress: Option<&ProgressSink>,
        cancel: &CancelToken,
    ) -> Result<()> {
        // The rootfs must exist before anything can be installed into it.
        const ORDER: &[ComponentId] = &[
            ComponentId::Rootfs,
            ComponentId::Hangover,
            ComponentId::Mesa,
            ComponentId::Vortex,
        ];
        for id in ORDER {
            if cancel.is_cancelled() {
                return Err(TempestError::Cancelled);
            }
            let spec = crate::runtime::manifest::spec(*id);
            if self.runtime.is_installed(&spec) {
                continue;
            }
            self.runtime.install(*id, progress, cancel).await?;
        }
        Ok(())
    }

    pub fn uninstall_component(&self, id: &str) -> Result<()> {
        let id = ComponentId::parse(id)
            .ok_or_else(|| TempestError::other(format!("unknown component '{id}'")))?;
        self.runtime.uninstall(id)
    }

    pub fn clear_cache(&self) -> Result<u64> {
        self.runtime.clear_cache()
    }

    // --- launching ---------------------------------------------------------

    /// Launch by game id: ask Vortex for the link, then start it.
    pub async fn play(&self, game_id: u32) -> Result<()> {
        let token = crate::auth::require_token(&self.platform)?;
        let link = crate::auth::fetch_play_link(&token, game_id).await?;
        let name = self.cached_games().get(game_id).map(|g| g.name.clone());
        self.session.launch(&link, name)
    }

    /// Launch from a `vortex://` deep link or a pasted URI.
    pub fn play_uri(&self, uri: &str) -> Result<VortexLink> {
        let link = crate::uri::parse(uri)?;
        let name = self.cached_games().get(link.game_id).map(|g| g.name.clone());
        self.session.launch(&link, name)?;
        Ok(link)
    }

    pub fn stop(&self) -> Result<()> {
        self.session.stop()
    }

    // --- config ------------------------------------------------------------

    pub fn config(&self) -> Config {
        Config::load_or_default(self.platform.paths())
    }

    pub fn save_config(&self, config: &Config) -> Result<()> {
        config.save(self.platform.paths())
    }

    // --- diagnostics -------------------------------------------------------

    pub fn diagnostics(&self) -> crate::diagnostics::Report {
        crate::diagnostics::run(&self.platform)
    }

    /// Everything the "Copy logs" button puts on the clipboard: diagnostics
    /// first, then the log buffer. Both are already redacted.
    pub fn export_logs(&self) -> String {
        format!(
            "{}\n---- log ----\n{}\n---- session output ----\n{}\n",
            self.diagnostics().to_text(),
            crate::logging::export(),
            self.session.output().join("\n")
        )
    }

    pub fn clear_logs(&self) {
        crate::logging::clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::secrets::MemorySecretStore;
    use crate::platform::{
        paths::TempestPaths, process::ProcessBackend, secrets::SecretStore,
        unix_process::UnixProcessBackend, HostKind, Platform, PlatformInfo, UriRegistration,
    };

    struct P {
        paths: TempestPaths,
        process: UnixProcessBackend,
        secrets: MemorySecretStore,
    }
    impl Platform for P {
        fn paths(&self) -> &TempestPaths { &self.paths }
        fn process(&self) -> &dyn ProcessBackend { &self.process }
        fn secrets(&self) -> &dyn SecretStore { &self.secrets }
        fn info(&self) -> PlatformInfo {
            PlatformInfo {
                kind: HostKind::Android,
                os_description: "Android 15 (API 35)".into(),
                cpu_arch: "arm64-v8a".into(),
                device_model: Some("POCO F7 Ultra".into()),
                needs_x86_translation: true,
            }
        }
        fn uri_handler_status(&self) -> Result<UriRegistration> {
            Ok(UriRegistration::ManifestDeclared)
        }
        fn register_uri_handler(&self) -> Result<UriRegistration> {
            Ok(UriRegistration::ManifestDeclared)
        }
    }

    fn app(dir: &std::path::Path) -> Tempest {
        let paths = TempestPaths::with_root(dir.join("data"), dir.join("lib"));
        Tempest::new(Arc::new(P {
            paths,
            process: UnixProcessBackend::restricted_to(dir.join("lib")),
            secrets: MemorySecretStore::default(),
        }))
        .unwrap()
    }

    #[test]
    fn a_fresh_app_is_signed_out_and_not_ready() {
        let dir = tempfile::tempdir().unwrap();
        let status = app(dir.path()).status();
        assert!(!status.signed_in);
        assert!(!status.runtime_ready);
        assert!(status.missing_components.contains(&"rootfs".to_string()));
        assert_eq!(status.session.state, crate::session::SessionState::Idle);
    }

    #[test]
    fn status_serialises_to_json_for_the_ui() {
        let dir = tempfile::tempdir().unwrap();
        let json = serde_json::to_string(&app(dir.path()).status()).unwrap();
        assert!(json.contains("\"runtime_ready\":false"));
        assert!(json.contains("POCO F7 Ultra"));
    }

    #[test]
    fn logout_clears_the_stored_session() {
        let dir = tempfile::tempdir().unwrap();
        let t = app(dir.path());
        t.platform
            .secrets()
            .set(crate::platform::secrets::SESSION_TOKEN_KEY, "tok")
            .unwrap();
        t.platform
            .secrets()
            .set(crate::platform::secrets::USERNAME_KEY, "bob")
            .unwrap();
        assert!(t.status().signed_in);
        assert_eq!(t.status().username.as_deref(), Some("bob"));

        t.logout().unwrap();
        assert!(!t.status().signed_in);
    }

    #[test]
    fn play_uri_rejects_a_malformed_link_before_touching_the_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let err = app(dir.path()).play_uri("http://evil.example/?game=1").unwrap_err();
        assert_eq!(err.kind(), "uri");
    }

    #[test]
    fn a_valid_link_fails_on_missing_runtime_with_a_useful_message() {
        let dir = tempfile::tempdir().unwrap();
        let err = app(dir.path())
            .play_uri("vortex://play?game=4&token=abc")
            .unwrap_err();
        assert_eq!(err.kind(), "missing");
        let msg = err.to_string();
        assert!(msg.contains("Settings"), "should tell the user where to go: {msg}");
        assert!(msg.contains("rootfs"), "should name what is missing: {msg}");
    }

    #[test]
    fn unknown_component_ids_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let err = app(dir.path()).uninstall_component("not-a-component").unwrap_err();
        assert!(err.to_string().contains("unknown component"));
    }

    #[test]
    fn config_round_trips_through_the_facade() {
        let dir = tempfile::tempdir().unwrap();
        let t = app(dir.path());
        let mut cfg = t.config();
        cfg.graphics.vulkan_driver = crate::config::VulkanDriver::Lavapipe;
        cfg.launcher.launch_timeout_secs = 240;
        t.save_config(&cfg).unwrap();

        let reloaded = t.config();
        assert_eq!(reloaded.graphics.vulkan_driver, crate::config::VulkanDriver::Lavapipe);
        assert_eq!(reloaded.launcher.launch_timeout_secs, 240);
    }

    #[test]
    fn search_over_the_cached_catalogue_works_offline() {
        let dir = tempfile::tempdir().unwrap();
        let t = app(dir.path());
        GameCatalogue {
            games: vec![Game { id: 1, name: "Portal".into(), description: None, image_url: None }],
            fetched_at: 0,
        }
        .save(t.platform.paths())
        .unwrap();

        assert_eq!(t.search("port").len(), 1);
        assert_eq!(t.search("").len(), 1);
        assert!(t.search("zzz").is_empty());
    }

    #[test]
    fn exported_logs_carry_diagnostics_and_no_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let t = app(dir.path());
        t.clear_logs();
        crate::logging::info("test", "opening vortex://play?game=1&token=TOPSECRET");

        let export = t.export_logs();
        assert!(export.contains("Tempest diagnostics"));
        assert!(!export.contains("TOPSECRET"), "log export leaked a token");
    }

    #[test]
    fn refresh_games_without_a_session_fails_as_an_auth_error() {
        let dir = tempfile::tempdir().unwrap();
        let t = app(dir.path());
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let err = rt
            .block_on(t.refresh_games(&CancelToken::new(), None))
            .unwrap_err();
        assert_eq!(err.kind(), "auth");
        assert!(err.to_string().contains("not signed in"));
    }
}
