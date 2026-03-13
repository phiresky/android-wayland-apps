package io.github.phiresky.wayland_android;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Intent;
import android.os.IBinder;

import androidx.core.app.NotificationCompat;

/**
 * Foreground service that keeps the compositor process alive.
 * Without this, DeX kills the NativeActivity when more than ~5 windows are open
 * because it's the "oldest" window.
 */
public class CompositorService extends Service {
    private static final String CHANNEL_ID = "compositor";
    private static final int NOTIFICATION_ID = 1;

    @Override
    public void onCreate() {
        super.onCreate();
        createNotificationChannel();
    }

    private static final String ACTION_KILL_ALL = "io.github.phiresky.wayland_android.KILL_ALL";

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        if (ACTION_KILL_ALL.equals(intent != null ? intent.getAction() : null)) {
            android.os.Process.killProcess(android.os.Process.myPid());
            return START_NOT_STICKY;
        }

        Intent tapIntent = new Intent(this, MainActivity.class);
        tapIntent.setFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP);
        PendingIntent pendingIntent = PendingIntent.getActivity(
                this, 0, tapIntent, PendingIntent.FLAG_IMMUTABLE);

        Intent killIntent = new Intent(this, CompositorService.class);
        killIntent.setAction(ACTION_KILL_ALL);
        PendingIntent killPendingIntent = PendingIntent.getService(
                this, 1, killIntent, PendingIntent.FLAG_IMMUTABLE);

        Notification notification = new NotificationCompat.Builder(this, CHANNEL_ID)
                .setContentTitle("Wayland Compositor")
                .setContentText("Running")
                .setSmallIcon(android.R.drawable.ic_menu_manage)
                .setContentIntent(pendingIntent)
                .addAction(android.R.drawable.ic_delete, "Kill All", killPendingIntent)
                .setOngoing(true)
                .build();

        startForeground(NOTIFICATION_ID, notification);
        return START_STICKY;
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    private void createNotificationChannel() {
        NotificationChannel channel = new NotificationChannel(
                CHANNEL_ID, "Compositor", NotificationManager.IMPORTANCE_LOW);
        channel.setDescription("Keeps the Wayland compositor running");
        NotificationManager nm = getSystemService(NotificationManager.class);
        if (nm != null) {
            nm.createNotificationChannel(channel);
        }
    }
}
