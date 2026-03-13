package io.github.phiresky.wayland_android;

import android.Manifest;
import android.app.Activity;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.PixelFormat;
import android.graphics.Typeface;
import android.os.Bundle;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowManager;
import android.widget.TextView;

/**
 * Main entry point for the Wayland compositor.
 * Loads the native library and calls nativeInit() to start the compositor
 * on a background thread. Also hosts setup and status overlays.
 */
public class MainActivity extends Activity {
    private static volatile TextView sStatusView;
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
            needsSetup = nativeInit(this);
        }

        // Request notification permission (required on Android 13+), then start
        // the foreground service to prevent DeX from killing the compositor.
        if (checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[]{Manifest.permission.POST_NOTIFICATIONS}, 1);
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
            } catch (Exception ignored) {}
            sStatusView = null;
        }
    }

    /** Called from native code via JNI to update the status text. */
    public static void updateStatus(String text) {
        TextView view = sStatusView;
        if (view != null) {
            view.post(() -> view.setText(text));
        }
    }
}
