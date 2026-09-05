//! Session-token storage backed by the Android Keystore.
//!
//! The Rust side cannot reach the hardware keystore, so the actual encryption
//! happens in Kotlin (`SecureStore`) and this module calls into it. Each
//! operation attaches to the VM, invokes a static Kotlin method, and converts
//! the result — a `String` or `null` — back into a Rust value.
//!
//! Failures are surfaced rather than swallowed: if the keystore is unavailable
//! (for example after a device-wide credential reset invalidates the key), the
//! user is told to sign in again instead of seeing an empty game list.

use jni::objects::{GlobalRef, JObject, JValue};
use jni::JavaVM;
use tempest_core::platform::secrets::SecretStore;
use tempest_core::{Result, TempestError};

/// Build a [`SecretStore`] that delegates to the Kotlin callback object.
///
/// The callback is expected to expose:
/// ```text
/// fun secretGet(key: String): String?
/// fun secretSet(key: String, value: String)
/// fun secretDelete(key: String)
/// fun secretDescribe(): String
/// ```
pub fn keystore_backed(vm: &JavaVM, callback: GlobalRef) -> Box<dyn SecretStore> {
    // SAFETY-adjacent note: `JavaVM` is `Send + Sync` and remains valid for the
    // lifetime of the process, so cloning the raw pointer into the closures is
    // sound. `GlobalRef` keeps the Kotlin object alive across GC.
    let vm_get = unsafe { JavaVM::from_raw(vm.get_java_vm_pointer()) }
        .expect("the JavaVM pointer came from a live VM");
    let vm_set = unsafe { JavaVM::from_raw(vm.get_java_vm_pointer()) }
        .expect("the JavaVM pointer came from a live VM");
    let vm_del = unsafe { JavaVM::from_raw(vm.get_java_vm_pointer()) }
        .expect("the JavaVM pointer came from a live VM");

    let cb_get = callback.clone();
    let cb_set = callback.clone();
    let cb_del = callback.clone();

    let description = describe(vm, &callback)
        .unwrap_or_else(|_| "Android Keystore (description unavailable)".to_string());

    Box::new(
        tempest_core::platform::android::CallbackSecretStore::new(
            Box::new(move |key: &str| call_get(&vm_get, &cb_get, key)),
            Box::new(move |key: &str, value: &str| call_set(&vm_set, &cb_set, key, value)),
            Box::new(move |key: &str| call_delete(&vm_del, &cb_del, key)),
            description,
        ),
    )
}

fn call_get(vm: &JavaVM, cb: &GlobalRef, key: &str) -> Result<Option<String>> {
    let mut env = attach(vm)?;
    let jkey = env
        .new_string(key)
        .map_err(|e| TempestError::other(format!("keystore: {e}")))?;
    let value = env
        .call_method(
            cb,
            "secretGet",
            "(Ljava/lang/String;)Ljava/lang/String;",
            &[JValue::Object(&jkey)],
        )
        .and_then(|v| v.l())
        .map_err(|e| keystore_error(&mut env, e))?;

    if value.is_null() {
        return Ok(None);
    }
    let s: String = env
        .get_string(&value.into())
        .map_err(|e| TempestError::other(format!("keystore returned an unreadable string: {e}")))?
        .into();
    Ok(Some(s))
}

fn call_set(vm: &JavaVM, cb: &GlobalRef, key: &str, value: &str) -> Result<()> {
    let mut env = attach(vm)?;
    let jkey = env
        .new_string(key)
        .map_err(|e| TempestError::other(format!("keystore: {e}")))?;
    let jvalue = env
        .new_string(value)
        .map_err(|e| TempestError::other(format!("keystore: {e}")))?;
    env.call_method(
        cb,
        "secretSet",
        "(Ljava/lang/String;Ljava/lang/String;)V",
        &[JValue::Object(&jkey), JValue::Object(&jvalue)],
    )
    .map_err(|e| keystore_error(&mut env, e))?;
    Ok(())
}

fn call_delete(vm: &JavaVM, cb: &GlobalRef, key: &str) -> Result<()> {
    let mut env = attach(vm)?;
    let jkey = env
        .new_string(key)
        .map_err(|e| TempestError::other(format!("keystore: {e}")))?;
    env.call_method(
        cb,
        "secretDelete",
        "(Ljava/lang/String;)V",
        &[JValue::Object(&jkey)],
    )
    .map_err(|e| keystore_error(&mut env, e))?;
    Ok(())
}

fn describe(vm: &JavaVM, cb: &GlobalRef) -> Result<String> {
    let mut env = attach(vm)?;
    let value = env
        .call_method(cb, "secretDescribe", "()Ljava/lang/String;", &[])
        .and_then(|v| v.l())
        .map_err(|e| keystore_error(&mut env, e))?;
    if value.is_null() {
        return Ok("Android Keystore".to_string());
    }
    Ok(env
        .get_string(&value.into())
        .map_err(|e| TempestError::other(e.to_string()))?
        .into())
}

fn attach(vm: &JavaVM) -> Result<jni::AttachGuard<'_>> {
    vm.attach_current_thread()
        .map_err(|e| TempestError::other(format!("could not attach to the JVM: {e}")))
}

/// Turn a pending Java exception into a Rust error and clear it, so the next
/// JNI call on this thread does not abort.
fn keystore_error(env: &mut jni::JNIEnv, e: jni::errors::Error) -> TempestError {
    let mut detail = e.to_string();
    if let Ok(true) = env.exception_check() {
        if let Ok(exception) = env.exception_occurred() {
            let _ = env.exception_clear();
            if let Ok(msg) = env.call_method(&exception, "getMessage", "()Ljava/lang/String;", &[]) {
                if let Ok(obj) = msg.l() {
                    if !obj.is_null() {
                        if let Ok(s) = env.get_string(&obj.into()) {
                            detail = s.into();
                        }
                    }
                }
            }
        }
    }
    TempestError::Auth(format!(
        "the secure credential store could not be used ({detail}). \
         Sign in again to recreate the stored credential."
    ))
}

/// Keeps `JObject` referenced so the import is used on every target.
#[allow(dead_code)]
fn _assert_object_type(o: JObject<'_>) -> bool {
    o.is_null()
}
