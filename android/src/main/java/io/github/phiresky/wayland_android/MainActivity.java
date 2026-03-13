package io.github.phiresky.wayland_android;

import android.Manifest;
import android.app.NativeActivity;
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
 * Subclass of NativeActivity that forces a first frame draw to dismiss
 * the Android 12+ splash screen before native setup blocks android_main.
 *
 * Also hosts a status overlay showing connected Wayland client info.
 */
public class MainActivity extends NativeActivity {
    private static volatile TextView sStatusView;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        // Force the window to draw a frame, which dismisses the splash screen.
        View placeholder = new View(this);
        placeholder.setBackgroundColor(0xFF111111);
        addContentView(placeholder,
                new ViewGroup.LayoutParams(
                        ViewGroup.LayoutParams.MATCH_PARENT,
                        ViewGroup.LayoutParams.MATCH_PARENT));

        reportFullyDrawn();

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
        if (hasFocus && sStatusView == null) {
            addStatusOverlay();
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
        // Don't destroy the NativeActivity — move to background instead.
        // Destroying it kills winit's event loop, which stops all Wayland
        // protocol processing and rendering, causing ANR.
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
