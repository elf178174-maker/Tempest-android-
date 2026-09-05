package io.tempest.android.data

import android.content.Context
import io.tempest.android.core.AppStatus
import io.tempest.android.core.BridgeEvent
import io.tempest.android.core.ComponentStatus
import io.tempest.android.core.DiagnosticsReport
import io.tempest.android.core.Game
import io.tempest.android.core.GamesProgress
import io.tempest.android.core.InstallProgress
import io.tempest.android.core.SessionSnapshot
import io.tempest.android.core.StoragePreferences
import io.tempest.android.core.StorageVolumeOption
import io.tempest.android.core.TempestBridge
import io.tempest.android.core.TempestConfig
import io.tempest.android.core.TempestException
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.mapNotNull
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.decodeFromJsonElement

/**
 * The app's single source of truth, sitting between the Compose layer and the
 * JNI bridge.
 *
 * Every method here is a suspend function or a flow: nothing that touches the
 * core runs on the main thread, and the UI observes state rather than polling
 * across JNI.
 */
class TempestRepository(context: Context) {

    private val appContext = context.applicationContext
    private val storagePrefs = StoragePreferences(appContext)
    // Must match the bridge's configuration: InstallPhase is tagged "phase".
    private val json = Json {
        ignoreUnknownKeys = true
        isLenient = true
        classDiscriminator = "phase"
    }

    private val _initError = MutableStateFlow<String?>(null)

    /** Non-null when the core could not start; the UI shows this instead of a blank screen. */
    val initError: StateFlow<String?> = _initError

    fun initialise(): Boolean {
        val result = TempestBridge.initialise(appContext)
        _initError.value = result.exceptionOrNull()?.let {
            it.message ?: "The Tempest core failed to start."
        }
        return result.isSuccess
    }

    // --- state ------------------------------------------------------------

    suspend fun status(): AppStatus = TempestBridge.status()

    suspend fun sessionSnapshot(): SessionSnapshot = status().session

    fun isSessionActive(): Boolean = TempestBridge.isSessionActive()

    // --- authentication ---------------------------------------------------

    suspend fun login(username: String, password: String): String =
        TempestBridge.login(username, password)

    suspend fun logout() {
        TempestBridge.logout()
    }

    // --- games ------------------------------------------------------------

    suspend fun cachedGames(): List<Game> = TempestBridge.cachedGames().games

    suspend fun refreshGames(): List<Game> = TempestBridge.refreshGames().games

    suspend fun search(query: String): List<Game> = TempestBridge.search(query)

    /** Progress of the catalogue walk, for a determinate indicator. */
    val gamesProgress: Flow<GamesProgress> = TempestBridge.events
        .filter { it.kind == "games.progress" }
        .mapNotNull { event -> event.data?.let { json.decodeFromJsonElement<GamesProgress>(it) } }

    // --- runtime ----------------------------------------------------------

    suspend fun components(): List<ComponentStatus> = TempestBridge.components()

    suspend fun installComponent(id: String) {
        TempestBridge.installComponent(id)
    }

    suspend fun installRequired() {
        TempestBridge.installRequired()
    }

    suspend fun uninstallComponent(id: String) {
        TempestBridge.uninstallComponent(id)
    }

    suspend fun cancel() {
        TempestBridge.cancel()
    }

    /** Per-component install progress. */
    val installProgress: Flow<InstallProgress> = TempestBridge.events
        .filter { it.kind == "install.progress" }
        .mapNotNull { event -> event.data?.let { json.decodeFromJsonElement<InstallProgress>(it) } }

    /** Terminal outcome of an install, successful or not. */
    val installOutcome: Flow<Result<String>> = TempestBridge.events
        .filter { it.kind == "install" }
        .map { it.toResult() }

    // --- launching --------------------------------------------------------

    suspend fun play(gameId: Int): SessionSnapshot = TempestBridge.play(gameId)

    suspend fun playUri(uri: String): SessionSnapshot = TempestBridge.playUri(uri)

    suspend fun parseUri(uri: String) = TempestBridge.parseUri(uri)

    suspend fun stop() {
        TempestBridge.stop()
    }

    // --- settings ---------------------------------------------------------

    suspend fun config(): TempestConfig = TempestBridge.config()

    suspend fun saveConfig(config: TempestConfig): TempestConfig =
        TempestBridge.saveConfig(config)

    fun storageVolumes(): List<StorageVolumeOption> = storagePrefs.availableVolumes()

    fun selectedGamesDirectory(): String? = storagePrefs.gamesDirectory()

    /**
     * Choose where game payloads live.
     *
     * The core reads this at start-up, so a change only takes full effect on
     * the next launch; the UI says so rather than silently doing half the job.
     */
    fun setGamesDirectory(path: String?) = storagePrefs.setGamesDirectory(path)

    // --- diagnostics ------------------------------------------------------

    suspend fun diagnostics(): DiagnosticsReport = TempestBridge.diagnostics()

    suspend fun exportLogs(): String = TempestBridge.exportLogs()

    suspend fun clearLogs() {
        TempestBridge.clearLogs()
    }

    suspend fun clearCache(): Long = TempestBridge.clearCache()

    companion object {
        @Volatile
        private var instance: TempestRepository? = null

        fun get(context: Context): TempestRepository =
            instance ?: synchronized(this) {
                instance ?: TempestRepository(context).also { instance = it }
            }
    }
}

private fun BridgeEvent.toResult(): Result<String> =
    if (error != null) {
        Result.failure(TempestException(error, errorKind ?: "other"))
    } else {
        Result.success(data?.toString() ?: "")
    }
