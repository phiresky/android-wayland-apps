package io.github.phiresky.wayland_android;

import android.app.Activity;
import android.graphics.Typeface;
import android.os.Bundle;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.View;
import android.widget.GridLayout;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

import java.io.BufferedReader;
import java.io.File;
import java.io.FileReader;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

/**
 * Native Android launcher that reads .desktop files from the Arch rootfs
 * and displays them in a touch-friendly grid. Tapping an app launches it
 * via proot through JNI.
 */
public class LauncherActivity extends Activity {

    static {
        System.loadLibrary("android_wayland_launcher");
    }

    private static native void nativeLaunchApp(String command);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        getWindow().setDecorFitsSystemWindows(true);

        List<DesktopEntry> apps = scanDesktopFiles();

        ScrollView scroll = new ScrollView(this);
        scroll.setBackgroundColor(0xFF1A1A2E);

        LinearLayout container = new LinearLayout(this);
        container.setOrientation(LinearLayout.VERTICAL);
        container.setPadding(dp(16), dp(24), dp(16), dp(24));

        // Title
        TextView title = new TextView(this);
        title.setText("Applications");
        title.setTextColor(0xFFE0E0E0);
        title.setTextSize(TypedValue.COMPLEX_UNIT_SP, 28);
        title.setTypeface(Typeface.DEFAULT_BOLD);
        title.setPadding(dp(8), 0, dp(8), dp(16));
        container.addView(title);

        if (apps.isEmpty()) {
            TextView empty = new TextView(this);
            empty.setText("No applications found.\nCheck that the Arch rootfs setup completed.");
            empty.setTextColor(0xFF888888);
            empty.setTextSize(TypedValue.COMPLEX_UNIT_SP, 16);
            empty.setPadding(dp(8), dp(32), dp(8), dp(32));
            container.addView(empty);
        } else {
            int columns = 3;
            GridLayout grid = new GridLayout(this);
            grid.setColumnCount(columns);

            for (int i = 0; i < apps.size(); i++) {
                DesktopEntry app = apps.get(i);
                View card = createAppCard(app);

                GridLayout.LayoutParams params = new GridLayout.LayoutParams();
                params.width = 0;
                params.columnSpec = GridLayout.spec(i % columns, 1, 1f);
                params.setMargins(dp(4), dp(4), dp(4), dp(4));
                grid.addView(card, params);
            }

            container.addView(grid);
        }

        scroll.addView(container);
        setContentView(scroll);
    }

    private View createAppCard(DesktopEntry app) {
        LinearLayout card = new LinearLayout(this);
        card.setOrientation(LinearLayout.VERTICAL);
        card.setBackgroundColor(0xFF16213E);
        card.setPadding(dp(16), dp(20), dp(16), dp(20));
        card.setMinimumHeight(dp(88));
        card.setGravity(Gravity.CENTER);

        TextView name = new TextView(this);
        name.setText(app.name);
        name.setTextColor(0xFFE0E0E0);
        name.setTextSize(TypedValue.COMPLEX_UNIT_SP, 16);
        name.setTypeface(Typeface.DEFAULT_BOLD);
        name.setGravity(Gravity.CENTER);
        card.addView(name);

        if (app.comment != null && !app.comment.isEmpty()) {
            TextView desc = new TextView(this);
            desc.setText(app.comment);
            desc.setTextColor(0xFF888888);
            desc.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12);
            desc.setGravity(Gravity.CENTER);
            desc.setMaxLines(2);
            card.addView(desc);
        }

        card.setClickable(true);
        card.setFocusable(true);
        // Ripple touch feedback
        TypedValue outValue = new TypedValue();
        getTheme().resolveAttribute(android.R.attr.selectableItemBackground, outValue, true);
        card.setForeground(getDrawable(outValue.resourceId));

        card.setOnClickListener(v -> nativeLaunchApp(app.exec));

        return card;
    }

    private int dp(int value) {
        return (int) TypedValue.applyDimension(
                TypedValue.COMPLEX_UNIT_DIP, value, getResources().getDisplayMetrics());
    }

    private List<DesktopEntry> scanDesktopFiles() {
        List<DesktopEntry> apps = new ArrayList<>();
        String rootfs = getApplicationInfo().dataDir + "/files/arch";
        File appsDir = new File(rootfs + "/usr/share/applications");

        if (!appsDir.isDirectory()) return apps;

        File[] files = appsDir.listFiles((dir, name) -> name.endsWith(".desktop"));
        if (files == null) return apps;

        for (File file : files) {
            DesktopEntry entry = parseDesktopFile(file);
            if (entry != null) {
                apps.add(entry);
            }
        }

        Collections.sort(apps, (a, b) -> a.name.compareToIgnoreCase(b.name));
        return apps;
    }

    private DesktopEntry parseDesktopFile(File file) {
        String name = null, exec = null, comment = null, type = null;
        boolean noDisplay = false, hidden = false;
        boolean inDesktopEntry = false;

        try (BufferedReader reader = new BufferedReader(new FileReader(file))) {
            String line;
            while ((line = reader.readLine()) != null) {
                line = line.trim();
                if ("[Desktop Entry]".equals(line)) {
                    inDesktopEntry = true;
                    continue;
                }
                if (line.startsWith("[") && inDesktopEntry) {
                    break; // past the main section
                }
                if (!inDesktopEntry) continue;

                if (line.startsWith("Name=") && name == null) {
                    name = line.substring(5);
                } else if (line.startsWith("Exec=")) {
                    exec = line.substring(5)
                            .replaceAll("%[fFuUdDnNickvm]", "")
                            .trim();
                } else if (line.startsWith("Comment=") && comment == null) {
                    comment = line.substring(8);
                } else if (line.startsWith("Type=")) {
                    type = line.substring(5);
                } else if ("NoDisplay=true".equals(line)) {
                    noDisplay = true;
                } else if ("Hidden=true".equals(line)) {
                    hidden = true;
                }
            }
        } catch (Exception e) {
            return null;
        }

        if (name == null || exec == null || !"Application".equals(type)
                || noDisplay || hidden) {
            return null;
        }

        return new DesktopEntry(name, exec, comment);
    }

    private static class DesktopEntry {
        final String name;
        final String exec;
        final String comment;

        DesktopEntry(String name, String exec, String comment) {
            this.name = name;
            this.exec = exec;
            this.comment = comment;
        }
    }
}
