package io.tempest.android.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import io.tempest.android.core.ComponentStatus
import io.tempest.android.core.InstallPhase
import io.tempest.android.ui.ErrorCard
import io.tempest.android.ui.InstallPhaseRow
import io.tempest.android.ui.TempestViewModel
import io.tempest.android.ui.UiState
import io.tempest.android.ui.formatBytes

/**
 * Runtime management.
 *
 * The compatibility stack is not bundled in the APK: it is several hundred
 * megabytes and carries licences (Wine is LGPL, the Ubuntu base image is a
 * whole distribution) that are cleanest to satisfy by fetching from the
 * original publishers. Each component shows what it is for, where it comes
 * from, its licence, and the SHA-256 of what was actually installed.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun RuntimeScreen(state: UiState, viewModel: TempestViewModel) {
    val anyInstalling = state.installing.values.any {
        it !is InstallPhase.Done && it !is InstallPhase.Failed
    }

    Column(Modifier.fillMaxSize()) {
        TopAppBar(title = { Text("Runtime") })

        state.error?.let { ErrorCard(it, viewModel::dismissError) }

        LazyColumn(
            contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            item {
                Card(Modifier.fillMaxWidth()) {
                    Column(Modifier.padding(16.dp)) {
                        Text(
                            if (state.runtimeReady) "Runtime ready" else "Runtime incomplete",
                            style = MaterialTheme.typography.titleMedium,
                        )
                        Text(
                            "Windows programs cannot run on Android directly. Tempest " +
                                "unpacks a small Linux filesystem into this app's private " +
                                "storage, runs Wine inside it without root, and translates " +
                                "x86 instructions to ARM. Everything below is downloaded " +
                                "from its original publisher and checked against a known " +
                                "SHA-256 before use.",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(top = 8.dp),
                        )
                        Row(
                            Modifier.padding(top = 12.dp),
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                        ) {
                            Button(
                                onClick = viewModel::installRequired,
                                enabled = !state.runtimeReady && !anyInstalling,
                            ) {
                                Text("Install everything required")
                            }
                            if (anyInstalling) {
                                OutlinedButton(onClick = viewModel::cancelInstall) {
                                    Text("Cancel")
                                }
                            }
                        }
                        if (!state.runtimeReady) {
                            Text(
                                "About 400 MB in total. Use Wi-Fi if you can.",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier.padding(top = 8.dp),
                            )
                        }
                    }
                }
            }

            items(state.components, key = { it.id }) { component ->
                ComponentCard(
                    component = component,
                    phase = state.installing[component.id],
                    busy = anyInstalling,
                    onInstall = { viewModel.install(component.id) },
                    onUninstall = { viewModel.uninstall(component.id) },
                )
            }
        }
    }
}

@Composable
private fun ComponentCard(
    component: ComponentStatus,
    phase: InstallPhase?,
    busy: Boolean,
    onInstall: () -> Unit,
    onUninstall: () -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }

    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(component.displayName, style = MaterialTheme.typography.titleSmall)
                    Text(
                        buildString {
                            append(component.availableVersion)
                            append(" · ")
                            append(formatBytes(component.approxBytes))
                        },
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                AssistChip(
                    onClick = {},
                    enabled = false,
                    label = {
                        Text(
                            when {
                                component.installed -> "Installed"
                                component.required -> "Required"
                                else -> "Optional"
                            },
                        )
                    },
                )
            }

            Text(
                component.purposeText,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 8.dp),
            )

            phase?.let {
                InstallPhaseRow(it, Modifier.padding(top = 12.dp))
            }

            Row(
                Modifier
                    .fillMaxWidth()
                    .padding(top = 12.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (!component.installed) {
                    Button(onClick = onInstall, enabled = !busy) { Text("Install") }
                } else {
                    OutlinedButton(onClick = onUninstall, enabled = !busy) { Text("Remove") }
                }
                TextButton(onClick = { expanded = !expanded }) {
                    Text(if (expanded) "Less" else "Details")
                }
            }

            if (expanded) {
                Column(Modifier.padding(top = 8.dp)) {
                    DetailRow("Source", component.upstream)
                    DetailRow("Licence", component.license)
                    component.installedVersion?.let { DetailRow("Installed", it) }
                    component.sha256?.let { DetailRow("SHA-256", it) }
                }
            }
        }
    }
}

@Composable
private fun DetailRow(label: String, value: String) {
    Column(Modifier.padding(bottom = 6.dp)) {
        Text(label, style = MaterialTheme.typography.labelSmall)
        Text(
            value,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}
