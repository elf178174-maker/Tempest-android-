package io.tempest.android.ui

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import io.tempest.android.core.AppStatus
import io.tempest.android.core.ComponentStatus
import io.tempest.android.core.DiagnosticsReport
import io.tempest.android.core.Game
import io.tempest.android.core.InstallPhase
import io.tempest.android.core.SessionSnapshot
import io.tempest.android.core.StorageVolumeOption
import io.tempest.android.core.TempestConfig
import io.tempest.android.core.TempestException
import io.tempest.android.data.TempestRepository
import io.tempest.android.service.GameSessionService
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** Which top-level destination is showing. */
enum class Screen { Login, Library, Session, Settings, Runtime, Logs, Diagnostics }

data class UiState(
    val loading: Boolean = true,
    val screen: Screen = Screen.Login,
    val status: AppStatus? = null,
    val games: List<Game> = emptyList(),
    val query: String = "",
    val components: List<ComponentStatus> = emptyList(),
    val installing: Map<String, InstallPhase> = emptyMap(),
    val gamesProgress: String? = null,
    val config: TempestConfig? = null,
    val diagnostics: DiagnosticsReport? = null,
    val logs: String = "",
    val volumes: List<StorageVolumeOption> = emptyList(),
    val selectedGamesDir: String? = null,
    /** A transient message shown in a snackbar. */
    val message: String? = null,
    /** A blocking error shown as a card, with the failure class for styling. */
    val error: String? = null,
    val errorKind: String? = null,
    val busy: Boolean = false,
) {
    val session: SessionSnapshot? get() = status?.session
    val signedIn: Boolean get() = status?.signedIn == true
    val runtimeReady: Boolean get() = status?.runtimeReady == true
    val filteredGames: List<Game>
        get() = if (query.isBlank()) games else games.filter {
            it.name.contains(query, ignoreCase = true) ||
                it.description?.contains(query, ignoreCase = true) == true
        }
}

class TempestViewModel(app: Application) : AndroidViewModel(app) {

    private val repo = TempestRepository.get(app)

    private val _state = MutableStateFlow(UiState())
    val state: StateFlow<UiState> = _state.asStateFlow()

    private var sessionPoll: Job? = null

    init {
        viewModelScope.launch {
            if (!repo.initialise()) {
                _state.update {
                    it.copy(
                        loading = false,
                        error = repo.initError.value,
                        errorKind = "other",
                    )
                }
                return@launch
            }
            observeProgress()
            refreshAll()
        }
    }

    private fun observeProgress() {
        viewModelScope.launch {
            repo.installProgress.collect { progress ->
                _state.update { it.copy(installing = it.installing + (progress.component to progress.phase)) }
                if (progress.phase is InstallPhase.Done || progress.phase is InstallPhase.Failed) {
                    refreshComponents()
                }
            }
        }
        viewModelScope.launch {
            repo.gamesProgress.collect { p ->
                _state.update {
                    it.copy(gamesProgress = "Found ${p.found} games (checked up to id ${p.probed})")
                }
            }
        }
    }

    fun refreshAll() {
        viewModelScope.launch {
            runGuarded {
                val status = repo.status()
                val games = repo.cachedGames()
                _state.update {
                    it.copy(
                        loading = false,
                        status = status,
                        games = games,
                        screen = when {
                            !status.signedIn -> Screen.Login
                            it.screen == Screen.Login -> Screen.Library
                            else -> it.screen
                        },
                    )
                }
                if (status.session.isActive) startSessionPolling()
            }
        }
    }

    // --- navigation --------------------------------------------------------

    fun navigate(screen: Screen) {
        _state.update { it.copy(screen = screen, message = null) }
        when (screen) {
            Screen.Runtime -> refreshComponents()
            Screen.Settings -> loadSettings()
            Screen.Logs -> loadLogs()
            Screen.Diagnostics -> loadDiagnostics()
            else -> Unit
        }
    }

    fun dismissMessage() = _state.update { it.copy(message = null) }
    fun dismissError() = _state.update { it.copy(error = null, errorKind = null) }

    // --- authentication ----------------------------------------------------

    fun login(username: String, password: String) {
        viewModelScope.launch {
            runGuarded(busy = true) {
                val name = repo.login(username, password)
                _state.update { it.copy(message = "Signed in as $name") }
                refreshAll()
                // A first sign-in almost always wants the game list immediately.
                refreshGames()
            }
        }
    }

    fun logout() {
        viewModelScope.launch {
            runGuarded {
                repo.logout()
                _state.update { it.copy(games = emptyList(), screen = Screen.Login) }
                refreshAll()
            }
        }
    }

    // --- games -------------------------------------------------------------

    fun setQuery(q: String) = _state.update { it.copy(query = q) }

    fun refreshGames() {
        viewModelScope.launch {
            runGuarded(busy = true) {
                val games = repo.refreshGames()
                _state.update {
                    it.copy(
                        games = games,
                        gamesProgress = null,
                        message = if (games.isEmpty()) {
                            "No games are available on this account."
                        } else {
                            "Found ${games.size} game${if (games.size == 1) "" else "s"}"
                        },
                    )
                }
            }
        }
    }

    // --- launching ---------------------------------------------------------

    fun play(game: Game) {
        viewModelScope.launch {
            runGuarded(busy = true) {
                _state.update { it.copy(screen = Screen.Session) }
                repo.play(game.id)
                GameSessionService.start(getApplication(), game.name)
                startSessionPolling()
            }
        }
    }

    /** Launch from a deep link or a pasted URI. */
    fun playUri(uri: String) {
        viewModelScope.launch {
            runGuarded(busy = true) {
                val link = repo.parseUri(uri)
                val name = _state.value.games.firstOrNull { it.id == link.gameId }?.name
                _state.update { it.copy(screen = Screen.Session) }
                repo.playUri(uri)
                GameSessionService.start(getApplication(), name)
                startSessionPolling()
            }
        }
    }

    fun stopSession() {
        viewModelScope.launch {
            runGuarded {
                repo.stop()
                GameSessionService.stop(getApplication())
                refreshAll()
            }
        }
    }

    /**
     * Poll while a session is live.
     *
     * Two seconds is frequent enough for the status line to feel responsive and
     * infrequent enough not to compete with the game for CPU. Polling stops the
     * moment the session ends.
     */
    private fun startSessionPolling() {
        if (sessionPoll?.isActive == true) return
        sessionPoll = viewModelScope.launch {
            while (true) {
                val status = runCatching { repo.status() }.getOrNull()
                if (status != null) {
                    _state.update { it.copy(status = status) }
                    if (!status.session.isActive) {
                        GameSessionService.stop(getApplication())
                        break
                    }
                }
                delay(2_000)
            }
        }
    }

    override fun onCleared() {
        sessionPoll?.cancel()
        super.onCleared()
    }

    // --- runtime -----------------------------------------------------------

    fun refreshComponents() {
        viewModelScope.launch {
            runGuarded {
                _state.update { it.copy(components = repo.components()) }
                _state.update { it.copy(status = repo.status()) }
            }
        }
    }

    fun install(id: String) {
        viewModelScope.launch {
            runGuarded {
                _state.update { it.copy(installing = it.installing + (id to InstallPhase.Queued)) }
                repo.installComponent(id)
                _state.update { it.copy(message = "Installed") }
                refreshComponents()
            }
        }
    }

    fun installRequired() {
        viewModelScope.launch {
            runGuarded {
                repo.installRequired()
                _state.update { it.copy(message = "Runtime ready") }
                refreshComponents()
            }
        }
    }

    fun uninstall(id: String) {
        viewModelScope.launch {
            runGuarded {
                repo.uninstallComponent(id)
                refreshComponents()
            }
        }
    }

    fun cancelInstall() {
        viewModelScope.launch {
            runCatching { repo.cancel() }
            _state.update { it.copy(installing = emptyMap(), message = "Cancelled") }
        }
    }

    // --- settings ----------------------------------------------------------

    private fun loadSettings() {
        viewModelScope.launch {
            runGuarded {
                _state.update {
                    it.copy(
                        config = repo.config(),
                        volumes = repo.storageVolumes(),
                        selectedGamesDir = repo.selectedGamesDirectory(),
                    )
                }
            }
        }
    }

    fun updateConfig(transform: (TempestConfig) -> TempestConfig) {
        val current = _state.value.config ?: return
        val updated = transform(current)
        _state.update { it.copy(config = updated) }
        viewModelScope.launch {
            runGuarded { repo.saveConfig(updated) }
        }
    }

    fun setGamesDirectory(path: String?) {
        repo.setGamesDirectory(path)
        _state.update {
            it.copy(
                selectedGamesDir = path,
                message = "Storage location saved. Restart Tempest for it to take effect.",
            )
        }
    }

    fun clearCache() {
        viewModelScope.launch {
            runGuarded {
                val freed = repo.clearCache()
                _state.update {
                    it.copy(message = "Freed ${formatBytes(freed)} of cached downloads")
                }
                refreshAll()
            }
        }
    }

    // --- diagnostics -------------------------------------------------------

    private fun loadDiagnostics() {
        viewModelScope.launch {
            runGuarded(busy = true) {
                // exportLogs() puts the diagnostics report at the top, so
                // loading it here is what makes this screen's Copy button
                // produce something even if the Logs screen was never opened.
                _state.update { it.copy(diagnostics = repo.diagnostics(), logs = repo.exportLogs()) }
            }
        }
    }

    fun loadLogs() {
        viewModelScope.launch {
            runGuarded { _state.update { it.copy(logs = repo.exportLogs()) } }
        }
    }

    fun clearLogs() {
        viewModelScope.launch {
            runGuarded {
                repo.clearLogs()
                _state.update { it.copy(logs = "", message = "Logs cleared") }
            }
        }
    }

    // --- error handling ----------------------------------------------------

    /**
     * Run a suspending block, turning any failure into visible UI state.
     *
     * Nothing fails silently: an auth failure routes back to sign-in, a
     * cancellation is acknowledged rather than shown as an error, and anything
     * else lands in an error card with the message the core produced.
     */
    private suspend fun runGuarded(busy: Boolean = false, block: suspend () -> Unit) {
        if (busy) _state.update { it.copy(busy = true) }
        try {
            block()
        } catch (e: TempestException) {
            when {
                e.isCancellation -> _state.update { it.copy(message = "Cancelled") }
                e.isAuthFailure -> _state.update {
                    it.copy(screen = Screen.Login, error = e.message, errorKind = e.kind)
                }
                else -> _state.update { it.copy(error = e.message, errorKind = e.kind) }
            }
        } catch (e: Exception) {
            _state.update {
                it.copy(error = e.message ?: e::class.java.simpleName, errorKind = "other")
            }
        } finally {
            if (busy) _state.update { it.copy(busy = false) }
        }
    }
}

fun formatBytes(bytes: Long): String = when {
    bytes >= 1_073_741_824 -> "%.1f GB".format(bytes / 1_073_741_824.0)
    bytes >= 1_048_576 -> "%.0f MB".format(bytes / 1_048_576.0)
    bytes >= 1024 -> "%.0f KB".format(bytes / 1024.0)
    else -> "$bytes B"
}
