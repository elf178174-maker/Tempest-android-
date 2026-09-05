//! `vortex://` deep-link parsing.
//!
//! Upstream's parser accepted any `vortex:` URL with `game` and `token` query
//! parameters and passed the *original string* straight to Wine as a command
//! line. On Android the same string arrives from an untrusted source — any
//! installed app or web page can fire an Intent at our exported activity — so
//! this version validates every field and **re-serialises** a canonical URI
//! from the validated parts. Whatever reaches the guest is built by us, not
//! echoed from the caller.

use crate::{Result, TempestError};
use serde::{Deserialize, Serialize};
use url::Url;

pub const SCHEME: &str = "vortex";

/// The maximum plausible length of a session token; anything longer is a
/// malformed or hostile link rather than something worth forwarding.
const MAX_TOKEN_LEN: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VortexLink {
    pub game_id: u32,
    pub token: String,
    /// Extra query parameters that passed validation, preserved in order so a
    /// future Vortex-side addition is not silently dropped.
    pub extra: Vec<(String, String)>,
}

impl VortexLink {
    /// Rebuild a canonical `vortex://play?...` URI from validated components.
    ///
    /// This is what gets handed to `Vortex.exe`; it is percent-encoded by
    /// `url::Url`, so no value can inject an extra parameter or a shell
    /// metacharacter into the command line.
    pub fn to_uri(&self) -> String {
        let mut url = Url::parse("vortex://play").expect("static URI is valid");
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("game", &self.game_id.to_string());
            q.append_pair("token", &self.token);
            for (k, v) in &self.extra {
                q.append_pair(k, v);
            }
        }
        url.to_string()
    }

    /// Display form with the token masked, for the UI and logs.
    pub fn redacted(&self) -> String {
        format!("vortex://play?game={}&token=***", self.game_id)
    }
}

/// Parse and validate a `vortex://` URI.
pub fn parse(uri: &str) -> Result<VortexLink> {
    // Reject absurd input before handing it to the URL parser.
    if uri.len() > 8192 {
        return Err(TempestError::Uri("link is implausibly long".into()));
    }

    let parsed =
        Url::parse(uri.trim()).map_err(|e| TempestError::Uri(format!("not a valid URI: {e}")))?;

    if !parsed.scheme().eq_ignore_ascii_case(SCHEME) {
        return Err(TempestError::Uri(format!(
            "expected the '{SCHEME}' scheme, got '{}'",
            parsed.scheme()
        )));
    }

    let mut game_id = None;
    let mut token = None;
    let mut extra = Vec::new();

    for (key, value) in parsed.query_pairs() {
        match key.as_ref() {
            "game" => {
                game_id = Some(value.parse::<u32>().map_err(|_| {
                    TempestError::Uri(format!("game id '{value}' is not a number"))
                })?);
            }
            "token" => token = Some(value.into_owned()),
            other => {
                // Only forward parameters that are safely representable.
                if is_safe_param_name(other) && value.len() <= 1024 {
                    extra.push((other.to_string(), value.into_owned()));
                } else {
                    crate::logging::warn(
                        "uri",
                        format!("dropping unrecognised parameter '{other}'"),
                    );
                }
            }
        }
    }

    let game_id = game_id.ok_or_else(|| TempestError::Uri("no 'game' parameter".into()))?;
    let token = token.ok_or_else(|| TempestError::Uri("no 'token' parameter".into()))?;

    validate_token(&token)?;

    Ok(VortexLink {
        game_id,
        token,
        extra,
    })
}

/// A session token must look like an opaque credential: printable ASCII with
/// no whitespace, quotes or shell metacharacters.
fn validate_token(token: &str) -> Result<()> {
    if token.is_empty() {
        return Err(TempestError::Uri("the 'token' parameter is empty".into()));
    }
    if token.len() > MAX_TOKEN_LEN {
        return Err(TempestError::Uri(
            "the 'token' parameter is too long".into(),
        ));
    }
    if let Some(bad) = token.chars().find(|c| !is_token_char(*c)) {
        return Err(TempestError::Uri(format!(
            "the 'token' parameter contains a disallowed character ({})",
            bad.escape_default()
        )));
    }
    Ok(())
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '+' | '/' | '=' | ':')
}

fn is_safe_param_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Sanitise a string for use as a single path component.
///
/// Used for anything derived from server-supplied names (game titles, archive
/// entry names). Strips separators, `..`, control characters and Windows
/// reserved names.
pub fn sanitize_filename(input: &str) -> String {
    const RESERVED: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];

    let mut out: String = input
        .chars()
        .map(|c| {
            if c.is_control()
                || matches!(
                    c,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0'
                )
            {
                '_'
            } else {
                c
            }
        })
        .collect();

    out = out.trim().trim_matches('.').to_string();
    if out.is_empty() || out == "." || out == ".." {
        return "unnamed".to_string();
    }
    let stem = out.split('.').next().unwrap_or("").to_ascii_lowercase();
    if RESERVED.contains(&stem.as_str()) {
        out.insert(0, '_');
    }
    out.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_normal_link() {
        let link = parse("vortex://play?game=4&token=abc123").unwrap();
        assert_eq!(link.game_id, 4);
        assert_eq!(link.token, "abc123");
        assert!(link.extra.is_empty());
    }

    #[test]
    fn scheme_is_case_insensitive_because_android_normalises_it() {
        // Android lowercases the scheme of an incoming Intent's data URI, but a
        // pasted link may not be normalised, so accept both.
        assert!(parse("VORTEX://play?game=1&token=t").is_ok());
        assert!(parse("Vortex://play?game=1&token=t").is_ok());
    }

    #[test]
    fn rejects_other_schemes() {
        let e = parse("http://example.com/?game=1&token=t").unwrap_err();
        assert_eq!(e.kind(), "uri");
        assert!(e.to_string().contains("http"));
    }

    #[test]
    fn missing_parameters_name_the_missing_one() {
        assert!(parse("vortex://play?game=4")
            .unwrap_err()
            .to_string()
            .contains("token"));
        assert!(parse("vortex://play?token=x")
            .unwrap_err()
            .to_string()
            .contains("game"));
    }

    #[test]
    fn non_numeric_game_id_is_rejected() {
        let e = parse("vortex://play?game=four&token=t").unwrap_err();
        assert!(e.to_string().contains("not a number"), "{e}");
    }

    #[test]
    fn rejects_tokens_containing_shell_metacharacters() {
        for hostile in [
            "vortex://play?game=1&token=a%3Brm%20-rf%20%2F", // a;rm -rf /
            "vortex://play?game=1&token=%60id%60",           // `id`
            "vortex://play?game=1&token=%24%28whoami%29",    // $(whoami)
            "vortex://play?game=1&token=a%20b",              // embedded space
            "vortex://play?game=1&token=a%22b",              // embedded quote
            "vortex://play?game=1&token=a%0Ab",              // newline
        ] {
            let err = parse(hostile).unwrap_err();
            assert_eq!(err.kind(), "uri", "accepted hostile token: {hostile}");
        }
    }

    #[test]
    fn rejects_empty_and_overlong_tokens() {
        assert!(parse("vortex://play?game=1&token=").is_err());
        let long = "a".repeat(MAX_TOKEN_LEN + 1);
        assert!(parse(&format!("vortex://play?game=1&token={long}")).is_err());
    }

    #[test]
    fn canonical_uri_is_rebuilt_not_echoed() {
        // Parameter order is normalised and the original path/fragment dropped.
        let link = parse("vortex://weird/path?token=tok&game=7#frag").unwrap();
        assert_eq!(link.to_uri(), "vortex://play?game=7&token=tok");
    }

    #[test]
    fn unknown_parameters_survive_if_they_are_well_formed() {
        let link = parse("vortex://play?game=1&token=t&region=eu-west").unwrap();
        assert_eq!(
            link.extra,
            vec![("region".to_string(), "eu-west".to_string())]
        );
        assert!(link.to_uri().contains("region=eu-west"));
    }

    #[test]
    fn hostile_parameter_names_are_dropped() {
        let link = parse("vortex://play?game=1&token=t&a%20b=1").unwrap();
        assert!(link.extra.is_empty());
    }

    #[test]
    fn redacted_form_never_contains_the_token() {
        let link = parse("vortex://play?game=1&token=SECRETVALUE").unwrap();
        assert!(!link.redacted().contains("SECRETVALUE"));
    }

    #[test]
    fn filename_sanitisation() {
        // Separators become underscores, so the result can never be a path.
        let traversal = sanitize_filename("../../etc/passwd");
        assert!(!traversal.contains('/'), "{traversal}");
        assert!(!traversal.contains('\\'), "{traversal}");
        assert_ne!(traversal, "..");
        assert_eq!(traversal, "_.._etc_passwd");
        assert_eq!(sanitize_filename("..\\..\\windows"), "_.._windows");
        assert_eq!(sanitize_filename("Half-Life 2"), "Half-Life 2");
        assert_eq!(sanitize_filename(""), "unnamed");
        assert_eq!(sanitize_filename("   ..  "), "unnamed");
        assert_eq!(sanitize_filename("con"), "_con");
        assert_eq!(sanitize_filename("COM1.txt"), "_COM1.txt");
        assert!(!sanitize_filename("a\0b\nc").contains('\0'));
        assert_eq!(sanitize_filename(&"x".repeat(500)).len(), 200);
    }
}
