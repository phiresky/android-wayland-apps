package io.github.phiresky.wayland_android;

import android.Manifest;
import android.app.Activity;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.PixelFormat;
import android.graphics.Typeface;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.Environment;
import android.provider.Settings;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.View;
import android.view.WindowManager;
import android.widget.TextView;

/**
 * Main entry point for the Wayland compositor.
 * Loads the native library and calls nativeInit() to start the compositor
 * on a background thread. Also hosts setup and status overlays.
 */
public class MainActivity extends Activity {
    private static volatile TextView sStatusView;
    private static volatile String sLastStatus = "";
    private static boolean sCompositorStarted = false;
    private boolean needsSetup = false;
    private boolean overlayShown = false;

    static {
        System.loadLibrary("android_wayland_launcher");
    }

    // Native method: initializes compositor, returns true if first-run setup is needed.
    private static native boolean nativeInit(Activity activity);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        // Force the window to draw a frame, which dismisses the splash screen.
        View placeholder = new View(this);
        placeholder.setBackgroundColor(0xFF111111);
        setContentView(placeholder);
        reportFullyDrawn();

        // Initialize native compositor (only on first create, not config changes).
        if (savedInstanceState == null) {
            if (sCompositorStarted) {
                // Compositor already running — show the launcher directly.
                startActivity(new Intent(this, LauncherActivity.class)
                        .addFlags(Intent.FLAG_ACTIVITY_REORDER_TO_FRONT));
            } else {
                needsSetup = nativeInit(this);
                sCompositorStarted = true;
            }
        }

        // Request full external storage access (Android 11+). Required so Linux apps
        // in proot can read/write files on shared storage (Downloads, etc.).
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R
                && !Environment.isExternalStorageManager()) {
            startActivity(new Intent(
                    Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
                    Uri.parse("package:" + getPackageName())));
        }

        // Request notification and camera permissions, then start the foreground service.
        // Camera permission causes Android to add the app to the 'camera' group, allowing
        // direct /dev/video* access from proot Linux apps (e.g. qv4l2, ffmpeg).
        java.util.List<String> permsToRequest = new java.util.ArrayList<>();
        if (checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                != PackageManager.PERMISSION_GRANTED) {
            permsToRequest.add(Manifest.permission.POST_NOTIFICATIONS);
        }
        if (checkSelfPermission(Manifest.permission.CAMERA)
                != PackageManager.PERMISSION_GRANTED) {
            permsToRequest.add(Manifest.permission.CAMERA);
        }
        if (!permsToRequest.isEmpty()) {
            requestPermissions(permsToRequest.toArray(new String[0]), 1);
        } else {
            startForegroundService(new Intent(this, CompositorService.class));
        }
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        // Start service regardless — it works without notification permission,
        // the notification just won't be visible.
        startForegroundService(new Intent(this, CompositorService.class));
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus) {
            if (needsSetup && !overlayShown) {
                SetupOverlay.show(this);
                overlayShown = true;
            }
            if (sStatusView == null) {
                addStatusOverlay();
            }
        }
    }

    private void addStatusOverlay() {
        TextView statusView = new TextView(this);
        statusView.setTextColor(0xFFCCCCCC);
        statusView.setBackgroundColor(0xCC111111);
        statusView.setTypeface(Typeface.MONOSPACE);
        statusView.setTextSize(TypedValue.COMPLEX_UNIT_SP, 13);
        statusView.setPadding(32, 32, 32, 32);

        WindowManager.LayoutParams params = new WindowManager.LayoutParams(
                WindowManager.LayoutParams.WRAP_CONTENT,
                WindowManager.LayoutParams.WRAP_CONTENT,
                WindowManager.LayoutParams.TYPE_APPLICATION_PANEL,
                WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE
                        | WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE,
                PixelFormat.TRANSLUCENT);
        params.gravity = Gravity.TOP | Gravity.START;
        params.token = getWindow().getDecorView().getWindowToken();

        getWindowManager().addView(statusView, params);
        sStatusView = statusView;
    }

    @Override
    public void finish() {
        // Don't destroy the Activity — move to background instead.
        // The compositor runs independently on its own thread.
        moveTaskToBack(true);
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        TextView view = sStatusView;
        if (view != null) {
            try {
                getWindowManager().removeView(view);
            } catch (Exception e) {
                // View may already be detached if the window was destroyed.
                android.util.Log.w("MainActivity", "removeView failed", e);
            }
            sStatusView = null;
        }
    }

    /** Called from native code via JNI to update the status text. */
    public static void updateStatus(String text) {
        sLastStatus = text;
        TextView view = sStatusView;
        if (view != null) {
            view.post(() -> view.setText(text));
        }
    }

    /** Returns the last status text received from the compositor. */
    public static String getLastStatus() {
        return sLastStatus;
    }
}
