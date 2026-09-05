//! Diagnostic logging with mandatory redaction.
//!
//! Two sinks: a bounded in-memory ring buffer the UI reads for the "Logs"
//! screen and the "Copy logs" button, and an append-only file. Every line
//! passes through [`redact`] first, so a session token can never reach either
//! sink — including lines that come from Wine's own stdout, which echoes the
//! `vortex://` command line it was given.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

const RING_CAPACITY: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub millis: u64,
    pub level: Level,
    pub tag: String,
    pub message: String,
}

impl LogEntry {
    pub fn format(&self) -> String {
        format!(
            "{} {:<5} [{}] {}",
            format_timestamp(self.millis),
            self.level.as_str(),
            self.tag,
            self.message
        )
    }
}

struct Sink {
    ring: std::collections::VecDeque<LogEntry>,
    file: Option<PathBuf>,
}

fn sink() -> &'static Mutex<Sink> {
    static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();
    SINK.get_or_init(|| {
        Mutex::new(Sink {
            ring: std::collections::VecDeque::with_capacity(RING_CAPACITY),
            file: None,
        })
    })
}

/// Point the file sink at `path`. Safe to call more than once.
pub fn init_file(path: PathBuf) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    if let Ok(mut s) = sink().lock() {
        s.file = Some(path);
    }
}

pub fn log(level: Level, tag: &str, message: impl AsRef<str>) {
    let entry = LogEntry {
        millis: now_millis(),
        level,
        tag: tag.to_string(),
        message: redact(message.as_ref()),
    };
    let line = entry.format();

    if let Ok(mut s) = sink().lock() {
        if s.ring.len() == RING_CAPACITY {
            s.ring.pop_front();
        }
        s.ring.push_back(entry);
        if let Some(path) = &s.file {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                writeln!(f, "{line}").ok();
            }
        }
    }
}

/// A line of output from a guest process (Wine, PRoot, the game).
pub fn guest_line(label: &str, stream: &str, line: &str) {
    log(Level::Debug, &format!("{label}/{stream}"), line);
}

pub fn debug(tag: &str, m: impl AsRef<str>) {
    log(Level::Debug, tag, m)
}
pub fn info(tag: &str, m: impl AsRef<str>) {
    log(Level::Info, tag, m)
}
pub fn warn(tag: &str, m: impl AsRef<str>) {
    log(Level::Warn, tag, m)
}
pub fn error(tag: &str, m: impl AsRef<str>) {
    log(Level::Error, tag, m)
}

/// Snapshot of the ring buffer, oldest first.
pub fn snapshot() -> Vec<LogEntry> {
    sink()
        .lock()
        .map(|s| s.ring.iter().cloned().collect())
        .unwrap_or_default()
}

/// The whole buffer as one blob, for "Copy logs".
pub fn export() -> String {
    snapshot()
        .iter()
        .map(LogEntry::format)
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn clear() {
    if let Ok(mut s) = sink().lock() {
        s.ring.clear();
    }
}

/// Environment variable / field names whose values must never be printed.
pub fn is_sensitive_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    [
        "token",
        "session",
        "password",
        "passwd",
        "secret",
        "cookie",
        "authorization",
        "auth_key",
        "apikey",
        "api_key",
    ]
    .iter()
    .any(|needle| k.contains(needle))
}

/// Mask secrets in an arbitrary line of text.
///
/// Covers the shapes that actually occur in this codebase's output:
/// `token=...` and `session_token=...` in URIs and cookie headers,
/// `Authorization: Bearer ...`, and JSON `"password": "..."`.
///
/// Scanning is by character, not by byte: log lines carry guest output and UI
/// strings that contain non-ASCII, and advancing a byte at a time through a
/// multi-byte character would corrupt it and then panic on the next slice.
pub fn redact(input: &str) -> String {
    /// Keys whose value follows `=`, `:` or `": "` — query strings, cookies,
    /// environment assignments and JSON fields alike.
    const KEYS: &[&str] = &[
        "session_token",
        "sessiontoken",
        "access_token",
        "refresh_token",
        "fp_token",
        "token",
        "password",
        "passwd",
        "secret",
        "api_key",
        "apikey",
    ];
    /// Header names whose entire value is sensitive.
    const HEADER_KEYS: &[&str] = &["authorization", "cookie", "set-cookie", "x-api-key"];

    let lower = input.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;

    while i < input.len() {
        // Only ASCII identifier characters can begin a key, and `i` is always a
        // char boundary, so these comparisons are safe.
        if is_key_start(input, i) {
            if let Some(consumed) = mask_header(input, &lower, i, HEADER_KEYS, &mut out) {
                i = consumed;
                continue;
            }
            if let Some(consumed) = mask_keyed_value(input, &lower, i, KEYS, &mut out) {
                i = consumed;
                continue;
            }
        }
        let ch = input[i..].chars().next().expect("i is a char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// True when position `i` starts a new word, so `tokenizer=x` is not treated as
/// `token`-something.
fn is_key_start(input: &str, i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let prev = input[..i].chars().next_back().unwrap_or(' ');
    !(prev.is_alphanumeric() || prev == '_' || prev == '-')
}

/// `Authorization: <everything to end of line>`.
fn mask_header(
    input: &str,
    lower: &str,
    i: usize,
    keys: &[&str],
    out: &mut String,
) -> Option<usize> {
    for key in keys {
        if !lower[i..].starts_with(key) {
            continue;
        }
        let after = i + key.len();
        if !input[after..].starts_with(':') {
            continue;
        }
        out.push_str(&input[i..=after]);
        out.push_str(" ***");
        let rest = &input[after + 1..];
        let end = rest
            .find(['\n', '"', ','])
            .map(|p| after + 1 + p)
            .unwrap_or(input.len());
        return Some(end);
    }
    None
}

/// `token=<value>`, `"token": "<value>"`.
fn mask_keyed_value(
    input: &str,
    lower: &str,
    i: usize,
    keys: &[&str],
    out: &mut String,
) -> Option<usize> {
    for key in keys {
        if !lower[i..].starts_with(key) {
            continue;
        }
        let after = i + key.len();
        // The key must end here, not run on into a longer identifier.
        if input[after..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }

        // Consume the separator run: `=`, or `"` / `:` / space for JSON.
        let mut sep_end = after;
        let mut saw_separator = false;
        for ch in input[after..].chars() {
            match ch {
                '=' | ':' => {
                    saw_separator = true;
                    sep_end += ch.len_utf8();
                }
                '"' | ' ' => sep_end += ch.len_utf8(),
                _ => break,
            }
        }
        if !saw_separator || sep_end == after {
            continue;
        }

        // The value runs to the next delimiter.
        let mut value_end = sep_end;
        for ch in input[sep_end..].chars() {
            if matches!(ch, '&' | ';' | '"' | ',' | '}' | ' ' | '\'' | '\\' | '\n') {
                break;
            }
            value_end += ch.len_utf8();
        }
        if value_end == sep_end {
            continue;
        }

        out.push_str(&input[i..sep_end]);
        out.push_str("***");
        return Some(value_end);
    }
    None
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Civil date/time from a Unix millisecond timestamp.
///
/// Upstream's version divided the day count by 365 and produced dates that
/// drifted by roughly a day per leap year; this uses the standard days-from-civil
/// inverse, so timestamps in logs the user sends back are trustworthy.
pub fn format_timestamp(millis: u64) -> String {
    let secs = (millis / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{:03}",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60,
        millis % 1000
    )
}

/// Howard Hinnant's `civil_from_days`, days since 1970-01-01 to (y, m, d).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_token_in_a_vortex_uri() {
        let got = redact("Launching vortex://play?game=4&token=abc123DEF then waiting");
        assert!(!got.contains("abc123DEF"), "{got}");
        assert!(got.contains("game=4"), "non-secret query kept: {got}");
        assert!(got.contains("token=***"), "{got}");
        assert!(got.contains("then waiting"), "tail preserved: {got}");
    }

    #[test]
    fn redacts_cookie_and_authorization_headers() {
        assert!(!redact("Cookie: session_token=deadbeef").contains("deadbeef"));
        assert!(!redact("Authorization: Bearer supersecret").contains("supersecret"));
    }

    #[test]
    fn redacts_json_password_and_token_fields() {
        let got = redact(r#"{"username":"bob","password":"hunter2","token":"zzz"}"#);
        assert!(!got.contains("hunter2"), "{got}");
        assert!(!got.contains("zzz"), "{got}");
        assert!(got.contains("bob"), "non-secret field kept: {got}");
    }

    #[test]
    fn leaves_ordinary_lines_untouched() {
        let line = "fixme:d3d11:wined3d_check_device_format unhandled format";
        assert_eq!(redact(line), line);
        // A word merely containing "token" as a substring of a longer
        // identifier must not trigger masking of the rest of the line.
        let s = "tokenizer=fast";
        assert_eq!(redact(s), s);
    }

    #[test]
    fn handles_non_ascii_without_panicking() {
        // Status strings shown to the user contain arrows and other multi-byte
        // characters; scanning them a byte at a time used to slice mid-character.
        let line = "Settings → Runtime: token=SECRET — install first (≈293 MB)";
        let got = redact(line);
        assert!(!got.contains("SECRET"), "{got}");
        assert!(got.contains("Settings → Runtime"), "{got}");
        assert!(got.contains("≈293 MB"), "{got}");
        assert!(got.contains("—"), "{got}");

        // Multi-byte characters inside the secret value itself.
        assert!(!redact("token=päßwörd&x=1").contains("päßwörd"));
        assert_eq!(redact("日本語のテキスト"), "日本語のテキスト");
    }

    #[test]
    fn sensitive_key_detection() {
        assert!(is_sensitive_key("VORTEX_SESSION_TOKEN"));
        assert!(is_sensitive_key("Authorization"));
        assert!(!is_sensitive_key("WINEPREFIX"));
        assert!(!is_sensitive_key("DXVK_HUD"));
    }

    #[test]
    fn ring_buffer_never_stores_a_raw_token() {
        clear();
        info("test", "play uri vortex://play?game=1&token=NEVERLOGTHIS");
        let dump = export();
        assert!(!dump.contains("NEVERLOGTHIS"), "{dump}");
        clear();
    }

    #[test]
    fn timestamp_matches_known_values() {
        // 2024-02-29T12:34:56.789Z — a leap day, which the upstream
        // days/365 arithmetic got wrong.
        assert_eq!(
            format_timestamp(1_709_210_096_789),
            "2024-02-29 12:34:56.789"
        );
        assert_eq!(format_timestamp(0), "1970-01-01 00:00:00.000");
        assert_eq!(
            format_timestamp(1_000_000_000_000),
            "2001-09-09 01:46:40.000"
        );
    }
}
