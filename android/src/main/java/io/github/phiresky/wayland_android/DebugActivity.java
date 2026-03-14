package io.github.phiresky.wayland_android;

import android.app.Activity;
import android.graphics.Typeface;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.util.TypedValue;
import android.view.Gravity;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

/**
 * Full-screen activity showing live compositor debug info (clients, toplevels, FPS).
 * Auto-refreshes every second while visible.
 */
public class DebugActivity extends Activity {

    private TextView content;
    private final Handler handler = new Handler(Looper.getMainLooper());
    private final Runnable refresh = new Runnable() {
        @Override
        public void run() {
            String status = MainActivity.getLastStatus();
            content.setText(status.isEmpty() ? "No compositor data yet" : status);
            handler.postDelayed(this, 1000);
        }
    };

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        getWindow().setDecorFitsSystemWindows(true);
        getWindow().setStatusBarColor(0xFF1A1A2E);
        getWindow().setNavigationBarColor(0xFF1A1A2E);

        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setBackgroundColor(0xFF1A1A2E);

        // Title bar
        TextView title = new TextView(this);
        title.setText("Compositor Status");
        title.setTextColor(0xFFE0E0E0);
        title.setTextSize(TypedValue.COMPLEX_UNIT_SP, 20);
        title.setTypeface(Typeface.DEFAULT_BOLD);
        title.setPadding(dp(16), dp(16), dp(16), dp(12));
        root.addView(title);

        // Scrollable status content
        ScrollView scroll = new ScrollView(this);
        scroll.setLayoutParams(new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT,
                LinearLayout.LayoutParams.MATCH_PARENT));

        content = new TextView(this);
        content.setTypeface(Typeface.MONOSPACE);
        content.setTextColor(0xFFCCCCCC);
        content.setTextSize(TypedValue.COMPLEX_UNIT_SP, 13);
        content.setPadding(dp(16), dp(8), dp(16), dp(16));
        content.setTextIsSelectable(true);

        String status = MainActivity.getLastStatus();
        content.setText(status.isEmpty() ? "No compositor data yet" : status);

        scroll.addView(content);
        root.addView(scroll);
        setContentView(root);
    }

    @Override
    protected void onResume() {
        super.onResume();
        handler.post(refresh);
    }

    @Override
    protected void onPause() {
        super.onPause();
        handler.removeCallbacks(refresh);
    }

    private int dp(int value) {
        return (int) TypedValue.applyDimension(
                TypedValue.COMPLEX_UNIT_DIP, value, getResources().getDisplayMetrics());
    }
}
