package io.github.phiresky.wayland_android;

import android.app.Activity;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.RectF;
import android.graphics.Typeface;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.drawable.BitmapDrawable;
import android.graphics.drawable.Drawable;
import android.os.Bundle;
import android.text.TextUtils;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.View;
import android.widget.GridLayout;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

import androidx.swiperefreshlayout.widget.SwipeRefreshLayout;

import java.io.File;
import java.nio.file.Files;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

/**
 * Native Android launcher that reads .desktop files from the Arch rootfs
 * and displays them in a touch-friendly grid. Tapping an app launches it
 * via proot through JNI.
 */
public class LauncherActivity extends Activity {

    private String rootfs;
    private String[] ignoreList = {};
    private DesktopEntry[] extraApps = {};
    private LinearLayout container;
    private SwipeRefreshLayout swipeRefresh;

    private static final int[] ICON_COLORS = {
            0xFF4285F4, 0xFFEA4335, 0xFFFBBC05, 0xFF34A853,
            0xFF9C27B0, 0xFFFF5722, 0xFF00BCD4, 0xFF607D8B,
            0xFFE91E63, 0xFF3F51B5, 0xFF009688, 0xFF795548,
    };

    static {
        System.loadLibrary("android_wayland_launcher");
    }

    private static native void nativeLaunchApp(String command);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        getWindow().setDecorFitsSystemWindows(true);
        rootfs = getApplicationInfo().dataDir + "/files/arch";

        // Read launcher config from intent extras (set by native code in launch.rs)
        String[] ignore = getIntent().getStringArrayExtra("ignore");
        if (ignore != null) ignoreList = ignore;
        String[] extraNames = getIntent().getStringArrayExtra("extra_names");
        String[] extraExecs = getIntent().getStringArrayExtra("extra_execs");
        String[] extraIcons = getIntent().getStringArrayExtra("extra_icons");
        if (extraNames != null && extraExecs != null) {
            int len = Math.min(extraNames.length, extraExecs.length);
            extraApps = new DesktopEntry[len];
            for (int i = 0; i < len; i++) {
                String icon = (extraIcons != null && i < extraIcons.length) ? extraIcons[i] : null;
                extraApps[i] = new DesktopEntry(extraNames[i], extraExecs[i], icon);
            }
        }

        swipeRefresh = new SwipeRefreshLayout(this);
        swipeRefresh.setBackgroundColor(0xFF1A1A2E);
        swipeRefresh.setColorSchemeColors(0xFF4285F4, 0xFFEA4335, 0xFF34A853);
        swipeRefresh.setProgressBackgroundColorSchemeColor(0xFF2A2A3E);

        ScrollView scroll = new ScrollView(this);

        container = new LinearLayout(this);
        container.setOrientation(LinearLayout.VERTICAL);
        container.setPadding(dp(12), dp(24), dp(12), dp(24));

        scroll.addView(container);
        swipeRefresh.addView(scroll);
        swipeRefresh.setOnRefreshListener(this::refreshApps);
        setContentView(swipeRefresh);

        refreshApps();
    }

    private void refreshApps() {
        container.removeAllViews();
        List<DesktopEntry> apps = scanDesktopFiles();

        if (apps.isEmpty()) {
            TextView empty = new TextView(this);
            empty.setText("No applications found.\nCheck that the Arch rootfs setup completed.");
            empty.setTextColor(0xFF888888);
            empty.setTextSize(TypedValue.COMPLEX_UNIT_SP, 16);
            empty.setPadding(dp(16), dp(48), dp(16), dp(48));
            empty.setGravity(Gravity.CENTER);
            container.addView(empty);
        } else {
            int columns = 4;
            GridLayout grid = new GridLayout(this);
            grid.setColumnCount(columns);

            for (int i = 0; i < apps.size(); i++) {
                DesktopEntry app = apps.get(i);
                View cell = createAppCell(app, i);

                GridLayout.LayoutParams params = new GridLayout.LayoutParams();
                params.width = 0;
                params.columnSpec = GridLayout.spec(i % columns, 1, 1f);
                params.setMargins(dp(2), dp(2), dp(2), dp(2));
                grid.addView(cell, params);
            }

            container.addView(grid);
        }

        if (swipeRefresh.isRefreshing()) {
            swipeRefresh.setRefreshing(false);
        }
    }

    private View createAppCell(DesktopEntry app, int index) {
        LinearLayout cell = new LinearLayout(this);
        cell.setOrientation(LinearLayout.VERTICAL);
        cell.setPadding(dp(8), dp(12), dp(8), dp(8));
        cell.setGravity(Gravity.CENTER_HORIZONTAL);

        // App icon — try real icon from rootfs, fall back to letter placeholder
        int iconSize = dp(52);
        ImageView icon = new ImageView(this);
        icon.setLayoutParams(new LinearLayout.LayoutParams(iconSize, iconSize));
        Drawable iconDrawable = loadIcon(app.icon, iconSize);
        if (iconDrawable == null) {
            int color = ICON_COLORS[Math.abs(app.name.hashCode()) % ICON_COLORS.length];
            String letter = app.name.substring(0, 1).toUpperCase();
            iconDrawable = new LetterIconDrawable(letter, color, iconSize);
        }
        icon.setImageDrawable(iconDrawable);
        cell.addView(icon);

        // App name below
        TextView name = new TextView(this);
        name.setText(app.name);
        name.setTextColor(0xFFE0E0E0);
        name.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12);
        name.setGravity(Gravity.CENTER);
        name.setMaxLines(2);
        name.setEllipsize(TextUtils.TruncateAt.END);
        LinearLayout.LayoutParams nameParams = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT,
                LinearLayout.LayoutParams.WRAP_CONTENT);
        nameParams.topMargin = dp(6);
        name.setLayoutParams(nameParams);
        cell.addView(name);

        cell.setClickable(true);
        cell.setFocusable(true);
        TypedValue outValue = new TypedValue();
        getTheme().resolveAttribute(android.R.attr.selectableItemBackground, outValue, true);
        cell.setForeground(getDrawable(outValue.resourceId));

        cell.setOnClickListener(v -> nativeLaunchApp(app.exec));

        return cell;
    }

    private int dp(int value) {
        return (int) TypedValue.applyDimension(
                TypedValue.COMPLEX_UNIT_DIP, value, getResources().getDisplayMetrics());
    }

    private List<DesktopEntry> scanDesktopFiles() {
        List<DesktopEntry> apps = new ArrayList<>();
        File appsDir = new File(rootfs + "/usr/share/applications");

        if (!appsDir.isDirectory()) return apps;

        File[] files = appsDir.listFiles((dir, name) -> name.endsWith(".desktop"));
        if (files == null) return apps;

        for (File file : files) {
            // Check ignore list against filename (without .desktop extension)
            String baseName = file.getName().replace(".desktop", "");
            boolean ignored = false;
            for (String ignore : ignoreList) {
                if (baseName.equals(ignore)) {
                    ignored = true;
                    break;
                }
            }
            if (ignored) continue;

            DesktopEntry entry = parseDesktopFile(file);
            if (entry != null) {
                apps.add(entry);
            }
        }

        // Add extra hardcoded entries
        Collections.addAll(apps, extraApps);

        Collections.sort(apps, (a, b) -> a.name.compareToIgnoreCase(b.name));
        return apps;
    }

    private static final String[] ICON_EXTENSIONS = {".png", ".svg", ".xpm"};
    private static final int[] ICON_SIZES = {256, 128, 64, 48, 32, 24, 22};

    private Drawable loadIcon(String iconName, int targetSize) {
        if (iconName == null || iconName.isEmpty()) return null;

        // If it's a bundled APK drawable (@drawable/name), load from resources
        if (iconName.startsWith("@drawable/")) {
            String resName = iconName.substring("@drawable/".length());
            int resId = getResources().getIdentifier(resName, "drawable", getPackageName());
            if (resId != 0) {
                Drawable d = getDrawable(resId);
                if (d != null) return d;
            }
            return null;
        }

        // If it's an absolute path, try it directly
        if (iconName.startsWith("/")) {
            return decodeIcon(new File(rootfs + iconName), targetSize);
        }

        // Search hicolor theme in descending size order
        String iconsBase = rootfs + "/usr/share/icons/hicolor";
        for (int size : ICON_SIZES) {
            String sizeDir = size + "x" + size;
            for (String ext : ICON_EXTENSIONS) {
                File f = new File(iconsBase + "/" + sizeDir + "/apps/" + iconName + ext);
                Drawable d = decodeIcon(f, targetSize);
                if (d != null) return d;
            }
        }

        // Try scalable
        for (String ext : ICON_EXTENSIONS) {
            File f = new File(iconsBase + "/scalable/apps/" + iconName + ext);
            Drawable d = decodeIcon(f, targetSize);
            if (d != null) return d;
        }

        // Try pixmaps
        for (String ext : ICON_EXTENSIONS) {
            File f = new File(rootfs + "/usr/share/pixmaps/" + iconName + ext);
            Drawable d = decodeIcon(f, targetSize);
            if (d != null) return d;
        }

        return null;
    }

    private Drawable decodeIcon(File file, int targetSize) {
        if (!file.exists()) return null;
        try {
            Bitmap bmp = BitmapFactory.decodeFile(file.getAbsolutePath());
            if (bmp == null) return null;
            if (bmp.getWidth() != targetSize || bmp.getHeight() != targetSize) {
                bmp = Bitmap.createScaledBitmap(bmp, targetSize, targetSize, true);
            }
            return new BitmapDrawable(getResources(), bmp);
        } catch (Exception e) {
            return null;
        }
    }

    private DesktopEntry parseDesktopFile(File file) {
        String content;
        try {
            content = new String(Files.readAllBytes(file.toPath()));
        } catch (Exception e) {
            return null;
        }

        DesktopFileParser.DesktopFile df = DesktopFileParser.parse(content);

        String type = df.getString("Type");
        if (!"Application".equals(type)) return null;
        if (df.getBoolean("NoDisplay") || df.getBoolean("Hidden") || df.getBoolean("Terminal"))
            return null;

        String name = df.getString("Name");
        String exec = df.getString("Exec");
        if (name == null || exec == null) return null;

        // Strip field codes (%f, %F, %u, %U, etc.)
        exec = exec.replaceAll("%[fFuUdDnNickvm]", "").trim();

        String icon = df.getString("Icon");
        return new DesktopEntry(name, exec, icon);
    }

    private static class DesktopEntry {
        final String name;
        final String exec;
        final String icon;

        DesktopEntry(String name, String exec, String icon) {
            this.name = name;
            this.exec = exec;
            this.icon = icon;
        }
    }

    /** Draws a rounded square with a centered letter as an app icon placeholder. */
    private static class LetterIconDrawable extends Drawable {
        private final String letter;
        private final Paint bgPaint = new Paint(Paint.ANTI_ALIAS_FLAG);
        private final Paint textPaint = new Paint(Paint.ANTI_ALIAS_FLAG);
        private final int size;

        LetterIconDrawable(String letter, int color, int size) {
            this.letter = letter;
            this.size = size;
            bgPaint.setColor(color);
            textPaint.setColor(Color.WHITE);
            textPaint.setTypeface(Typeface.DEFAULT_BOLD);
            textPaint.setTextSize(size * 0.45f);
            textPaint.setTextAlign(Paint.Align.CENTER);
        }

        @Override
        public void draw(Canvas canvas) {
            float radius = size * 0.22f;
            canvas.drawRoundRect(new RectF(0, 0, size, size), radius, radius, bgPaint);
            float y = size / 2f - (textPaint.descent() + textPaint.ascent()) / 2f;
            canvas.drawText(letter, size / 2f, y, textPaint);
        }

        @Override public void setAlpha(int alpha) { bgPaint.setAlpha(alpha); }
        @Override public void setColorFilter(android.graphics.ColorFilter cf) { bgPaint.setColorFilter(cf); }
        @Override public int getOpacity() { return android.graphics.PixelFormat.TRANSLUCENT; }
        @Override public int getIntrinsicWidth() { return size; }
        @Override public int getIntrinsicHeight() { return size; }
    }
}
