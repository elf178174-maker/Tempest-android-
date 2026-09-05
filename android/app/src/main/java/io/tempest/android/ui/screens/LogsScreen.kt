package io.tempest.android.ui.screens

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import io.tempest.android.ui.TempestViewModel
import io.tempest.android.ui.UiState

/**
 * The log view and the "Copy logs" button.
 *
 * Everything shown here has already been through the core's redactor, so a
 * session token cannot appear even in raw Wine output — which does echo the
 * `vortex://` command line it was given. That matters because the whole point
 * of this screen is that the user pastes its contents into a bug report.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LogsScreen(state: UiState, viewModel: TempestViewModel) {
    val context = LocalContext.current

    Column(Modifier.fillMaxSize()) {
        TopAppBar(title = { Text("Logs") })

        Row(
            Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Button(onClick = { copyToClipboard(context, state.logs) }) {
                Text("Copy logs")
            }
            OutlinedButton(onClick = viewModel::loadLogs) { Text("Refresh") }
            OutlinedButton(onClick = viewModel::clearLogs) { Text("Clear") }
        }

        Text(
            "Safe to share: authentication tokens are stripped before anything " +
                "reaches the log.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
        )

        Text(
            text = state.logs.ifBlank { "No log entries yet." },
            style = MaterialTheme.typography.bodySmall,
            fontFamily = FontFamily.Monospace,
            modifier = Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .horizontalScroll(rememberScrollState())
                .padding(16.dp),
        )
    }
}

private fun copyToClipboard(context: Context, text: String) {
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    clipboard.setPrimaryClip(ClipData.newPlainText("Tempest logs", text))
}
