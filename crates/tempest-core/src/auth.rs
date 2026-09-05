//! Vortex authentication.
//!
//! The wire protocol is exactly what upstream discovered and is preserved
//! verbatim: a form POST to `/login` with `username`, `password`, `fingerprint`
//! and `fp_token`, answered with a `session_token` cookie on a 2xx or 3xx.
//! What changed is everything around it — no terminal prompts, no config-file
//! storage, and errors carry the server's message so the UI can show it.

use crate::platform::secrets::{SESSION_TOKEN_KEY, USERNAME_KEY};
use crate::platform::PlatformRef;
use crate::{Result, TempestError};
use serde::{Deserialize, Serialize};

pub const BASE: &str = "https://playvortex.io";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Session {
    pub username: String,
}

/// Build the login request body. Split out so it can be asserted on in tests
/// without touching the network.
pub fn login_form<'a>(username: &'a str, password: &'a str) -> Vec<(&'static str, &'a str)> {
    vec![
        ("username", username),
        ("password", password),
        // Vortex's web client sends these; the server accepts empty values,
        // and sending anything device-identifying would be worse for the user.
        ("fingerprint", ""),
        ("fp_token", ""),
    ]
}

pub fn login_url() -> String {
    format!("{BASE}/login")
}

pub fn game_api_url(game_id: u32) -> String {
    format!("{BASE}/api/games/{game_id}")
}

pub fn game_play_url(game_id: u32) -> String {
    format!("{BASE}/games/{game_id}/play")
}

/// The cookie header value for an authenticated request.
pub fn session_cookie(token: &str) -> String {
    format!("session_token={token}")
}

/// Authenticate and persist the session token in the platform secret store.
pub async fn login(platform: &PlatformRef, username: &str, password: &str) -> Result<Session> {
    if username.trim().is_empty() {
        return Err(TempestError::Auth("enter a username".into()));
    }
    if password.is_empty() {
        return Err(TempestError::Auth("enter a password".into()));
    }

    let token = request_token(username.trim(), password).await?;

    let secrets = platform.secrets();
    secrets.set(SESSION_TOKEN_KEY, &token)?;
    secrets.set(USERNAME_KEY, username.trim())?;
    crate::logging::info("auth", format!("signed in as {}", username.trim()));

    Ok(Session { username: username.trim().to_string() })
}

/// Perform the login exchange and return the raw session token.
pub async fn request_token(username: &str, password: &str) -> Result<String> {
    let client = crate::net::no_redirect_client()?;
    let resp = client
        .post(login_url())
        .form(&login_form(username, password))
        .send()
        .await?;

    let status = resp.status();
    if status.is_redirection() || status.is_success() {
        if let Some(cookie) = resp.cookies().find(|c| c.name() == "session_token") {
            let token = cookie.value().to_string();
            if token.is_empty() {
                return Err(TempestError::Auth(
                    "the server returned an empty session token".into(),
                ));
            }
            return Ok(token);
        }
        return Err(TempestError::Auth(
            "the server accepted the login but did not return a session cookie. \
             This usually means Vortex changed its login flow."
                .into(),
        ));
    }

    let body = resp.text().await.unwrap_or_default();
    Err(TempestError::Auth(server_message(status, &body)))
}

/// Extract a human-usable message from an error response.
///
/// Vortex answers with `{"detail": "..."}`; anything else falls back to the
/// status code. The body is never echoed wholesale, because an HTML error page
/// can contain the submitted credentials.
pub fn server_message(status: reqwest::StatusCode, body: &str) -> String {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
        for key in ["detail", "message", "error"] {
            if let Some(msg) = json.get(key).and_then(|v| v.as_str()) {
                if !msg.is_empty() && msg.len() < 300 {
                    return msg.to_string();
                }
            }
        }
    }
    match status.as_u16() {
        401 | 403 => "incorrect username or password".to_string(),
        429 => "too many attempts — wait a moment and try again".to_string(),
        s if (500..600).contains(&s) => {
            format!("Vortex returned a server error (HTTP {s}); try again later")
        }
        s => format!("login failed (HTTP {s})"),
    }
}

/// Current session, if a token is stored.
pub fn current_session(platform: &PlatformRef) -> Result<Option<Session>> {
    let secrets = platform.secrets();
    if secrets.get(SESSION_TOKEN_KEY)?.is_none() {
        return Ok(None);
    }
    Ok(Some(Session {
        username: secrets.get(USERNAME_KEY)?.unwrap_or_default(),
    }))
}

pub fn stored_token(platform: &PlatformRef) -> Result<Option<String>> {
    platform.secrets().get(SESSION_TOKEN_KEY)
}

/// Require a token, with an actionable error when there is none.
pub fn require_token(platform: &PlatformRef) -> Result<String> {
    stored_token(platform)?.ok_or_else(|| {
        TempestError::Auth("you are not signed in — sign in to Vortex first".into())
    })
}

pub fn logout(platform: &PlatformRef) -> Result<()> {
    let secrets = platform.secrets();
    secrets.delete(SESSION_TOKEN_KEY)?;
    secrets.delete(USERNAME_KEY)?;
    crate::logging::info("auth", "signed out");
    Ok(())
}

/// Ask Vortex for the `vortex://` launch link for a game.
///
/// The play page embeds the URI in its HTML. Upstream scanned for the first
/// `vortex://` occurrence and cut at the first quote or whitespace; that is
/// kept, but the result is then run through the validating parser so a
/// malformed or hostile page cannot produce a command line we would forward.
pub async fn fetch_play_link(token: &str, game_id: u32) -> Result<crate::uri::VortexLink> {
    let client = crate::net::client()?;
    let resp = client
        .get(game_play_url(game_id))
        .header("Cookie", session_cookie(token))
        .send()
        .await?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(TempestError::Auth(
            "your Vortex session has expired — sign in again".into(),
        ));
    }
    if !status.is_success() {
        return Err(TempestError::Network(format!(
            "Vortex returned HTTP {status} for the play page of game {game_id}"
        )));
    }

    let html = resp.text().await?;
    let raw = extract_vortex_uri(&html).ok_or_else(|| {
        TempestError::Auth(format!(
            "no vortex:// link on the play page for game {game_id} — \
             the account may not own this game, or the session has expired"
        ))
    })?;
    crate::uri::parse(&raw)
}

/// Pull the first `vortex://...` token out of a page of HTML.
pub fn extract_vortex_uri(html: &str) -> Option<String> {
    let start = html.find("vortex://")?;
    let rest = &html[start..];
    let end = rest
        .find(|c: char| c == '"' || c == '\'' || c == '<' || c == '\\' || c.is_whitespace())
        .unwrap_or(rest.len());
    let candidate = &rest[..end];
    // HTML-escaped ampersands are common in embedded attributes.
    Some(candidate.replace("&amp;", "&"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_form_matches_the_vortex_endpoint_contract() {
        let form = login_form("bob", "hunter2");
        assert_eq!(form.len(), 4);
        assert_eq!(form[0], ("username", "bob"));
        assert_eq!(form[1], ("password", "hunter2"));
        // Empty rather than a real device fingerprint: nothing identifying.
        assert_eq!(form[2], ("fingerprint", ""));
        assert_eq!(form[3], ("fp_token", ""));
    }

    #[test]
    fn urls_are_built_against_the_vortex_origin() {
        assert_eq!(login_url(), "https://playvortex.io/login");
        assert_eq!(game_api_url(12), "https://playvortex.io/api/games/12");
        assert_eq!(game_play_url(12), "https://playvortex.io/games/12/play");
        assert_eq!(session_cookie("abc"), "session_token=abc");
    }

    #[test]
    fn server_message_prefers_the_api_detail_field() {
        let status = reqwest::StatusCode::UNAUTHORIZED;
        assert_eq!(
            server_message(status, r#"{"detail":"Account not activated"}"#),
            "Account not activated"
        );
        assert_eq!(
            server_message(status, "<html>login failed</html>"),
            "incorrect username or password"
        );
        assert!(server_message(reqwest::StatusCode::TOO_MANY_REQUESTS, "").contains("wait"));
        assert!(server_message(reqwest::StatusCode::BAD_GATEWAY, "").contains("server error"));
    }

    #[test]
    fn server_message_does_not_echo_a_whole_html_page() {
        let body = format!("<html>{}</html>", "x".repeat(5000));
        let msg = server_message(reqwest::StatusCode::BAD_REQUEST, &body);
        assert!(msg.len() < 100, "echoed the page body: {} chars", msg.len());
    }

    #[test]
    fn extracts_the_play_uri_from_page_markup() {
        let html = r#"<a class="btn" href="vortex://play?game=4&amp;token=abc123">Play</a>"#;
        let raw = extract_vortex_uri(html).unwrap();
        assert_eq!(raw, "vortex://play?game=4&token=abc123");
        let link = crate::uri::parse(&raw).unwrap();
        assert_eq!(link.game_id, 4);
        assert_eq!(link.token, "abc123");
    }

    #[test]
    fn extraction_handles_single_quotes_and_javascript_strings() {
        assert_eq!(
            extract_vortex_uri("window.location='vortex://play?game=9&token=t9';").unwrap(),
            "vortex://play?game=9&token=t9"
        );
        assert_eq!(
            extract_vortex_uri(r#"var u = "vortex://play?game=1&token=t1";"#).unwrap(),
            "vortex://play?game=1&token=t1"
        );
    }

    #[test]
    fn extraction_returns_none_when_absent() {
        assert!(extract_vortex_uri("<html>Please sign in</html>").is_none());
    }

    #[test]
    fn session_helpers_round_trip_through_a_secret_store() {
        use crate::platform::secrets::{MemorySecretStore, SecretStore};
        let s = MemorySecretStore::default();
        assert!(s.get(SESSION_TOKEN_KEY).unwrap().is_none());
        s.set(SESSION_TOKEN_KEY, "tok").unwrap();
        s.set(USERNAME_KEY, "bob").unwrap();
        assert_eq!(s.get(SESSION_TOKEN_KEY).unwrap().as_deref(), Some("tok"));
        s.delete(SESSION_TOKEN_KEY).unwrap();
        assert!(s.get(SESSION_TOKEN_KEY).unwrap().is_none());
        // Deleting twice is not an error.
        s.delete(SESSION_TOKEN_KEY).unwrap();
    }
}
