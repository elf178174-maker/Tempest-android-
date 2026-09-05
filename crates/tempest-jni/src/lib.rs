//! JNI bridge between the Kotlin application layer and `tempest-core`.
//!
//! # Design
//!
//! Crossing the JNI boundary is not free — each call is a thread-state
//! transition plus marshalling — so the surface here is deliberately small and
//! coarse. Calls fall into three shapes:
//!
//! * **Commands** take a JSON request and return a JSON envelope. One call per
//!   user action, never one per field.
//! * **Long-running work** (downloads, catalogue refresh, launching) runs on a
//!   Tokio runtime owned by this crate and reports back through a single
//!   `TempestBridge.onEvent(String)` callback rather than by being polled from
//!   the UI thread.
//! * **Polling** is limited to `status()`, which the UI already refreshes on a
//!   timer while a session is live.
//!
//! Every function returns a JSON envelope of the form
//! `{"ok":true,"data":…}` or `{"ok":false,"error":…,"kind":…}`, so no Rust
//! panic or error ever crosses the boundary as an exception.

use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::sys::{jboolean, jint, jlong, jstring, JNI_TRUE};
use jni::{JNIEnv, JavaVM};
use serde::Serialize;
use std::sync::{Arc, OnceLock};
use tempest_core::api::Tempest;
use tempest_core::net::CancelToken;
use tempest_core::platform::android::{AndroidContext, AndroidPlatform};
use tempest_core::runtime::{InstallPhase, ProgressSink};
use tempest_core::{PlatformRef, Result, TempestError};

mod envelope;
mod secrets;

use envelope::{err_json, ok_json};

/// Process-wide state. The app is a single Tempest instance; holding it in a
/// static avoids handing Kotlin a raw pointer it could use after free.
struct Bridge {
    app: Tempest,
    runtime: tokio::runtime::Runtime,
    vm: JavaVM,
    /// The `TempestBridge` Kotlin object that receives events.
    callback: GlobalRef,
    cancel: std::sync::Mutex<CancelToken>,
}

static BRIDGE: OnceLock<Bridge> = OnceLock::new();

fn bridge() -> Result<&'static Bridge> {
    BRIDGE.get().ok_or_else(|| {
        TempestError::other("the Tempest core has not been initialised — call nativeInit first")
    })
}

// ---------------------------------------------------------------------------
// Initialisation
// ---------------------------------------------------------------------------

/// `TempestBridge.nativeInit(configJson: String, callback: Any): String`
///
/// `configJson` carries the paths and device facts only `Context` can answer.
#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativeInit(
    mut env: JNIEnv,
    _class: JClass,
    config_json: JString,
    callback: JObject,
) -> jstring {
    let result = (|| -> Result<String> {
        if BRIDGE.get().is_some() {
            // Re-initialising after an Activity restart is normal and harmless.
            return Ok("already-initialised".to_string());
        }

        let raw: String = env
            .get_string(&config_json)
            .map_err(|e| TempestError::other(format!("bad init payload: {e}")))?
            .into();

        #[derive(serde::Deserialize)]
        struct InitConfig {
            files_dir: String,
            cache_dir: String,
            native_lib_dir: String,
            games_dir: Option<String>,
            sdk_int: i32,
            release: String,
            model: String,
            primary_abi: String,
        }
        let cfg: InitConfig =
            serde_json::from_str(&raw).map_err(|e| TempestError::Config(e.to_string()))?;

        let vm = env
            .get_java_vm()
            .map_err(|e| TempestError::other(format!("no JavaVM: {e}")))?;
        let callback_ref = env
            .new_global_ref(&callback)
            .map_err(|e| TempestError::other(format!("could not retain the callback: {e}")))?;

        let secret_store = secrets::keystore_backed(&vm, callback_ref.clone());

        let platform: PlatformRef = Arc::new(AndroidPlatform::new(
            AndroidContext {
                files_dir: cfg.files_dir.into(),
                cache_dir: cfg.cache_dir.into(),
                native_lib_dir: cfg.native_lib_dir.into(),
                games_dir: cfg.games_dir.map(Into::into),
                sdk_int: cfg.sdk_int,
                release: cfg.release,
                model: cfg.model,
                primary_abi: cfg.primary_abi,
            },
            secret_store,
        )?);

        #[cfg(target_os = "android")]
        android_logger::init_once(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Info)
                .with_tag("Tempest"),
        );

        let runtime = tokio::runtime::Builder::new_multi_thread()
            // A phone does not benefit from a thread per core here: the work is
            // network- and disk-bound, and extra threads compete with the game.
            .worker_threads(3)
            .enable_all()
            .thread_name("tempest-core")
            .build()
            .map_err(|e| TempestError::other(format!("could not start the async runtime: {e}")))?;

        let app = Tempest::new(platform)?;

        BRIDGE
            .set(Bridge {
                app,
                runtime,
                vm,
                callback: callback_ref,
                cancel: std::sync::Mutex::new(CancelToken::new()),
            })
            .map_err(|_| TempestError::other("the core was initialised twice"))?;

        Ok("initialised".to_string())
    })();

    reply(&mut env, result)
}

// ---------------------------------------------------------------------------
// Synchronous queries
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativeStatus(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    reply(&mut env, bridge().map(|b| b.app.status()))
}

#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativeComponents(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    reply(&mut env, bridge().map(|b| b.app.component_status()))
}

#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativeDiagnostics(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    reply(&mut env, bridge().map(|b| b.app.diagnostics()))
}

#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativeConfig(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    reply(&mut env, bridge().map(|b| b.app.config()))
}

#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativeSaveConfig(
    mut env: JNIEnv,
    _class: JClass,
    config_json: JString,
) -> jstring {
    let result = (|| {
        let b = bridge()?;
        let raw = jstring_to_string(&mut env, &config_json)?;
        let cfg: tempest_core::config::Config =
            serde_json::from_str(&raw).map_err(|e| TempestError::Config(e.to_string()))?;
        b.app.save_config(&cfg)?;
        Ok(cfg)
    })();
    reply(&mut env, result)
}

#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativeCachedGames(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    reply(&mut env, bridge().map(|b| b.app.cached_games()))
}

#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativeSearch(
    mut env: JNIEnv,
    _class: JClass,
    query: JString,
) -> jstring {
    let result = (|| {
        let b = bridge()?;
        let q = jstring_to_string(&mut env, &query)?;
        Ok(b.app.search(&q))
    })();
    reply(&mut env, result)
}

#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativeExportLogs(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    reply(&mut env, bridge().map(|b| b.app.export_logs()))
}

#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativeClearLogs(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    reply(
        &mut env,
        bridge().map(|b| {
            b.app.clear_logs();
            "cleared"
        }),
    )
}

#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativeClearCache(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    reply(&mut env, bridge().and_then(|b| b.app.clear_cache()))
}

/// Validate a URI without launching, so the UI can reject a bad paste inline.
#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativeParseUri(
    mut env: JNIEnv,
    _class: JClass,
    uri: JString,
) -> jstring {
    let result = (|| {
        let raw = jstring_to_string(&mut env, &uri)?;
        let link = tempest_core::uri::parse(&raw)?;
        // The token never crosses back to Kotlin: the UI has no use for it,
        // and keeping it on this side means it cannot end up in a Bundle,
        // a log, or a crash report.
        #[derive(Serialize)]
        struct SafeLink {
            game_id: u32,
            display: String,
        }
        Ok(SafeLink { game_id: link.game_id, display: link.redacted() })
    })();
    reply(&mut env, result)
}

#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativeIsSessionActive(
    _env: JNIEnv,
    _class: JClass,
) -> jboolean {
    bridge().map(|b| b.app.session().is_active()).unwrap_or(false) as jboolean
}

// ---------------------------------------------------------------------------
// Asynchronous operations
// ---------------------------------------------------------------------------

/// Every async operation reports completion through this event shape.
#[derive(Serialize)]
struct Event<'a, T: Serialize> {
    /// Correlates the event with the call that started it.
    request_id: jlong,
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_kind: Option<String>,
}

fn emit<T: Serialize>(b: &'static Bridge, request_id: jlong, kind: &str, payload: Result<T>) {
    let event = match payload {
        Ok(data) => Event { request_id, kind, data: Some(data), error: None, error_kind: None },
        Err(e) => Event {
            request_id,
            kind,
            data: None,
            error: Some(e.to_string()),
            error_kind: Some(e.kind().to_string()),
        },
    };
    let json = serde_json::to_string(&event)
        .unwrap_or_else(|_| format!(r#"{{"request_id":{request_id},"kind":"{kind}","error":"could not serialise the result"}}"#));
    deliver(b, &json);
}

/// Emit a progress update that is not a completion.
fn emit_progress(b: &'static Bridge, request_id: jlong, kind: &str, data: serde_json::Value) {
    let json = serde_json::json!({
        "request_id": request_id,
        "kind": kind,
        "data": data,
    });
    deliver(b, &json.to_string());
}

/// Call `TempestBridge.onEvent(String)` from whatever thread we are on.
fn deliver(b: &'static Bridge, json: &str) {
    // Worker threads are not attached to the VM; attaching as a daemon means
    // the thread does not keep the VM alive on shutdown.
    let Ok(mut env) = b.vm.attach_current_thread_as_daemon() else {
        tempest_core::logging::error("jni", "could not attach to the JVM to deliver an event");
        return;
    };
    let Ok(payload) = env.new_string(json) else {
        tempest_core::logging::error("jni", "could not allocate the event string");
        return;
    };
    if let Err(e) = env.call_method(
        &b.callback,
        "onEvent",
        "(Ljava/lang/String;)V",
        &[JValue::Object(&payload)],
    ) {
        tempest_core::logging::error("jni", format!("onEvent failed: {e}"));
        // An exception left pending would explode at the next JNI call.
        let _ = env.exception_clear();
    }
}

#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativeLogin(
    mut env: JNIEnv,
    _class: JClass,
    request_id: jlong,
    username: JString,
    password: JString,
) -> jstring {
    let result = (|| {
        let b = bridge()?;
        let user = jstring_to_string(&mut env, &username)?;
        let pass = jstring_to_string(&mut env, &password)?;
        b.runtime.spawn(async move {
            let outcome = b.app.login(&user, &pass).await;
            emit(b, request_id, "login", outcome);
        });
        Ok("started")
    })();
    reply(&mut env, result)
}

#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativeLogout(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    reply(
        &mut env,
        bridge().and_then(|b| b.app.logout().map(|()| "signed-out")),
    )
}

#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativeRefreshGames(
    mut env: JNIEnv,
    _class: JClass,
    request_id: jlong,
) -> jstring {
    let result = (|| {
        let b = bridge()?;
        let cancel = b.cancel.lock().expect("cancel lock").clone();
        b.runtime.spawn(async move {
            let progress = move |found: usize, probed: u32| {
                emit_progress(
                    b,
                    request_id,
                    "games.progress",
                    serde_json::json!({ "found": found, "probed": probed }),
                );
            };
            let outcome = b.app.refresh_games(&cancel, Some(&progress)).await;
            emit(b, request_id, "games", outcome);
        });
        Ok("started")
    })();
    reply(&mut env, result)
}

#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativeInstallComponent(
    mut env: JNIEnv,
    _class: JClass,
    request_id: jlong,
    component: JString,
) -> jstring {
    let result = (|| {
        let b = bridge()?;
        let id = jstring_to_string(&mut env, &component)?;
        let cancel = b.cancel.lock().expect("cancel lock").clone();
        b.runtime.spawn(async move {
            let sink = install_sink(b, request_id);
            let outcome = if id == "all" {
                b.app.install_required(Some(&sink), &cancel).await
            } else {
                b.app.install_component(&id, Some(&sink), &cancel).await
            };
            emit(b, request_id, "install", outcome.map(|()| id));
        });
        Ok("started")
    })();
    reply(&mut env, result)
}

fn install_sink(b: &'static Bridge, request_id: jlong) -> ProgressSink {
    Arc::new(move |id, phase: InstallPhase| {
        emit_progress(
            b,
            request_id,
            "install.progress",
            serde_json::json!({
                "component": id.as_str(),
                "phase": phase,
            }),
        );
    })
}

#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativeUninstallComponent(
    mut env: JNIEnv,
    _class: JClass,
    component: JString,
) -> jstring {
    let result = (|| {
        let b = bridge()?;
        let id = jstring_to_string(&mut env, &component)?;
        b.app.uninstall_component(&id)?;
        Ok(id)
    })();
    reply(&mut env, result)
}

#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativePlay(
    mut env: JNIEnv,
    _class: JClass,
    request_id: jlong,
    game_id: jint,
) -> jstring {
    let result = (|| {
        let b = bridge()?;
        if game_id < 0 {
            return Err(TempestError::other("game id must not be negative"));
        }
        let id = game_id as u32;
        b.runtime.spawn(async move {
            let outcome = b.app.play(id).await.map(|()| b.app.session().snapshot());
            emit(b, request_id, "launch", outcome);
        });
        Ok("started")
    })();
    reply(&mut env, result)
}

#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativePlayUri(
    mut env: JNIEnv,
    _class: JClass,
    request_id: jlong,
    uri: JString,
) -> jstring {
    let result = (|| {
        let b = bridge()?;
        let raw = jstring_to_string(&mut env, &uri)?;
        // Parse on the calling thread so an invalid link is rejected
        // synchronously and the UI can show the error without a round trip.
        let link = tempest_core::uri::parse(&raw)?;
        b.runtime.spawn(async move {
            let outcome = b
                .app
                .play_uri(&raw)
                .map(|_| b.app.session().snapshot());
            emit(b, request_id, "launch", outcome);
        });
        Ok(link.game_id)
    })();
    reply(&mut env, result)
}

#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativeStop(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    reply(&mut env, bridge().and_then(|b| b.app.stop().map(|()| "stopped")))
}

/// Cancel any in-flight download or catalogue refresh, and arm a fresh token so
/// the next operation is not born cancelled.
#[no_mangle]
pub extern "system" fn Java_io_tempest_android_core_TempestBridge_nativeCancel(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let result = (|| {
        let b = bridge()?;
        let mut guard = b.cancel.lock().expect("cancel lock");
        guard.cancel();
        *guard = CancelToken::new();
        Ok("cancelled")
    })();
    reply(&mut env, result)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn jstring_to_string(env: &mut JNIEnv, s: &JString) -> Result<String> {
    if s.is_null() {
        return Err(TempestError::other("a required string argument was null"));
    }
    env.get_string(s)
        .map(Into::into)
        .map_err(|e| TempestError::other(format!("could not read a Java string: {e}")))
}

/// Serialise a result into the JSON envelope and hand it back as a `jstring`.
fn reply<T: Serialize>(env: &mut JNIEnv, result: Result<T>) -> jstring {
    let json = match result {
        Ok(value) => ok_json(&value),
        Err(e) => err_json(&e),
    };
    match env.new_string(&json) {
        Ok(s) => s.into_raw(),
        Err(_) => {
            // Out of memory in the JVM; there is nothing useful left to do
            // except return null, which Kotlin treats as a bridge failure.
            std::ptr::null_mut()
        }
    }
}

/// Silences the unused-import warning on non-Android hosts, where the crate is
/// still compiled so the bridge is covered by `cargo test` on CI runners.
#[allow(dead_code)]
const _: jboolean = JNI_TRUE;
