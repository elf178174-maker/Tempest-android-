package io.tempest.android.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import io.tempest.android.ui.ErrorCard
import io.tempest.android.ui.Screen
import io.tempest.android.ui.TempestViewModel
import io.tempest.android.ui.UiState

/**
 * What is running, and why it stopped when it stops.
 *
 * Also carries the manual URI box. Android will not always let a browser hand a
 * custom scheme to an app — some browsers block non-http schemes from a page
 * navigation entirely — so pasting the link has to work as a fallback, not as
 * an afterthought.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SessionScreen(state: UiState, viewModel: TempestViewModel) {
    val session = state.session

    Column(
        Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState()),
    ) {
        TopAppBar(title = { Text("Session") })

        state.error?.let { ErrorCard(it, viewModel::dismissError) }

        Card(
            Modifier
                .fillMaxWidth()
                .padding(16.dp),
        ) {
            Column(Modifier.padding(16.dp)) {
                Text(
                    session?.gameName ?: session?.gameId?.let { "Game $it" } ?: "No game running",
                    style = MaterialTheme.typography.titleMedium,
                )
                Text(
                    session?.statusText ?: "Idle",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 4.dp),
                )

                if (session?.isActive == true) {
                    LinearProgressIndicator(
                        Modifier
                            .fillMaxWidth()
                            .padding(top = 12.dp),
                    )
                    session.pid?.let {
                        Text(
                            "Process $it",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(top = 8.dp),
                        )
                    }
                    Text(
                        "Tempest starts an X server for the game to draw on, and the " +
                            "Termux:X11 viewer should open by itself. If it does not, " +
                            "switch to it manually — Tempest keeps the game running in " +
                            "the background either way.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(top = 8.dp),
                    )
                    Button(
                        onClick = viewModel::stopSession,
                        modifier = Modifier.padding(top = 12.dp),
                    ) {
                        Text("Stop")
                    }
                }

                if (session?.isFailed == true && session.error != null) {
                    Card(
                        colors = CardDefaults.cardColors(
                            containerColor = MaterialTheme.colorScheme.errorContainer,
                            contentColor = MaterialTheme.colorScheme.onErrorContainer,
                        ),
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(top = 12.dp),
                    ) {
                        Text(
                            session.error,
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.padding(12.dp),
                        )
                    }
                    OutlinedButton(
                        onClick = { viewModel.navigate(Screen.Logs) },
                        modifier = Modifier.padding(top = 8.dp),
                    ) {
                        Text("Open the log")
                    }
                }
            }
        }

        ManualLinkCard(state, viewModel)

        val output = session?.recentOutput.orEmpty()
        if (output.isNotEmpty()) {
            Text(
                "Recent output",
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.padding(start = 16.dp, top = 8.dp),
            )
            Card(
                Modifier
                    .fillMaxWidth()
                    .padding(16.dp),
            ) {
                Column(Modifier.padding(12.dp)) {
                    output.takeLast(40).forEach {
                        Text(
                            it,
                            style = MaterialTheme.typography.bodySmall,
                            fontFamily = FontFamily.Monospace,
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun ManualLinkCard(state: UiState, viewModel: TempestViewModel) {
    var uri by remember { mutableStateOf("") }

    Card(
        Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp),
    ) {
        Column(Modifier.padding(16.dp)) {
            Text("Open a Vortex link", style = MaterialTheme.typography.titleSmall)
            Text(
                "Tapping Play on the Vortex website should open Tempest directly. " +
                    "If your browser refuses to hand over a vortex:// link, copy it " +
                    "and paste it here.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )
            OutlinedTextField(
                value = uri,
                onValueChange = { uri = it },
                label = { Text("vortex://…") },
                singleLine = true,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(top = 12.dp),
            )
            Button(
                onClick = {
                    viewModel.playUri(uri.trim())
                    uri = ""
                },
                enabled = uri.trim().startsWith("vortex://", ignoreCase = true) && !state.busy,
                modifier = Modifier.padding(top = 8.dp),
            ) {
                Text("Launch")
            }
        }
    }
}
