package io.tempest.android.service

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.IBinder
import android.os.PowerManager
import android.util.Log
import io.tempest.android.R
import io.tempest.android.core.TempestBridge
import io.tempest.android.ui.MainActivity
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * Keeps a running game alive while the app is not in the foreground.
 *
 * This is not optional polish. The user must switch to the X server app to
 * *see* the game, at which point Tempest is backgrounded; without a foreground
 * service Android would put the process in the frozen cached state and then
 * reclaim it, killing Wine and the guest mid-session. A foreground service with
 * a visible notification is the sanctioned way to say "this work is happening
 * on the user's behalf and they know about it".
 *
 * The service stops itself the moment the session ends, so it never holds
 * resources longer than the game runs.
 */
class GameSessionService : Service() {

    companion object {
        private const val TAG = "GameSessionService"
        private const val CHANNEL_ID = "tempest_session"
        private const val NOTIFICATION_ID = 1001
        private const val POLL_INTERVAL_MS = 2_000L
        private const val WAKE_LOCK_TAG = "tempest:session"

        /** Safety net: a wake lock is never held indefinitely. */
        private const val WAKE_LOCK_TIMEOUT_MS = 6 * 60 * 60 * 1000L

        private const val ACTION_START = "io.tempest.android.action.SESSION_START"
        private const val ACTION_STOP = "io.tempest.android.action.SESSION_STOP"

        const val EXTRA_GAME_NAME = "game_name"

        fun start(context: Context, gameName: String?) {
            val intent = Intent(context, GameSessionService::class.java)
                .setAction(ACTION_START)
                .putExtra(EXTRA_GAME_NAME, gameName)
            context.startForegroundService(intent)
        }

        fun stop(context: Context) {
            context.startService(
                Intent(context, GameSessionService::class.java).setAction(ACTION_STOP),
            )
        }
    }

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private var wakeLock: PowerManager.WakeLock? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        createChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                scope.launch {
                    runCatching { TempestBridge.stop() }
                        .onFailure { Log.w(TAG, "stopping the session failed", it) }
                    stopSelfSafely()
                }
                return START_NOT_STICKY
            }

            else -> {
                val title = intent?.getStringExtra(EXTRA_GAME_NAME) ?: getString(R.string.session_running)
                startForeground(NOTIFICATION_ID, buildNotification(title, getString(R.string.session_starting)))
                acquireWakeLock()
                watchSession(title)
            }
        }

        // Deliberately not START_STICKY: if Android kills this process the guest
        // process is gone too, and silently restarting an empty service would
        // show a notification for a game that is not running.
        return START_NOT_STICKY
    }

    /** Poll the core and tear the service down as soon as the session ends. */
    private fun watchSession(title: String) {
        scope.launch {
            // Give the launch a moment to register before treating "not active"
            // as "already finished".
            delay(POLL_INTERVAL_MS)
            while (true) {
                val active = runCatching { TempestBridge.isSessionActive() }
                    .getOrElse {
                        Log.w(TAG, "could not read the session state", it)
                        false
                    }
                if (!active) {
                    Log.i(TAG, "session ended; stopping the foreground service")
                    stopSelfSafely()
                    return@launch
                }
                notify(title, getString(R.string.session_running))
                delay(POLL_INTERVAL_MS)
            }
        }
    }

    private fun acquireWakeLock() {
        val power = getSystemService(Context.POWER_SERVICE) as PowerManager
        wakeLock = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, WAKE_LOCK_TAG).apply {
            setReferenceCounted(false)
            acquire(WAKE_LOCK_TIMEOUT_MS)
        }
    }

    private fun stopSelfSafely() {
        wakeLock?.let { if (it.isHeld) it.release() }
        wakeLock = null
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onDestroy() {
        wakeLock?.let { if (it.isHeld) it.release() }
        wakeLock = null
        scope.cancel()
        super.onDestroy()
    }

    private fun createChannel() {
        val channel = NotificationChannel(
            CHANNEL_ID,
            getString(R.string.channel_session),
            // Low: the notification is a status indicator and a stop button, not
            // something that should interrupt the user mid-game.
            NotificationManager.IMPORTANCE_LOW,
        ).apply {
            description = getString(R.string.channel_session_description)
            setShowBadge(false)
        }
        (getSystemService(NotificationManager::class.java)).createNotificationChannel(channel)
    }

    private fun notify(title: String, text: String) {
        (getSystemService(NotificationManager::class.java))
            .notify(NOTIFICATION_ID, buildNotification(title, text))
    }

    private fun buildNotification(title: String, text: String): Notification {
        val open = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val stop = PendingIntent.getService(
            this,
            1,
            Intent(this, GameSessionService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE,
        )

        return Notification.Builder(this, CHANNEL_ID)
            .setContentTitle(title)
            .setContentText(text)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentIntent(open)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .addAction(
                // The Icon overload, explicitly: a bare null is ambiguous
                // between Builder(Icon, ...) and Builder(int, ...).
                Notification.Action.Builder(
                    null as android.graphics.drawable.Icon?,
                    getString(R.string.action_stop),
                    stop,
                ).build(),
            )
            .build()
    }

}
