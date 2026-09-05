package io.tempest.android.ui.screens

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import io.tempest.android.ui.ErrorCard
import io.tempest.android.ui.Screen
import io.tempest.android.ui.TempestViewModel
import io.tempest.android.ui.UiState
import io.tempest.android.ui.formatBytes

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(state: UiState, viewModel: TempestViewModel) {
    val config = state.config

    Column(
        Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState()),
    ) {
        TopAppBar(title = { Text("Settings") })
        state.error?.let { ErrorCard(it, viewModel::dismissError) }

        Section("Account") {
            state.status?.username?.let {
                Text("Signed in as $it", style = MaterialTheme.typography.bodyMedium)
            }
            OutlinedButton(
                onClick = viewModel::logout,
                modifier = Modifier.padding(top = 8.dp),
            ) {
                Text("Sign out")
            }
        }

        if (config != null) {
            Section("Graphics") {
                Text("Vulkan driver", style = MaterialTheme.typography.titleSmall)
                Text(
                    "Turnip is hardware-accelerated on Qualcomm Adreno GPUs. " +
                        "Lavapipe renders on the CPU: far too slow for a game, but it " +
                        "works everywhere and is the right choice when you are trying " +
                        "to find out whether the rest of the stack is functioning.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(vertical = 4.dp),
                )
                listOf(
                    "auto" to "Automatic",
                    "turnip" to "Turnip (Adreno hardware)",
                    "lavapipe" to "Lavapipe (software)",
                ).forEach { (value, label) ->
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .selectable(
                                selected = config.graphics.vulkanDriver == value,
                                onClick = {
                                    viewModel.updateConfig {
                                        it.copy(graphics = it.graphics.copy(vulkanDriver = value))
                                    }
                                },
                            )
                            .padding(vertical = 4.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        RadioButton(
                            selected = config.graphics.vulkanDriver == value,
                            onClick = null,
                        )
                        Text(label, Modifier.padding(start = 8.dp))
                    }
                }

                SettingSwitch(
                    title = "DXVK",
                    subtitle = "Translate Direct3D 9/10/11 to Vulkan. Almost every game needs this.",
                    checked = config.graphics.enableDxvk,
                ) { checked ->
                    viewModel.updateConfig { it.copy(graphics = it.graphics.copy(enableDxvk = checked)) }
                }

                SettingSwitch(
                    title = "vkd3d-proton",
                    subtitle = "Direct3D 12 support. Off by default: Vortex does not need it, " +
                        "and it adds another component to install.",
                    checked = config.graphics.enableVkd3d,
                ) { checked ->
                    viewModel.updateConfig { it.copy(graphics = it.graphics.copy(enableVkd3d = checked)) }
                }

                SettingSwitch(
                    title = "Show an FPS overlay",
                    subtitle = "Draws DXVK's frame counter over the game.",
                    checked = config.graphics.dxvkHud != null,
                ) { checked ->
                    viewModel.updateConfig {
                        it.copy(graphics = it.graphics.copy(dxvkHud = if (checked) "fps" else null))
                    }
                }
            }

            Section("Performance") {
                SettingSwitch(
                    title = "esync",
                    subtitle = "Cheaper synchronisation between Windows threads.",
                    checked = config.launcher.useEsync,
                ) { checked ->
                    viewModel.updateConfig { it.copy(launcher = it.launcher.copy(useEsync = checked)) }
                }
                SettingSwitch(
                    title = "fsync",
                    subtitle = "Cheaper still, but it needs futex_waitv in the kernel. " +
                        "If games stop launching after enabling this, turn it back off.",
                    checked = config.launcher.useFsync,
                ) { checked ->
                    viewModel.updateConfig { it.copy(launcher = it.launcher.copy(useFsync = checked)) }
                }
                SettingSwitch(
                    title = "Cache compiled shaders",
                    subtitle = "Makes the second and later launches of a game faster.",
                    checked = config.launcher.shaderCache,
                ) { checked ->
                    viewModel.updateConfig { it.copy(launcher = it.launcher.copy(shaderCache = checked)) }
                }
                SettingSwitch(
                    title = "Keep games running in the background",
                    subtitle = "Runs a foreground service so Android does not kill the game " +
                        "when you switch to the X server app.",
                    checked = config.launcher.keepAliveInBackground,
                ) { checked ->
                    viewModel.updateConfig {
                        it.copy(launcher = it.launcher.copy(keepAliveInBackground = checked))
                    }
                }
                SettingSwitch(
                    title = "Hide routine Wine messages",
                    subtitle = "Filters the fixme: lines Wine prints constantly.",
                    checked = config.launcher.filterWineNoise,
                ) { checked ->
                    viewModel.updateConfig {
                        it.copy(launcher = it.launcher.copy(filterWineNoise = checked))
                    }
                }
            }
        }

        Section("Storage") {
            state.status?.let {
                Text(
                    "Tempest is using ${formatBytes(it.storageBytes)}",
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
            if (state.volumes.size > 1) {
                Text(
                    "Where to keep games",
                    style = MaterialTheme.typography.titleSmall,
                    modifier = Modifier.padding(top = 12.dp),
                )
                state.volumes.forEach { volume ->
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .selectable(
                                selected = state.selectedGamesDir == volume.path ||
                                    (state.selectedGamesDir == null && volume == state.volumes.first()),
                                onClick = {
                                    viewModel.setGamesDirectory(
                                        if (volume == state.volumes.first()) null else volume.path,
                                    )
                                },
                            )
                            .padding(vertical = 6.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        RadioButton(
                            selected = state.selectedGamesDir == volume.path ||
                                (state.selectedGamesDir == null && volume == state.volumes.first()),
                            onClick = null,
                        )
                        Column(Modifier.padding(start = 8.dp)) {
                            Text(volume.label, style = MaterialTheme.typography.bodyMedium)
                            Text(
                                "${formatBytes(volume.freeBytes)} free",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
            }
            OutlinedButton(
                onClick = viewModel::clearCache,
                modifier = Modifier.padding(top = 12.dp),
            ) {
                Text("Clear cached downloads")
            }
        }

        Section("Diagnostics") {
            NavigationRow("Runtime components") { viewModel.navigate(Screen.Runtime) }
            HorizontalDivider()
            NavigationRow("Logs") { viewModel.navigate(Screen.Logs) }
            HorizontalDivider()
            NavigationRow("Run diagnostics") { viewModel.navigate(Screen.Diagnostics) }
        }

        Section("About") {
            state.status?.let {
                Text("Tempest ${it.version}", style = MaterialTheme.typography.bodyMedium)
                Text(
                    it.platform.osDescription,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Text(
                    "${it.platform.deviceModel ?: "unknown device"} · ${it.platform.cpuArch}",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Text(
                "An unofficial community project. Not affiliated with, endorsed by, " +
                    "or connected to the operators of Vortex or playvortex.io.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 8.dp),
            )
        }
    }
}

@Composable
private fun Section(title: String, content: @Composable () -> Unit) {
    Card(
        Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 8.dp),
    ) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                title,
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.padding(bottom = 8.dp),
            )
            content()
        }
    }
}

@Composable
private fun SettingSwitch(
    title: String,
    subtitle: String,
    checked: Boolean,
    onChange: (Boolean) -> Unit,
) {
    Row(
        Modifier
            .fillMaxWidth()
            .padding(vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.bodyMedium)
            Text(
                subtitle,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Switch(checked = checked, onCheckedChange = onChange)
    }
}

@Composable
private fun NavigationRow(title: String, onClick: () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(title, style = MaterialTheme.typography.bodyLarge)
    }
}
