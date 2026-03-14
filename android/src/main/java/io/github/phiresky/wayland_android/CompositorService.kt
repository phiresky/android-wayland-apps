package io.github.phiresky.wayland_android

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.os.IBinder
import android.os.Process
import androidx.core.app.NotificationCompat

/**
 * Foreground service that keeps the compositor process alive.
 * Without this, DeX kills the MainActivity when more than ~5 windows are open
 * because it's the "oldest" window.
 */
class CompositorService : Service() {

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_KILL_ALL) {
            Process.killProcess(Process.myPid())
            return START_NOT_STICKY
        }

        val tapIntent = Intent(this, MainActivity::class.java).apply {
            this.flags = Intent.FLAG_ACTIVITY_SINGLE_TOP
        }
        val pendingIntent = PendingIntent.getActivity(
            this, 0, tapIntent, PendingIntent.FLAG_IMMUTABLE
        )

        val killIntent = Intent(this, CompositorService::class.java).apply {
            action = ACTION_KILL_ALL
        }
        val killPendingIntent = PendingIntent.getService(
            this, 1, killIntent, PendingIntent.FLAG_IMMUTABLE
        )

        val notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle("Wayland Compositor")
            .setContentText("Running")
            .setSmallIcon(android.R.drawable.ic_menu_manage)
            .setContentIntent(pendingIntent)
            .addAction(android.R.drawable.ic_delete, "Kill All", killPendingIntent)
            .setOngoing(true)
            .build()

        startForeground(NOTIFICATION_ID, notification)
        return START_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun createNotificationChannel() {
        val channel = NotificationChannel(
            CHANNEL_ID, "Compositor", NotificationManager.IMPORTANCE_LOW
        ).apply {
            description = "Keeps the Wayland compositor running"
        }
        getSystemService(NotificationManager::class.java)?.createNotificationChannel(channel)
    }

    companion object {
        private const val CHANNEL_ID = "compositor"
        private const val NOTIFICATION_ID = 1
        private const val ACTION_KILL_ALL = "io.github.phiresky.wayland_android.KILL_ALL"
    }
}
