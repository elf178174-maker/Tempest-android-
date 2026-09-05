//! The JSON envelope every bridge call returns.
//!
//! Kotlin never sees an exception thrown from Rust: a failure becomes
//! `{"ok":false,"error":"…","kind":"…"}`, where `kind` is the stable machine
//! code from `TempestError::kind()` so the UI can branch on the failure class
//! (for example, routing `auth` back to the sign-in screen) without matching
//! on English text.

use serde::Serialize;
use tempest_core::TempestError;

/// Run a bridge call, converting a panic into an error rather than letting it
/// cross the FFI boundary.
///
/// Unwinding out of an `extern "system"` function aborts the process, which for
/// an Android app means the whole thing disappears with no explanation. A
/// panic in the core is a bug either way, but the user is far better served by
/// an error card naming it than by a silent disappearance — and the panic
/// message reaches the log, which is what makes the bug reportable.
pub fn guard<T: Serialize>(what: &str, f: impl FnOnce() -> tempest_core::Result<T>) -> String {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(Ok(value)) => ok_json(&value),
        Ok(Err(e)) => err_json(&e),
        Err(payload) => {
            let detail = panic_message(&payload);
            tempest_core::logging::error("jni", format!("panic in {what}: {detail}"));
            err_json(&TempestError::other(format!(
                "Tempest hit an internal error in {what} ({detail}). \
                 This is a bug — please report it with the log."
            )))
        }
    }
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "no message".to_string()
    }
}

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
    fn guard_passes_success_through() {
        let json = guard("test", || Ok(42));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["data"], 42);
    }

    #[test]
    fn guard_passes_errors_through_with_their_kind() {
        let json = guard("test", || {
            Err::<i32, _>(TempestError::Auth("expired".into()))
        });
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["kind"], "auth");
    }

    #[test]
    fn guard_turns_a_panic_into_a_reportable_error_instead_of_an_abort() {
        // Silence the default hook so the test output stays readable.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let json = guard("the widget", || -> tempest_core::Result<i32> {
            panic!("something went badly wrong")
        });
        std::panic::set_hook(previous);

        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["ok"], false);
        let message = parsed["error"].as_str().unwrap();
        assert!(message.contains("the widget"), "{message}");
        assert!(message.contains("something went badly wrong"), "{message}");
        assert!(message.contains("report it"), "{message}");
    }

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
