package io.tempest.android.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.List
import androidx.compose.material.icons.filled.Build
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import io.tempest.android.ui.screens.DiagnosticsScreen
import io.tempest.android.ui.screens.LibraryScreen
import io.tempest.android.ui.screens.LoginScreen
import io.tempest.android.ui.screens.LogsScreen
import io.tempest.android.ui.screens.RuntimeScreen
import io.tempest.android.ui.screens.SessionScreen
import io.tempest.android.ui.screens.SettingsScreen

@Composable
fun TempestScaffold(state: UiState, viewModel: TempestViewModel) {
    val snackbar = remember { SnackbarHostState() }

    LaunchedEffect(state.message) {
        state.message?.let {
            snackbar.showSnackbar(it)
            viewModel.dismissMessage()
        }
    }

    if (state.loading) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            CircularProgressIndicator()
        }
        return
    }

    // Before sign-in there is nothing to navigate between.
    if (!state.signedIn && state.screen == Screen.Login) {
        LoginScreen(state = state, viewModel = viewModel)
        return
    }

    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        bottomBar = { BottomBar(state, viewModel) },
    ) { padding ->
        Box(Modifier.padding(padding)) {
            when (state.screen) {
                Screen.Login -> LoginScreen(state, viewModel)
                Screen.Library -> LibraryScreen(state, viewModel)
                Screen.Session -> SessionScreen(state, viewModel)
                Screen.Settings -> SettingsScreen(state, viewModel)
                Screen.Runtime -> RuntimeScreen(state, viewModel)
                Screen.Logs -> LogsScreen(state, viewModel)
                Screen.Diagnostics -> DiagnosticsScreen(state, viewModel)
            }
        }
    }
}

@Composable
private fun BottomBar(state: UiState, viewModel: TempestViewModel) {
    NavigationBar {
        NavigationBarItem(
            selected = state.screen == Screen.Library,
            onClick = { viewModel.navigate(Screen.Library) },
            icon = { Icon(Icons.AutoMirrored.Filled.List, contentDescription = null) },
            label = { Text("Games") },
        )
        NavigationBarItem(
            selected = state.screen == Screen.Session,
            onClick = { viewModel.navigate(Screen.Session) },
            icon = { Icon(Icons.Default.PlayArrow, contentDescription = null) },
            label = { Text(if (state.session?.isActive == true) "Running" else "Session") },
        )
        NavigationBarItem(
            selected = state.screen == Screen.Runtime,
            onClick = { viewModel.navigate(Screen.Runtime) },
            icon = { Icon(Icons.Default.Build, contentDescription = null) },
            label = { Text("Runtime") },
        )
        NavigationBarItem(
            selected = state.screen == Screen.Settings,
            onClick = { viewModel.navigate(Screen.Settings) },
            icon = { Icon(Icons.Default.Settings, contentDescription = null) },
            label = { Text("Settings") },
        )
    }
}
