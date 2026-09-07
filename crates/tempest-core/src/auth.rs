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

/// Everything Vortex handed back at sign-in.
///
/// Upstream kept only the cookie named `session_token` and discarded the rest.
/// That is enough for endpoints which merely read a session, but a site can
/// perfectly well require a second cookie — a CSRF companion, a device id, a
/// signed pair — on the pages that actually do something. Losing it produces
/// exactly the confusing split that showed up on a real device: the game list
/// loaded while the play page redirected to sign-in.
///
/// So the whole jar is kept, and the whole jar is sent back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCookies {
    /// A complete `Cookie:` header value: `name=value; name=value`.
    pub header: String,
    /// The `session_token` value on its own, which is what a `vortex://` link
    /// carries.
    pub token: String,
}

/// Secret-store key for the full cookie header.
pub const COOKIE_HEADER_KEY: &str = "vortex.cookies";

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

    let session = request_session(username.trim(), password).await?;

    let secrets = platform.secrets();
    secrets.set(SESSION_TOKEN_KEY, &session.token)?;
    secrets.set(COOKIE_HEADER_KEY, &session.header)?;
    secrets.set(USERNAME_KEY, username.trim())?;
    crate::logging::info("auth", format!("signed in as {}", username.trim()));

    Ok(Session {
        username: username.trim().to_string(),
    })
}

/// Perform the login exchange and return the raw session token.
pub async fn request_session(username: &str, password: &str) -> Result<SessionCookies> {
    let client = crate::net::no_redirect_client()?;
    let resp = client
        .post(login_url())
        .form(&login_form(username, password))
        .send()
        .await?;

    let status = resp.status();
    if !(status.is_redirection() || status.is_success()) {
        let body = resp.text().await.unwrap_or_default();
        return Err(TempestError::Auth(server_message(status, &body)));
    }

    // Where the server sends us next says whether the credentials were
    // accepted. Bouncing back to the sign-in page — including the `/?next=…`
    // form, which is a redirect to the landing page carrying the page we were
    // trying to reach — means they were not.
    let redirect_target = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let cookies: Vec<(String, String)> = resp
        .cookies()
        .map(|c| (c.name().to_string(), c.value().to_string()))
        .collect();

    // Cookie *names* are protocol, not secrets, and knowing which ones arrived
    // is the difference between diagnosing this in one round trip and guessing.
    crate::logging::info(
        "auth",
        format!(
            "login returned HTTP {status}; cookies set: [{}]{}",
            cookies
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            if redirect_target.is_empty() {
                String::new()
            } else {
                format!("; redirect to {redirect_target}")
            }
        ),
    );

    let token = cookies
        .iter()
        .find(|(name, _)| name == "session_token")
        .map(|(_, value)| value.clone())
        .unwrap_or_default();

    if token.is_empty() {
        if !redirect_target.is_empty() && looks_like_sign_in(&redirect_target) {
            return Err(TempestError::Auth(
                "incorrect username or password — Vortex sent the sign-in page back".into(),
            ));
        }
        return Err(TempestError::Auth(format!(
            "Vortex accepted the request but set no session_token cookie{}. \
             The sign-in flow has probably changed; Settings → Logs lists the \
             cookie names it did set.",
            if cookies.is_empty() {
                String::new()
            } else {
                format!(" (it set {} other cookie(s))", cookies.len())
            }
        )));
    }

    // Even with a token, a redirect back to sign-in means it is an anonymous
    // session rather than ours.
    if looks_like_sign_in(&redirect_target) && !redirect_target.is_empty() {
        return Err(TempestError::Auth(
            "incorrect username or password — Vortex issued a session but sent \
             the sign-in page back with it."
                .into(),
        ));
    }

    Ok(SessionCookies {
        header: cookies
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; "),
        token,
    })
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

/// The `Cookie:` header to send with authenticated requests.
///
/// Falls back to the single `session_token` for a profile signed in before the
/// whole jar was kept, so an existing install keeps working until the next
/// sign-in replaces it.
pub fn stored_cookie_header(platform: &PlatformRef) -> Result<String> {
    let secrets = platform.secrets();
    if let Some(header) = secrets.get(COOKIE_HEADER_KEY)? {
        if !header.trim().is_empty() {
            return Ok(header);
        }
    }
    let token = stored_token(platform)?.ok_or_else(|| {
        TempestError::Auth("you are not signed in — sign in to Vortex first".into())
    })?;
    Ok(session_cookie(&token))
}

/// Require a token, with an actionable error when there is none.
pub fn require_token(platform: &PlatformRef) -> Result<String> {
    stored_token(platform)?
        .ok_or_else(|| TempestError::Auth("you are not signed in — sign in to Vortex first".into()))
}

pub fn logout(platform: &PlatformRef) -> Result<()> {
    let secrets = platform.secrets();
    secrets.delete(SESSION_TOKEN_KEY)?;
    secrets.delete(COOKIE_HEADER_KEY)?;
    secrets.delete(USERNAME_KEY)?;
    crate::logging::info("auth", "signed out");
    Ok(())
}

/// Ask Vortex for the `vortex://` launch link for a game.
///
/// Upstream scanned the play page's HTML for a literal `vortex://` substring.
/// That is kept as the first strategy, but it is no longer the only one — and a
/// failure is no longer reported as "the session has expired", which was a
/// guess, and a misleading one whenever the real cause is that the page changed
/// shape.
pub async fn fetch_play_link(cookies: &str, game_id: u32) -> Result<crate::uri::VortexLink> {
    // Redirects are deliberately *not* followed. When a session is rejected,
    // Vortex answers the play page with a redirect to the sign-in page;
    // following it yields a perfectly good 200 containing no launch link, and
    // the old code reported that as "no vortex:// link" — blaming the page for
    // what is actually an authentication problem.
    let client = crate::net::no_redirect_client()?;
    let resp = client
        .get(game_play_url(game_id))
        .header("Cookie", cookies)
        .send()
        .await?;

    let status = resp.status();

    if status.is_redirection() {
        let target = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        crate::logging::warn(
            "auth",
            format!("play page for game {game_id} redirected to {target}"),
        );
        if looks_like_sign_in(&target) {
            return Err(TempestError::Auth(
                "Vortex sent the request back to the sign-in page, so the stored \
                 session is no longer valid. Sign out and in again from Settings."
                    .into(),
            ));
        }
        return Err(TempestError::Network(format!(
            "the play page for game {game_id} redirected to '{target}', which \
             Tempest does not know how to follow"
        )));
    }

    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(TempestError::Auth(
            "Vortex rejected the stored session — sign out and in again from Settings.".into(),
        ));
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(TempestError::Other(format!(
            "Vortex has no play page for game {game_id}. The cached game list may \
             be stale — refresh it from the Games screen."
        )));
    }
    if !status.is_success() {
        return Err(TempestError::Network(format!(
            "Vortex returned HTTP {status} for the play page of game {game_id}"
        )));
    }

    let html = resp.text().await?;

    if let Some(raw) = extract_vortex_uri(&html) {
        return crate::uri::parse(&raw);
    }

    // Nothing matched. Record what the page looked like — its structure, never
    // its content — so the failure is diagnosable from a log the user can share
    // without hesitation.
    crate::logging::warn("auth", describe_play_page(&html, game_id));

    if looks_like_sign_in_page(&html) {
        return Err(TempestError::Auth(
            "the play page came back as a sign-in form, so the stored session is \
             no longer valid. Sign out and in again from Settings."
                .into(),
        ));
    }

    Err(TempestError::Other(format!(
        "Vortex's play page for game {game_id} loaded, but contains no launch link \
         in any form Tempest recognises. That normally means the website changed. \
         Settings → Logs now holds a description of the page's structure — please \
         send it; it contains no personal data."
    )))
}

/// Whether a redirect target means "you are not signed in".
///
/// The obvious `/login` forms are only half of it. A site just as often bounces
/// you to its landing page carrying the address you wanted, as `/?next=…` —
/// which is what Vortex actually does, and what the first version of this check
/// failed to recognise, reporting a plain auth failure as an unfollowable
/// redirect.
pub fn looks_like_sign_in(url: &str) -> bool {
    let u = url.to_ascii_lowercase();

    if ["/login", "/signin", "/sign-in", "/auth"]
        .iter()
        .any(|p| u.contains(p))
    {
        return true;
    }

    // A "come back here afterwards" parameter is the tell: nothing but an
    // interstitial needs to remember where you were going.
    [
        "next=",
        "redirect=",
        "redirect_to=",
        "return_to=",
        "returnurl=",
    ]
    .iter()
    .any(|p| u.contains(p))
}

/// Whether a *page* is a sign-in form rather than the content that was asked for.
fn looks_like_sign_in_page(html: &str) -> bool {
    let h = html.to_ascii_lowercase();
    let password_field = h.contains("type=\"password\"") || h.contains("type='password'");
    password_field && (h.contains("login") || h.contains("sign in") || h.contains("signin"))
}

/// Describe a page that failed to yield a launch link.
///
/// Deliberately describes rather than quotes. The page belongs to the user's
/// account and may carry their name or other details, none of which is needed
/// to work out why the parser missed; what matters is which shapes are present.
pub fn describe_play_page(html: &str, game_id: u32) -> String {
    let lower = html.to_ascii_lowercase();
    let mut notes: Vec<String> = vec![format!("{} bytes", html.len())];

    // A page title is metadata, not user content.
    if let Some(title) = between(&lower, "<title>", "</title>") {
        notes.push(format!("title {title:?}"));
    }

    for (label, needle) in [
        ("mentions 'vortex:'", "vortex:"),
        ("mentions percent-encoded 'vortex%3a'", "vortex%3a"),
        ("mentions 'launch'", "launch"),
        ("mentions 'token'", "token"),
        ("has a password field", "type=\"password\""),
        ("has a <script> block", "<script"),
        ("mentions 'application/json'", "application/json"),
    ] {
        if lower.contains(needle) {
            notes.push(label.to_string());
        }
    }

    // If "vortex:" appears at all, report how it is *written*. That single fact
    // decides which unescaping the parser needs, and it reveals nothing: every
    // alphanumeric character is replaced with 'x' before it is recorded.
    if let Some(i) = lower.find("vortex:") {
        let shape: String = lower[i..]
            .chars()
            .take(28)
            .map(|c| if c.is_ascii_alphanumeric() { 'x' } else { c })
            .collect();
        notes.push(format!("first 'vortex:' is written {shape:?}"));
    }

    format!("play page for game {game_id}: {}", notes.join("; "))
}

fn between<'a>(haystack: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = haystack.find(open)? + open.len();
    let end = haystack[start..].find(close)? + start;
    Some(haystack[start..end].trim())
}

/// Pull a `vortex://` launch link out of a page.
///
/// Upstream looked for one exact spelling. The same URI can appear in several
/// forms depending on how the page emitted it, and all of them decode to the
/// same link:
///
/// * plain, in an `href`                   `vortex://play?game=4&token=x`
/// * with HTML-escaped ampersands          `vortex://play?game=4&amp;token=x`
/// * inside a JSON or JavaScript string    `vortex:\/\/play?game=4&token=x`
/// * percent-encoded in a query parameter  `vortex%3A%2F%2Fplay%3Fgame%3D4`
/// * with HTML entities for the slashes    `vortex:&#47;&#47;play?...`
pub fn extract_vortex_uri(html: &str) -> Option<String> {
    // Percent-encoded first: decoding it may reveal one of the other forms.
    if let Some(found) =
        find_run(html, "vortex%3A%2F%2F").or_else(|| find_run(html, "vortex%3a%2f%2f"))
    {
        if let Some(uri) = scan_plain(&percent_decode(&found)) {
            return Some(uri);
        }
    }

    let unescaped = html
        .replace("\\/", "/")
        .replace("&#47;", "/")
        .replace("&#x2F;", "/")
        .replace("&#x2f;", "/")
        .replace("&sol;", "/");

    scan_plain(&unescaped).or_else(|| scan_plain(html))
}

/// Find the first `vortex://...` run in already-unescaped text.
fn scan_plain(text: &str) -> Option<String> {
    let start = text.find("vortex://")?;
    let rest = &text[start..];
    let end = rest
        .find(|c: char| matches!(c, '"' | '\'' | '<' | '>' | '\\' | '`' | ')') || c.is_whitespace())
        .unwrap_or(rest.len());
    let candidate = rest[..end].trim_end_matches([',', ';']);
    if candidate.len() <= "vortex://".len() {
        return None;
    }
    Some(candidate.replace("&amp;", "&"))
}

/// Everything from `needle` up to the first delimiter, `needle` included.
fn find_run(text: &str, needle: &str) -> Option<String> {
    let start = text.find(needle)?;
    let rest = &text[start..];
    let end = rest
        .find(|c: char| matches!(c, '"' | '\'' | '<' | '>' | '\\') || c.is_whitespace())
        .unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// Minimal percent-decoding: only `%XX`, which is all a URI needs.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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
        // A bare scheme with nothing after it is not a link.
        assert!(extract_vortex_uri("see vortex:// for details").is_none());
    }

    #[test]
    fn extracts_a_link_escaped_inside_a_javascript_string() {
        // How a URI looks when a page embeds it in JSON or a JS string literal.
        let html = r#"<script>window.__DATA__={"launch":"vortex:\/\/play?game=15&token=abc123"};</script>"#;
        let raw = extract_vortex_uri(html).unwrap();
        let link = crate::uri::parse(&raw).unwrap();
        assert_eq!(link.game_id, 15);
        assert_eq!(link.token, "abc123");
    }

    #[test]
    fn extracts_a_link_written_with_html_entities_for_the_slashes() {
        let html = "<a href=\"vortex:&#47;&#47;play?game=15&amp;token=abc123\">Play</a>";
        let link = crate::uri::parse(&extract_vortex_uri(html).unwrap()).unwrap();
        assert_eq!(link.game_id, 15);
        assert_eq!(link.token, "abc123");

        let hex = "<a href=\"vortex:&#x2F;&#x2F;play?game=15&amp;token=abc123\">Play</a>";
        assert_eq!(
            crate::uri::parse(&extract_vortex_uri(hex).unwrap())
                .unwrap()
                .game_id,
            15
        );
    }

    #[test]
    fn extracts_a_percent_encoded_link_from_a_query_parameter() {
        let html =
            "<a href=\"/redirect?to=vortex%3A%2F%2Fplay%3Fgame%3D15%26token%3Dabc123\">Play</a>";
        let link = crate::uri::parse(&extract_vortex_uri(html).unwrap()).unwrap();
        assert_eq!(link.game_id, 15);
        assert_eq!(link.token, "abc123");
    }

    #[test]
    fn extraction_stops_at_markup_and_punctuation_boundaries() {
        // A link followed by a closing paren or a comma in prose.
        let link = crate::uri::parse(
            &extract_vortex_uri("open (vortex://play?game=1&token=t), then wait").unwrap(),
        )
        .unwrap();
        assert_eq!(link.token, "t");
    }

    #[test]
    fn a_sign_in_page_is_recognised_rather_than_blamed_on_the_link() {
        // The failure the first device report hit: the play page came back as
        // something with no launch link, and the old code guessed "the account
        // may not own this game", which was wrong and unactionable.
        let login = r#"<html><head><title>Sign in</title></head><body>
            <form action="/login"><input type="password" name="password"></form>
            </body></html>"#;
        assert!(looks_like_sign_in_page(login));
        assert!(extract_vortex_uri(login).is_none());

        // A real play page is not mistaken for one.
        let play =
            r#"<html><title>Play</title><a href="vortex://play?game=1&token=t">Play</a></html>"#;
        assert!(!looks_like_sign_in_page(play));
    }

    #[test]
    fn redirect_targets_are_classified() {
        assert!(looks_like_sign_in("/login?next=/games/15/play"));
        assert!(looks_like_sign_in("https://playvortex.io/sign-in"));

        // The exact redirect a real device hit. It names no sign-in path at
        // all — it is the landing page carrying the address we asked for — and
        // the first version of this check called it "a redirect Tempest does
        // not know how to follow" instead of "you are not signed in".
        assert!(looks_like_sign_in("/?next=/games/15/play"));
        assert!(looks_like_sign_in("/?redirect_to=%2Fgames%2F15%2Fplay"));
        assert!(looks_like_sign_in("/home?return_to=/games/1/play"));

        // A genuine destination is not mistaken for one.
        assert!(!looks_like_sign_in("/games/15/launch"));
        assert!(!looks_like_sign_in("https://cdn.playvortex.io/vortex.zip"));
    }

    #[test]
    fn a_session_is_the_whole_cookie_jar_not_one_cookie() {
        // The bug this fixes: keeping only `session_token` and dropping the
        // rest. A public endpoint still answers, so the game list loads and
        // everything looks fine — until a page that actually checks the
        // session redirects to sign-in.
        let session = SessionCookies {
            header: "session_token=abc123; csrftoken=xyz789; device=dev1".into(),
            token: "abc123".into(),
        };

        // Every cookie survives, in a form a Cookie header accepts.
        assert!(session.header.contains("session_token=abc123"));
        assert!(session.header.contains("csrftoken=xyz789"));
        assert!(session.header.contains("device=dev1"));
        assert_eq!(session.header.matches("; ").count(), 2);

        // And the token is still available on its own, because that is what a
        // vortex:// link carries.
        assert_eq!(session.token, "abc123");
    }

    #[test]
    fn stored_cookies_fall_back_to_the_session_token_for_an_older_profile() {
        use crate::platform::secrets::{MemorySecretStore, SecretStore};

        // A profile signed in before the whole jar was kept has only the token.
        let store = MemorySecretStore::default();
        store.set(SESSION_TOKEN_KEY, "legacy-token").unwrap();
        assert_eq!(
            store.get(COOKIE_HEADER_KEY).unwrap(),
            None,
            "the fallback only applies when no jar was stored"
        );
        assert_eq!(session_cookie("legacy-token"), "session_token=legacy-token");
    }

    #[test]
    fn the_page_description_reports_structure_and_never_content() {
        let html = r#"<html><head><title>Play - Vortex</title></head><body>
            <p>Welcome back, Jane Doe (jane@example.com)</p>
            <script>var launch = "vortex:\/\/play?game=15&token=SECRETTOKEN";</script>
            </body></html>"#;
        let described = describe_play_page(html, 15);

        // Useful: it says what shapes are present.
        assert!(described.contains("play page for game 15"), "{described}");
        assert!(described.contains("bytes"), "{described}");
        assert!(described.contains("<script>"), "{described}");
        assert!(described.contains("mentions 'vortex:'"), "{described}");
        assert!(described.contains("mentions 'token'"), "{described}");
        // And it reports how the URI is written, which is the actionable part.
        assert!(
            described.contains("first 'vortex:' is written"),
            "{described}"
        );

        // Safe: nothing from the page itself survives.
        assert!(!described.contains("SECRETTOKEN"), "{described}");
        assert!(!described.contains("Jane"), "{described}");
        assert!(!described.contains("jane@example.com"), "{described}");
        assert!(!described.contains("Welcome"), "{described}");
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
