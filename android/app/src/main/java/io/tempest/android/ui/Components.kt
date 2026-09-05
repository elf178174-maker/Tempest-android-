package io.tempest.android.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import io.tempest.android.core.InstallPhase

/**
 * A dismissible error card.
 *
 * Errors are never swallowed: whatever the core said is shown verbatim,
 * including the "what to do next" sentence the diagnostics and runtime layers
 * attach to every failure.
 */
@Composable
fun ErrorCard(message: String, onDismiss: () -> Unit, modifier: Modifier = Modifier) {
    Card(
        modifier = modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.errorContainer,
            contentColor = MaterialTheme.colorScheme.onErrorContainer,
        ),
    ) {
        Row(
            Modifier.padding(start = 16.dp, top = 12.dp, bottom = 12.dp, end = 4.dp),
            verticalAlignment = Alignment.Top,
        ) {
            Text(
                text = message,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.weight(1f),
            )
            IconButton(onClick = onDismiss) {
                Icon(Icons.Default.Close, contentDescription = "Dismiss")
            }
        }
    }
}

/** Human-readable rendering of an install phase, with a bar where one applies. */
@Composable
fun InstallPhaseRow(phase: InstallPhase, modifier: Modifier = Modifier) {
    Column(modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        when (phase) {
            is InstallPhase.Queued -> Text("Waiting…", style = MaterialTheme.typography.bodySmall)

            is InstallPhase.Downloading -> {
                val total = phase.total
                if (total != null && total > 0) {
                    Text(
                        "Downloading ${formatBytes(phase.done)} of ${formatBytes(total)}",
                        style = MaterialTheme.typography.bodySmall,
                    )
                    LinearProgressIndicator(
                        progress = { (phase.done.toFloat() / total).coerceIn(0f, 1f) },
                        modifier = Modifier.fillMaxWidth(),
                    )
                } else {
                    Text(
                        "Downloading ${formatBytes(phase.done)}",
                        style = MaterialTheme.typography.bodySmall,
                    )
                    LinearProgressIndicator(Modifier.fillMaxWidth())
                }
            }

            is InstallPhase.Verifying -> {
                Text("Checking the download…", style = MaterialTheme.typography.bodySmall)
                LinearProgressIndicator(Modifier.fillMaxWidth())
            }

            is InstallPhase.Extracting -> {
                Text("Extracting…", style = MaterialTheme.typography.bodySmall)
                LinearProgressIndicator(Modifier.fillMaxWidth())
            }

            is InstallPhase.Configuring -> {
                Text("${phase.step.replaceFirstChar { it.uppercase() }}…",
                    style = MaterialTheme.typography.bodySmall)
                LinearProgressIndicator(Modifier.fillMaxWidth())
            }

            is InstallPhase.Done -> Text("Installed", style = MaterialTheme.typography.bodySmall)

            is InstallPhase.Failed -> Text(
                phase.error,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        }
    }
}
