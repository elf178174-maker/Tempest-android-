package io.tempest.android.ui.screens

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AssistChip
import androidx.compose.material3.AssistChipDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import io.tempest.android.core.DiagnosticsCheck
import io.tempest.android.ui.ErrorCard
import io.tempest.android.ui.TempestViewModel
import io.tempest.android.ui.UiState

/**
 * A full check of the stack, with a concrete next step for every failure.
 *
 * This is what the user is asked to send back when something does not work on
 * their device, so every row says what was tested, what was found, and what to
 * do about it.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DiagnosticsScreen(state: UiState, viewModel: TempestViewModel) {
    val context = LocalContext.current
    val report = state.diagnostics

    Column(Modifier.fillMaxSize()) {
        TopAppBar(title = { Text("Diagnostics") })
        state.error?.let { ErrorCard(it, viewModel::dismissError) }

        if (report == null) {
            Column(
                Modifier.fillMaxSize(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.Center,
            ) {
                CircularProgressIndicator()
                Text("Checking…", modifier = Modifier.padding(top = 16.dp))
            }
            return
        }

        Row(
            Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            OutlinedButton(onClick = { viewModel.navigate(state.screen) }) { Text("Re-run") }
            OutlinedButton(
                onClick = {
                    val clipboard =
                        context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                    clipboard.setPrimaryClip(
                        ClipData.newPlainText("Tempest diagnostics", state.logs),
                    )
                },
                enabled = state.logs.isNotBlank(),
            ) {
                Text("Copy everything")
            }
        }

        Text(
            "${report.failures} failed, ${report.warnings} warnings",
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(16.dp),
        )

        LazyColumn(
            contentPadding = androidx.compose.foundation.layout.PaddingValues(
                start = 16.dp, end = 16.dp, bottom = 24.dp,
            ),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            items(report.checks) { CheckRow(it) }
        }
    }
}

@Composable
private fun CheckRow(check: DiagnosticsCheck) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                AssistChip(
                    onClick = {},
                    enabled = false,
                    label = { Text(check.verdict.uppercase()) },
                    colors = AssistChipDefaults.assistChipColors(
                        disabledLabelColor = when (check.verdict) {
                            "pass" -> Color(0xFF2E7D32)
                            "warn" -> Color(0xFFB26A00)
                            else -> MaterialTheme.colorScheme.error
                        },
                    ),
                )
                Text(
                    check.name,
                    style = MaterialTheme.typography.titleSmall,
                    modifier = Modifier.padding(start = 12.dp),
                )
            }
            Text(
                check.detail,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 8.dp),
            )
            check.fix?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.padding(top = 8.dp),
                )
            }
        }
    }
}
