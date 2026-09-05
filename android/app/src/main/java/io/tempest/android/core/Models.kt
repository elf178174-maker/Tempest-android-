package io.tempest.android.core

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement

/** The envelope every synchronous bridge call returns. */
@Serializable
data class Envelope(
    val ok: Boolean,
    val data: JsonElement? = null,
    val error: String? = null,
    val kind: String? = null,
)

/** A progress or completion event from an asynchronous operation. */
@Serializable
data class BridgeEvent(
    @SerialName("request_id") val requestId: Long,
    val kind: String,
    val data: JsonElement? = null,
    val error: String? = null,
    @SerialName("error_kind") val errorKind: String? = null,
)

/**
 * A failure reported by the core.
 *
 * [kind] is a stable machine code — "auth", "network", "missing", "runtime",
 * "integrity", "uri", "process", "cancelled" — so the UI can react to the
 * *class* of failure without matching on the message text.
 */
class TempestException(
    override val message: String,
    val kind: String,
) : Exception(message) {

    /** An expired or missing session; the UI should return to sign-in. */
    val isAuthFailure: Boolean get() = kind == "auth"

    /** The user cancelled; not worth showing as an error. */
    val isCancellation: Boolean get() = kind == "cancelled"

    /** Retrying may help. */
    val isTransient: Boolean get() = kind == "network"
}

@Serializable
data class PlatformInfo(
    val kind: String,
    @SerialName("os_description") val osDescription: String,
    @SerialName("cpu_arch") val cpuArch: String,
    @SerialName("device_model") val deviceModel: String? = null,
    @SerialName("needs_x86_translation") val needsX86Translation: Boolean = true,
)

@Serializable
data class AppStatus(
    val version: String,
    @SerialName("signed_in") val signedIn: Boolean,
    val username: String? = null,
    @SerialName("runtime_ready") val runtimeReady: Boolean,
    @SerialName("missing_components") val missingComponents: List<String> = emptyList(),
    val session: SessionSnapshot,
    val platform: PlatformInfo,
    @SerialName("storage_bytes") val storageBytes: Long = 0,
)

@Serializable
data class Game(
    val id: Int,
    val name: String,
    val description: String? = null,
    @SerialName("image_url") val imageUrl: String? = null,
)

@Serializable
data class GameCatalogue(
    val games: List<Game> = emptyList(),
    @SerialName("fetched_at") val fetchedAt: Long = 0,
)

@Serializable
data class SessionSnapshot(
    val state: String = "idle",
    @SerialName("game_id") val gameId: Int? = null,
    @SerialName("game_name") val gameName: String? = null,
    @SerialName("status_text") val statusText: String = "No game running",
    val error: String? = null,
    @SerialName("error_kind") val errorKind: String? = null,
    @SerialName("started_at") val startedAt: Long? = null,
    val pid: Int? = null,
    @SerialName("recent_output") val recentOutput: List<String> = emptyList(),
) {
    val isActive: Boolean
        get() = state == "preparing_prefix" || state == "starting_vortex" || state == "running"

    val isFailed: Boolean get() = state == "failed"
}

@Serializable
data class ComponentStatus(
    val id: String,
    @SerialName("display_name") val displayName: String,
    val required: Boolean,
    val installed: Boolean,
    @SerialName("installed_version") val installedVersion: String? = null,
    @SerialName("available_version") val availableVersion: String,
    @SerialName("approx_bytes") val approxBytes: Long,
    val license: String,
    val upstream: String,
    val purpose: String,
    val sha256: String? = null,
) {
    /** Multi-line literals from the Rust catalogue collapse to one line here. */
    val purposeText: String get() = purpose.split(Regex("\\s+")).joinToString(" ").trim()
}

@Serializable
data class DiagnosticsCheck(
    val name: String,
    val verdict: String,
    val detail: String,
    val fix: String? = null,
)

@Serializable
data class DiagnosticsReport(
    val checks: List<DiagnosticsCheck> = emptyList(),
    val failures: Int = 0,
    val warnings: Int = 0,
    val platform: PlatformInfo,
)

/** What [TempestBridge.parseUri] hands back. Deliberately never the token. */
@Serializable
data class ParsedLink(
    @SerialName("game_id") val gameId: Int,
    val display: String,
)

// --- configuration ---------------------------------------------------------

@Serializable
data class TempestConfig(
    val wine: WineConfig = WineConfig(),
    val launcher: LauncherConfig = LauncherConfig(),
    val graphics: GraphicsConfig = GraphicsConfig(),
    val storage: StorageConfig = StorageConfig(),
)

@Serializable
data class WineConfig(
    val binary: String = "wine",
    val env: Map<String, String> = emptyMap(),
    @SerialName("windows_version") val windowsVersion: String = "win10",
)

@Serializable
data class LauncherConfig(
    @SerialName("filter_wine_noise") val filterWineNoise: Boolean = true,
    @SerialName("auto_update_vortex") val autoUpdateVortex: Boolean = true,
    @SerialName("use_esync") val useEsync: Boolean = true,
    @SerialName("use_fsync") val useFsync: Boolean = false,
    @SerialName("shader_cache") val shaderCache: Boolean = true,
    @SerialName("keep_alive_in_background") val keepAliveInBackground: Boolean = true,
    @SerialName("launch_timeout_secs") val launchTimeoutSecs: Long = 180,
)

@Serializable
data class GraphicsConfig(
    @SerialName("vulkan_driver") val vulkanDriver: String = "auto",
    @SerialName("enable_dxvk") val enableDxvk: Boolean = true,
    @SerialName("enable_vkd3d") val enableVkd3d: Boolean = false,
    @SerialName("dxvk_hud") val dxvkHud: String? = null,
    val display: String = ":0",
)

@Serializable
data class StorageConfig(
    @SerialName("games_dir") val gamesDir: String? = null,
    @SerialName("prune_archives_after_install") val pruneArchivesAfterInstall: Boolean = true,
)

/** Phases reported while a runtime component installs. */
@Serializable
data class InstallProgress(
    val component: String,
    val phase: InstallPhase,
)

@Serializable
sealed class InstallPhase {
    @Serializable
    @SerialName("queued")
    data object Queued : InstallPhase()

    @Serializable
    @SerialName("downloading")
    data class Downloading(val done: Long, val total: Long? = null) : InstallPhase()

    @Serializable
    @SerialName("verifying")
    data object Verifying : InstallPhase()

    @Serializable
    @SerialName("extracting")
    data object Extracting : InstallPhase()

    @Serializable
    @SerialName("configuring")
    data class Configuring(val step: String) : InstallPhase()

    @Serializable
    @SerialName("done")
    data object Done : InstallPhase()

    @Serializable
    @SerialName("failed")
    data class Failed(val error: String, val kind: String) : InstallPhase()
}

/** Progress of the catalogue walk. */
@Serializable
data class GamesProgress(val found: Int, val probed: Int)
