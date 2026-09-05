package io.tempest.android.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import coil.compose.AsyncImage
import io.tempest.android.core.Game
import io.tempest.android.ui.ErrorCard
import io.tempest.android.ui.Screen
import io.tempest.android.ui.TempestViewModel
import io.tempest.android.ui.UiState

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LibraryScreen(state: UiState, viewModel: TempestViewModel) {
    Column(Modifier.fillMaxSize()) {
        TopAppBar(
            title = { Text("Games") },
            actions = {
                IconButton(
                    onClick = viewModel::refreshGames,
                    enabled = !state.busy,
                ) {
                    Icon(Icons.Default.Refresh, contentDescription = "Refresh the game list")
                }
            },
        )

        state.error?.let { ErrorCard(it, viewModel::dismissError) }

        if (!state.runtimeReady) {
            RuntimeNotReadyBanner(state, viewModel)
        }

        OutlinedTextField(
            value = state.query,
            onValueChange = viewModel::setQuery,
            label = { Text("Search games") },
            leadingIcon = { Icon(Icons.Default.Search, contentDescription = null) },
            singleLine = true,
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp, vertical = 8.dp),
        )

        if (state.busy) {
            LinearProgressIndicator(Modifier.fillMaxWidth())
            state.gamesProgress?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                )
            }
        }

        val games = state.filteredGames
        when {
            games.isEmpty() && state.games.isEmpty() -> EmptyLibrary(state, viewModel)
            games.isEmpty() -> Box(
                Modifier.fillMaxSize(),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    "No games match \"${state.query}\".",
                    style = MaterialTheme.typography.bodyMedium,
                )
            }

            else -> LazyColumn(
                contentPadding = androidx.compose.foundation.layout.PaddingValues(
                    start = 16.dp, end = 16.dp, bottom = 24.dp,
                ),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                items(games, key = { it.id }) { game ->
                    GameRow(
                        game = game,
                        enabled = !state.busy && state.session?.isActive != true,
                        onPlay = { viewModel.play(game) },
                    )
                }
            }
        }
    }
}

@Composable
private fun EmptyLibrary(state: UiState, viewModel: TempestViewModel) {
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            modifier = Modifier.padding(32.dp),
        ) {
            if (state.busy) {
                CircularProgressIndicator()
                Spacer(Modifier.height(16.dp))
                Text("Looking up your Vortex games…")
            } else {
                Text(
                    "No games loaded yet.",
                    style = MaterialTheme.typography.titleMedium,
                )
                Text(
                    "Tempest reads the catalogue from your Vortex account.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                    modifier = Modifier.padding(top = 4.dp),
                )
                Button(
                    onClick = viewModel::refreshGames,
                    modifier = Modifier.padding(top = 16.dp),
                ) {
                    Text("Load games")
                }
            }
        }
    }
}

@Composable
private fun RuntimeNotReadyBanner(state: UiState, viewModel: TempestViewModel) {
    Card(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 8.dp),
    ) {
        Column(Modifier.padding(16.dp)) {
            Text("Runtime not installed", style = MaterialTheme.typography.titleSmall)
            Text(
                "Games cannot launch until the Windows compatibility runtime is " +
                    "installed. Missing: ${state.status?.missingComponents?.joinToString(", ").orEmpty()}",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )
            Button(
                onClick = { viewModel.navigate(Screen.Runtime) },
                modifier = Modifier.padding(top = 12.dp),
            ) {
                Text("Set up runtime")
            }
        }
    }
}

@Composable
private fun GameRow(game: Game, enabled: Boolean, onPlay: () -> Unit) {
    Card(Modifier.fillMaxWidth()) {
        Row(
            Modifier.padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (game.imageUrl != null) {
                AsyncImage(
                    model = game.imageUrl,
                    contentDescription = null,
                    modifier = Modifier
                        .size(64.dp)
                        .clip(RoundedCornerShape(8.dp)),
                )
                Spacer(Modifier.width(12.dp))
            }
            Column(Modifier.weight(1f)) {
                Text(
                    game.name,
                    style = MaterialTheme.typography.titleMedium,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
                game.description?.let {
                    Text(
                        it,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
            Spacer(Modifier.width(8.dp))
            Button(onClick = onPlay, enabled = enabled) {
                Text("Play")
            }
        }
    }
}
