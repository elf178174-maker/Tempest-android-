package io.tempest.android.ui

import android.Manifest
import android.content.Intent
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.compose.runtime.getValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import io.tempest.android.ui.theme.TempestTheme

class MainActivity : ComponentActivity() {

    private val viewModel: TempestViewModel by viewModels()

    private val notificationPermission = registerForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { /* Declined only costs the session notification; nothing else breaks. */ }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
        }

        // singleTask, so a vortex:// link that arrives while the app is already
        // running comes through onNewIntent rather than a fresh Activity.
        handleIntent(intent)

        setContent {
            TempestTheme {
                val state by viewModel.state.collectAsStateWithLifecycle()
                TempestScaffold(state = state, viewModel = viewModel)
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleIntent(intent)
    }

    override fun onResume() {
        super.onResume()
        // Configuration changes and process recreation both land here; re-read
        // the core's state rather than trusting whatever the UI last held.
        viewModel.refreshAll()
    }

    /**
     * Take a `vortex://` deep link off an Intent.
     *
     * Any installed app can send one, so nothing is trusted: the URI goes
     * straight to the Rust parser, which validates every field and rebuilds a
     * canonical link before anything acts on it.
     */
    private fun handleIntent(intent: Intent?) {
        if (intent?.action != Intent.ACTION_VIEW) return
        val uri = intent.data?.toString() ?: return
        if (!uri.startsWith("vortex://", ignoreCase = true)) return
        // Consume it, so a configuration change does not relaunch the game.
        intent.data = null
        viewModel.playUri(uri)
    }
}
