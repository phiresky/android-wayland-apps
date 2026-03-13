package io.github.phiresky.wayland_android;

import android.app.Activity;
import android.graphics.Typeface;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.util.TypedValue;
import android.widget.ScrollView;
import android.widget.TextView;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

/**
 * Shows setup/install log output during first-run proot setup.
 * Native code sends log lines via the static appendLog() method.
 * Lines are buffered in a thread-safe list and flushed to the UI by a polling handler.
 */
public class SetupActivity extends Activity {
    private static volatile SetupActivity sInstance;
    private static final List<String> sLines =
            Collections.synchronizedList(new ArrayList<>());

    private TextView logView;
    private ScrollView scrollView;
    private final Handler handler = new Handler(Looper.getMainLooper());
    private int lastFlushed = 0;

    private final Runnable flushRunnable = () -> flushLines();

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        scrollView = new ScrollView(this);
        logView = new TextView(this);
        logView.setTextColor(0xFFCCCCCC);
        logView.setBackgroundColor(0xFF111111);
        logView.setTypeface(Typeface.MONOSPACE);
        logView.setTextSize(TypedValue.COMPLEX_UNIT_SP, 11);
        logView.setPadding(32, 48, 32, 32);
        scrollView.addView(logView);
        setContentView(scrollView);

        sInstance = this;

        logView.setText("=== SetupActivity is visible ===\n");

        // Start polling for new lines
        handler.post(flushRunnable);
    }

    private void flushLines() {
        synchronized (sLines) {
            for (int i = lastFlushed; i < sLines.size(); i++) {
                logView.append(sLines.get(i) + "\n");
            }
            lastFlushed = sLines.size();
        }
        scrollView.post(() -> scrollView.fullScroll(ScrollView.FOCUS_DOWN));
        // Keep polling
        handler.postDelayed(flushRunnable, 100);
    }

    /** Called from native code via JNI to append a log line. Thread-safe. */
    public static void appendLog(String line) {
        sLines.add(line);
    }

    /** Called from native code via JNI when setup is complete. */
    public static void finishSetup() {
        SetupActivity instance = sInstance;
        if (instance != null) {
            instance.handler.removeCallbacks(instance.flushRunnable);
            instance.runOnUiThread(() -> {
                instance.flushLines();
                instance.finish();
            });
        }
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        handler.removeCallbacks(flushRunnable);
        if (sInstance == this) {
            sInstance = null;
        }
    }
}
