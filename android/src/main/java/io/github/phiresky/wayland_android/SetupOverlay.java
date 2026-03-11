package io.github.phiresky.wayland_android;

import android.app.Activity;
import android.graphics.PixelFormat;
import android.graphics.Typeface;
import android.os.Handler;
import android.os.Looper;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.WindowManager;
import android.widget.ScrollView;
import android.widget.TextView;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

/**
 * Adds a full-screen log overlay on top of the NativeActivity's EGL surface
 * during first-run proot setup. Uses WindowManager to place the view above
 * the native rendering surface.
 */
public class SetupOverlay {
    private static final List<String> sLines =
            Collections.synchronizedList(new ArrayList<>());
    private static volatile TextView sLogView;
    private static volatile ScrollView sScrollView;
    private static Handler sHandler;
    private static int sLastFlushed = 0;

    /** Called from native code via JNI. Adds the overlay on the UI thread. */
    public static void show(Activity activity) {
        sLines.clear();
        sLastFlushed = 0;
        activity.runOnUiThread(() -> {
            ScrollView scrollView = new ScrollView(activity);
            TextView logView = new TextView(activity);
            logView.setTextColor(0xFFCCCCCC);
            logView.setBackgroundColor(0xFF111111);
            logView.setTypeface(Typeface.MONOSPACE);
            logView.setTextSize(TypedValue.COMPLEX_UNIT_SP, 11);
            logView.setPadding(32, 48, 32, 32);
            logView.setText("=== Setup starting ===\n");
            scrollView.addView(logView);

            WindowManager.LayoutParams params = new WindowManager.LayoutParams(
                    WindowManager.LayoutParams.MATCH_PARENT,
                    WindowManager.LayoutParams.MATCH_PARENT,
                    WindowManager.LayoutParams.TYPE_APPLICATION_PANEL,
                    WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE
                            | WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN,
                    PixelFormat.OPAQUE);
            params.gravity = Gravity.FILL;
            params.token = activity.getWindow().getDecorView().getWindowToken();

            activity.getWindowManager().addView(scrollView, params);

            sLogView = logView;
            sScrollView = scrollView;
            sHandler = new Handler(Looper.getMainLooper());
            sHandler.post(SetupOverlay::flushLines);
        });
    }

    private static void flushLines() {
        TextView logView = sLogView;
        ScrollView scrollView = sScrollView;
        if (logView == null || scrollView == null) return;

        synchronized (sLines) {
            for (int i = sLastFlushed; i < sLines.size(); i++) {
                logView.append(sLines.get(i) + "\n");
            }
            sLastFlushed = sLines.size();
        }
        scrollView.post(() -> scrollView.fullScroll(ScrollView.FOCUS_DOWN));
        if (sHandler != null) {
            sHandler.postDelayed(SetupOverlay::flushLines, 100);
        }
    }

    /** Called from native code via JNI to append a log line. Thread-safe. */
    public static void appendLog(String line) {
        sLines.add(line);
    }

    /** Called from native code via JNI when setup is complete. Removes the overlay. */
    public static void hide(Activity activity) {
        if (sHandler != null) {
            sHandler.removeCallbacksAndMessages(null);
        }
        activity.runOnUiThread(() -> {
            // Flush remaining lines
            flushLines();
            if (sHandler != null) {
                sHandler.removeCallbacksAndMessages(null);
            }
            if (sScrollView != null) {
                try {
                    activity.getWindowManager().removeView(sScrollView);
                } catch (Exception e) {
                    // View might not be attached
                }
            }
            sLogView = null;
            sScrollView = null;
            sHandler = null;
        });
    }
}
