package io.tempest.android.core

import android.content.Context
import android.os.Build
import android.util.Log
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.put
import java.util.concurrent.atomic.AtomicLong

/**
 * The single point of contact with the Rust core.
 *
 * Everything crosses as JSON in one envelope shape, so a Rust failure arrives
 * as data rather than as an exception thrown across the boundary. Long-running
 * work (downloads, the catalogue walk, launching a game) is started with a
 * request id and completes through [events]; the caller awaits the matching
 * event rather than blocking a thread.
 */
object TempestBridge {

    private const val TAG = "TempestBridge"

    /** Loaded lazily so a load failure can be reported rather than crashing at class-init. */
    @Volatile
    private var loadError: Throwable? = null

    @Volatile
    private var initialised = false

    private val nextRequestId = AtomicLong(1)

    private val json = Json {
        ignoreUnknownKeys = true
        isLenient = true
        encodeDefaults = true
        explicitNulls = false
    }

    private val _events = MutableSharedFlow<BridgeEvent>(
        replay = 0,
        extraBufferCapacity = 256,
    )

    /** Progress and completion events emitted by in-flight operations. */
    val events: SharedFlow<BridgeEvent> = _events

    private val pending = mutableMapOf<Long, CompletableDeferred<BridgeEvent>>()
    private val pendingLock = Any()

    private lateinit var secureStore: SecureStore

    init {
        try {
            System.loadLibrary("tempest_jni")
        } catch (e: UnsatisfiedLinkError) {
            // A missing native library is a build problem, not a user problem;
            // surfacing it beats an opaque crash on first launch.
            loadError = e
            Log.e(TAG, "native library tempest_jni could not be loaded", e)
        }
    }

    /**
     * Hand the Rust core the paths and device facts only [Context] can answer.
     *
     * Nothing is hard-coded on the Rust side: package data paths differ between
     * users, work profiles, Android versions and OEM builds.
     */
    fun initialise(context: Context): Result<Unit> {
        loadError?.let {
            return Result.failure(
                IllegalStateException(
                    "The native Tempest library is missing from this build. " +
                        "Install an APK produced by the project's CI workflow.",
                    it,
                ),
            )
        }
        if (initialised) return Result.success(Unit)

        val app = context.applicationContext
        secureStore = SecureStore(app)

        val config = buildJsonObject {
            put("files_dir", app.filesDir.absolutePath)
            put("cache_dir", app.cacheDir.absolutePath)
            put("native_lib_dir", app.applicationInfo.nativeLibraryDir)
            // A removable volume, when the device has one and the user picked it.
            StoragePreferences(app).gamesDirectory()?.let { put("games_dir", it) }
            put("sdk_int", Build.VERSION.SDK_INT)
            put("release", Build.VERSION.RELEASE ?: "unknown")
            put("model", Build.MODEL ?: "unknown")
            put("primary_abi", Build.SUPPORTED_ABIS.firstOrNull() ?: "unknown")
        }

        return runCatching {
            val reply = nativeInit(config.toString(), Callback)
            decodeEnvelope<String>(reply)
            initialised = true
        }
    }

    val isInitialised: Boolean get() = initialised

    /** Receives calls from Rust. Must stay `internal`; ProGuard keeps it by name. */
    private object Callback {

        @Suppress("unused") // called from Rust
        fun onEvent(payload: String) {
            val event = runCatching { json.decodeFromString<BridgeEvent>(payload) }
                .getOrElse {
                    Log.e(TAG, "unparseable event from the core: $payload", it)
                    return
                }
            synchronized(pendingLock) {
                // Progress events keep the request open; a completion closes it.
                if (!event.kind.endsWith(".progress")) {
                    pending.remove(event.requestId)?.complete(event)
                }
            }
            _events.tryEmit(event)
        }

        @Suppress("unused") // called from Rust
        fun secretGet(key: String): String? = secureStore.get(key)

        @Suppress("unused") // called from Rust
        fun secretSet(key: String, value: String) = secureStore.set(key, value)

        @Suppress("unused") // called from Rust
        fun secretDelete(key: String) = secureStore.delete(key)

        @Suppress("unused") // called from Rust
        fun secretDescribe(): String = secureStore.describe()
    }

    // -- synchronous calls ---------------------------------------------------

    suspend fun status(): AppStatus = call { nativeStatus() }

    suspend fun components(): List<ComponentStatus> = call { nativeComponents() }

    suspend fun diagnostics(): DiagnosticsReport = call { nativeDiagnostics() }

    suspend fun config(): TempestConfig = call { nativeConfig() }

    suspend fun saveConfig(config: TempestConfig): TempestConfig =
        call { nativeSaveConfig(json.encodeToString(TempestConfig.serializer(), config)) }

    suspend fun cachedGames(): GameCatalogue = call { nativeCachedGames() }

    suspend fun search(query: String): List<Game> = call { nativeSearch(query) }

    suspend fun exportLogs(): String = call { nativeExportLogs() }

    suspend fun clearLogs(): String = call { nativeClearLogs() }

    suspend fun clearCache(): Long = call { nativeClearCache() }

    /** Validate a pasted or received URI without launching anything. */
    suspend fun parseUri(uri: String): ParsedLink = call { nativeParseUri(uri) }

    suspend fun logout(): String = call { nativeLogout() }

    suspend fun stop(): String = call { nativeStop() }

    suspend fun cancel(): String = call { nativeCancel() }

    suspend fun uninstallComponent(id: String): String = call { nativeUninstallComponent(id) }

    /** Cheap enough to poll from a foreground service. */
    fun isSessionActive(): Boolean =
        if (initialised) nativeIsSessionActive() else false

    // -- asynchronous operations --------------------------------------------

    suspend fun login(username: String, password: String): String =
        awaitOperation { id -> nativeLogin(id, username, password) }

    suspend fun refreshGames(): GameCatalogue =
        awaitOperation { id -> nativeRefreshGames(id) }

    suspend fun installComponent(id: String): String =
        awaitOperation { requestId -> nativeInstallComponent(requestId, id) }

    /** Install every required component in dependency order. */
    suspend fun installRequired(): String = installComponent("all")

    suspend fun play(gameId: Int): SessionSnapshot =
        awaitOperation { id -> nativePlay(id, gameId) }

    suspend fun playUri(uri: String): SessionSnapshot =
        awaitOperation { id -> nativePlayUri(id, uri) }

    // -- plumbing ------------------------------------------------------------

    private suspend inline fun <reified T> call(crossinline block: () -> String?): T =
        withContext(Dispatchers.IO) {
            requireInitialised()
            decodeEnvelope<T>(block())
        }

    /**
     * Start an operation and suspend until its completion event arrives.
     *
     * The request is registered *before* the native call, so an operation that
     * finishes immediately cannot complete before anyone is listening.
     */
    private suspend inline fun <reified T> awaitOperation(
        crossinline start: (Long) -> String?,
    ): T = withContext(Dispatchers.IO) {
        requireInitialised()
        val id = nextRequestId.getAndIncrement()
        val deferred = CompletableDeferred<BridgeEvent>()
        synchronized(pendingLock) { pending[id] = deferred }

        try {
            // The synchronous reply only reports whether the work started.
            decodeEnvelope<JsonElement>(start(id))
        } catch (e: Throwable) {
            synchronized(pendingLock) { pending.remove(id) }
            throw e
        }

        val event = deferred.await()
        event.error?.let { throw TempestException(it, event.errorKind ?: "other") }
        val data = event.data
            ?: throw TempestException("the core returned no result", "other")
        json.decodeFromJsonElement<T>(data)
    }

    private fun requireInitialised() {
        check(initialised) { "TempestBridge.initialise() has not been called" }
    }

    /** Unwrap `{"ok":true,"data":…}` / `{"ok":false,"error":…,"kind":…}`. */
    private inline fun <reified T> decodeEnvelope(raw: String?): T {
        if (raw == null) {
            throw TempestException(
                "The native core returned nothing. The device may be out of memory.",
                "other",
            )
        }
        val envelope = json.decodeFromString<Envelope>(raw)
        if (!envelope.ok) {
            throw TempestException(
                envelope.error ?: "the core reported an unspecified failure",
                envelope.kind ?: "other",
            )
        }
        val data = envelope.data
            ?: throw TempestException("the core returned an empty response", "other")
        return json.decodeFromJsonElement<T>(data)
    }

    // -- native declarations -------------------------------------------------

    @JvmStatic private external fun nativeInit(configJson: String, callback: Any): String?
    @JvmStatic private external fun nativeStatus(): String?
    @JvmStatic private external fun nativeComponents(): String?
    @JvmStatic private external fun nativeDiagnostics(): String?
    @JvmStatic private external fun nativeConfig(): String?
    @JvmStatic private external fun nativeSaveConfig(configJson: String): String?
    @JvmStatic private external fun nativeCachedGames(): String?
    @JvmStatic private external fun nativeSearch(query: String): String?
    @JvmStatic private external fun nativeExportLogs(): String?
    @JvmStatic private external fun nativeClearLogs(): String?
    @JvmStatic private external fun nativeClearCache(): String?
    @JvmStatic private external fun nativeParseUri(uri: String): String?
    @JvmStatic private external fun nativeIsSessionActive(): Boolean
    @JvmStatic private external fun nativeLogin(requestId: Long, username: String, password: String): String?
    @JvmStatic private external fun nativeLogout(): String?
    @JvmStatic private external fun nativeRefreshGames(requestId: Long): String?
    @JvmStatic private external fun nativeInstallComponent(requestId: Long, component: String): String?
    @JvmStatic private external fun nativeUninstallComponent(component: String): String?
    @JvmStatic private external fun nativePlay(requestId: Long, gameId: Int): String?
    @JvmStatic private external fun nativePlayUri(requestId: Long, uri: String): String?
    @JvmStatic private external fun nativeStop(): String?
    @JvmStatic private external fun nativeCancel(): String?
}
