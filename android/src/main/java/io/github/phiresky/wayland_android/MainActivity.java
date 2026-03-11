package io.github.phiresky.wayland_android;

import android.app.NativeActivity;
import android.graphics.PixelFormat;
import android.graphics.Typeface;
import android.os.Bundle;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
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
