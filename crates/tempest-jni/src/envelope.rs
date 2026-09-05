//! The JSON envelope every bridge call returns.
//!
//! Kotlin never sees an exception thrown from Rust: a failure becomes
//! `{"ok":false,"error":"…","kind":"…"}`, where `kind` is the stable machine
//! code from `TempestError::kind()` so the UI can branch on the failure class
//! (for example, routing `auth` back to the sign-in screen) without matching
//! on English text.

use serde::Serialize;
use tempest_core::TempestError;

pub fn ok_json<T: Serialize>(value: &T) -> String {
    #[derive(Serialize)]
    struct Ok<'a, T: Serialize> {
        ok: bool,
        data: &'a T,
    }
    serde_json::to_string(&Ok {
        ok: true,
        data: value,
    })
    .unwrap_or_else(|e| {
        // Serialising our own types should not fail; if it somehow does, the
        // caller still gets a well-formed envelope describing the problem.
        format!(
            r#"{{"ok":false,"error":{},"kind":"other"}}"#,
            json_string(&format!("could not serialise the response: {e}"))
        )
    })
}

pub fn err_json(error: &TempestError) -> String {
    format!(
        r#"{{"ok":false,"error":{},"kind":{}}}"#,
        json_string(&error.to_string()),
        json_string(error.kind())
    )
}

/// Encode a string as a JSON string literal, so an error message containing a
/// quote or a newline cannot produce malformed JSON.
fn json_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_envelope_wraps_the_payload() {
        let json = ok_json(&serde_json::json!({"a": 1}));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["data"]["a"], 1);
    }

    #[test]
    fn error_envelope_carries_a_stable_kind() {
        let json = err_json(&TempestError::Auth("bad password".into()));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["kind"], "auth");
        assert!(parsed["error"].as_str().unwrap().contains("bad password"));
    }

    #[test]
    fn error_messages_with_quotes_and_newlines_stay_valid_json() {
        let nasty = TempestError::other("he said \"no\"\nand \\ left\ttabbed");
        let json = err_json(&nasty);
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("envelope must remain parseable");
        assert!(parsed["error"].as_str().unwrap().contains("he said \"no\""));
    }

    #[test]
    fn every_error_kind_survives_the_round_trip() {
        for e in [
            TempestError::Config("c".into()),
            TempestError::Network("n".into()),
            TempestError::Auth("a".into()),
            TempestError::Uri("u".into()),
            TempestError::Process("p".into()),
            TempestError::Cancelled,
            TempestError::missing("thing", "reason"),
            TempestError::runtime("comp", "reason"),
            TempestError::Integrity {
                what: "w".into(),
                expected: "e".into(),
                actual: "a".into(),
            },
        ] {
            let expected_kind = e.kind();
            let parsed: serde_json::Value = serde_json::from_str(&err_json(&e)).unwrap();
            assert_eq!(parsed["kind"], expected_kind);
            assert_eq!(parsed["ok"], false);
        }
    }
}
