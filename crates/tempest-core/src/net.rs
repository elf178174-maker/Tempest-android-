//! HTTP client construction, streaming downloads with progress, and integrity
//! verification.
//!
//! Everything downloaded by the runtime manager is treated as untrusted: it is
//! written to a temporary file, hashed, compared against a pinned SHA-256 where
//! one is known, and only then moved into place.

use crate::{Result, TempestError};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const USER_AGENT: &str = concat!("tempest-android/", env!("CARGO_PKG_VERSION"));

/// Progress callback: `(bytes_done, total_bytes_if_known)`.
pub type ProgressFn = Box<dyn Fn(u64, Option<u64>) + Send + Sync>;

pub fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(20))
        // No total-request timeout: runtime archives are hundreds of megabytes
        // and a phone on mobile data can legitimately take a long time.
        .pool_idle_timeout(Duration::from_secs(30))
        .build()
        .map_err(Into::into)
}

/// A client that does not follow redirects — needed for the login flow, where
/// the session cookie rides on the 302 itself.
pub fn no_redirect_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(Into::into)
}

/// Signals cancellation to an in-flight download.
#[derive(Clone, Default)]
pub struct CancelToken(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Stream `url` to `dest`, reporting progress and verifying the digest.
///
/// The download goes to `<dest>.part` and is renamed only after the hash
/// matches, so an interrupted or tampered transfer can never be mistaken for a
/// complete component.
pub async fn download_verified(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_sha256: Option<&str>,
    progress: Option<&ProgressFn>,
    cancel: &CancelToken,
) -> Result<PathBuf> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // A previously verified file is reused rather than re-downloaded.
    if dest.exists() {
        if let Some(expected) = expected_sha256 {
            match sha256_file(dest) {
                Ok(actual) if actual.eq_ignore_ascii_case(expected) => {
                    crate::logging::info("net", format!("reusing verified {}", dest.display()));
                    return Ok(dest.to_path_buf());
                }
                _ => {
                    crate::logging::warn(
                        "net",
                        format!("{} failed verification, re-downloading", dest.display()),
                    );
                    tokio::fs::remove_file(dest).await.ok();
                }
            }
        }
    }

    let part = dest.with_extension(format!(
        "{}.part",
        dest.extension().and_then(|e| e.to_str()).unwrap_or("dl")
    ));

    crate::logging::info("net", format!("downloading {url}"));
    let resp = client.get(url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(TempestError::Network(format!(
            "HTTP {status} while fetching {}",
            sanitize_url_for_log(url)
        )));
    }

    let total = resp.content_length();
    let mut hasher = Sha256::new();
    let mut written: u64 = 0;
    let mut file = tokio::fs::File::create(&part).await?;
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        if cancel.is_cancelled() {
            drop(file);
            tokio::fs::remove_file(&part).await.ok();
            return Err(TempestError::Cancelled);
        }
        let chunk = chunk.map_err(TempestError::from)?;
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
        written += chunk.len() as u64;
        if let Some(cb) = progress {
            cb(written, total);
        }
    }
    file.flush().await?;
    file.sync_all().await?;
    drop(file);

    if let Some(expected) = total {
        if written != expected {
            tokio::fs::remove_file(&part).await.ok();
            return Err(TempestError::Network(format!(
                "transfer truncated: got {written} of {expected} bytes"
            )));
        }
    }

    let actual = hex::encode(hasher.finalize());
    if let Some(expected) = expected_sha256 {
        if !actual.eq_ignore_ascii_case(expected) {
            tokio::fs::remove_file(&part).await.ok();
            return Err(TempestError::Integrity {
                what: dest
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                expected: expected.to_string(),
                actual,
            });
        }
    } else {
        // No pinned digest (e.g. "latest release" resolution): record what we
        // got so the value is at least visible in the log and the UI.
        crate::logging::warn(
            "net",
            format!(
                "{} had no pinned checksum; sha256 of what was received is {actual}",
                dest.file_name().unwrap_or_default().to_string_lossy()
            ),
        );
    }

    tokio::fs::rename(&part, dest).await?;
    Ok(dest.to_path_buf())
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Strip the query string from a URL before it goes anywhere near a log:
/// Vortex play links carry the session token there.
pub fn sanitize_url_for_log(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut u) => {
            u.set_query(None);
            u.set_fragment(None);
            u.to_string()
        }
        Err(_) => "<unparseable url>".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_sanitisation_removes_the_token() {
        let got = sanitize_url_for_log("https://playvortex.io/games/4/play?session_token=secret");
        assert!(!got.contains("secret"), "{got}");
        assert!(got.starts_with("https://playvortex.io/games/4/play"));
    }

    #[test]
    fn digest_of_known_input() {
        // SHA-256 of the empty string, and of "abc".
        assert_eq!(
            sha256_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn file_digest_matches_byte_digest() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.bin");
        std::fs::write(&p, b"tempest").unwrap();
        assert_eq!(sha256_file(&p).unwrap(), sha256_bytes(b"tempest"));
    }

    #[test]
    fn cancel_token_flips_once() {
        let t = CancelToken::new();
        assert!(!t.is_cancelled());
        let clone = t.clone();
        clone.cancel();
        assert!(
            t.is_cancelled(),
            "cancellation must be shared across clones"
        );
    }

    #[tokio::test]
    async fn download_of_an_unreachable_host_is_a_network_error() {
        let c = client().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let err = download_verified(
            &c,
            "http://127.0.0.1:1/nothing",
            &dir.path().join("x"),
            None,
            None,
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "network", "got {err}");
    }
}
