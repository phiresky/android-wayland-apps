package io.github.phiresky.wayland_android

import android.app.Activity
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.ColorFilter
import android.graphics.Paint
import android.graphics.PixelFormat
import android.graphics.RectF
import android.graphics.Typeface
import android.graphics.drawable.BitmapDrawable
import android.graphics.drawable.Drawable
import android.os.Bundle
import android.text.TextUtils
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.view.ViewTreeObserver
import android.widget.GridLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import androidx.swiperefreshlayout.widget.SwipeRefreshLayout
import java.io.File
import java.nio.file.Files
import kotlin.math.abs
import kotlin.math.max
import kotlin.math.min

/**
 * Native Android launcher that reads .desktop files from the Arch rootfs
 * and displays them in a touch-friendly grid. Tapping an app launches it
 * via proot through JNI.
 */
class LauncherActivity : Activity() {

    private lateinit var rootfs: String
    private var ignoreList = emptyArray<String>()
    private var extraApps = emptyArray<DesktopEntry>()
    private lateinit var container: LinearLayout
    private lateinit var swipeRefresh: SwipeRefreshLayout

    private var cachedApps = mutableListOf<DesktopEntry>()
    private var lastGridWidth = 0

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        rootfs = applicationInfo.dataDir + "/files/arch"

        // Read launcher config from intent extras (set by native code in launch.rs).
        // Save to companion object so config survives Activity recreation.
        intent.getStringArrayExtra("ignore")?.let {
            ignoreList = it
            savedIgnoreList = it
        } ?: savedIgnoreList?.let { ignoreList = it }

        val extraNames = intent.getStringArrayExtra("extra_names")
        val extraExecs = intent.getStringArrayExtra("extra_execs")
        val extraIcons = intent.getStringArrayExtra("extra_icons")
        if (extraNames != null && extraExecs != null) {
            val len = min(extraNames.size, extraExecs.size)
            extraApps = Array(len) { i ->
                val icon = if (extraIcons != null && i < extraIcons.size) extraIcons[i] else null
                DesktopEntry(extraNames[i], extraExecs[i], icon)
            }
            savedExtraApps = extraApps
        } else {
            savedExtraApps?.let { extraApps = it }
        }

        swipeRefresh = SwipeRefreshLayout(this).apply {
            setBackgroundColor(0xFF1A1A2E.toInt())
            setColorSchemeColors(0xFF4285F4.toInt(), 0xFFEA4335.toInt(), 0xFF34A853.toInt())
            setProgressBackgroundColorSchemeColor(0xFF2A2A3E.toInt())
        }

        val scroll = ScrollView(this)

        container = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(12), dp(24), dp(12), dp(24))
        }

        scroll.addView(container)
        swipeRefresh.addView(scroll)
        swipeRefresh.setOnRefreshListener(::refreshApps)
        setContentView(swipeRefresh)

        // SDK 35 ignores setDecorFitsSystemWindows(true) and enforces edge-to-edge.
        // Apply system bar insets as padding so content doesn't overlap status/nav bars.
        swipeRefresh.setOnApplyWindowInsetsListener { v, insets ->
            val bars = insets.getInsets(android.view.WindowInsets.Type.systemBars())
            v.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            insets
        }

        swipeRefresh.addOnLayoutChangeListener { _, left, _, right, _, _, _, _, _ ->
            val newWidth = right - left
            if (newWidth > 0 && newWidth != lastGridWidth && cachedApps.isNotEmpty()) {
                rebuildGrid()
            }
        }

        refreshApps()
    }

    private fun refreshApps() {
        cachedApps = scanDesktopFiles().toMutableList()
        rebuildGrid()
        if (swipeRefresh.isRefreshing) {
            swipeRefresh.isRefreshing = false
        }
    }

    private fun rebuildGrid() {
        container.removeAllViews()

        if (cachedApps.isEmpty()) {
            val empty = TextView(this).apply {
                text = "No applications found.\nCheck that the Arch rootfs setup completed."
                setTextColor(0xFF888888.toInt())
                setTextSize(TypedValue.COMPLEX_UNIT_SP, 16f)
                setPadding(dp(16), dp(48), dp(16), dp(48))
                gravity = Gravity.CENTER
            }
            container.addView(empty)
            return
        }

        val containerWidth = container.width
        if (containerWidth == 0) {
            container.viewTreeObserver.addOnGlobalLayoutListener(
                object : ViewTreeObserver.OnGlobalLayoutListener {
                    override fun onGlobalLayout() {
                        container.viewTreeObserver.removeOnGlobalLayoutListener(this)
                        rebuildGrid()
                    }
                }
            )
            return
        }

        val columns = max(2, containerWidth / dp(88))
        lastGridWidth = containerWidth
        val grid = GridLayout(this).apply { columnCount = columns }

        for ((i, app) in cachedApps.withIndex()) {
            val cell = createAppCell(app)
            val params = GridLayout.LayoutParams().apply {
                width = 0
                columnSpec = GridLayout.spec(i % columns, 1, 1f)
                setMargins(dp(2), dp(2), dp(2), dp(2))
            }
            grid.addView(cell, params)
        }

        container.addView(grid)
    }

    private fun createAppCell(app: DesktopEntry): View {
        val cell = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(8), dp(12), dp(8), dp(8))
            gravity = Gravity.CENTER_HORIZONTAL
        }

        // App icon — try real icon from rootfs, fall back to letter placeholder
        val iconSize = dp(52)
        val icon = ImageView(this).apply {
            layoutParams = LinearLayout.LayoutParams(iconSize, iconSize)
        }
        val iconDrawable = loadIcon(app.icon, iconSize)
            ?: run {
                val color = ICON_COLORS[abs(app.name.hashCode()) % ICON_COLORS.size]
                val letter = app.name.substring(0, 1).uppercase()
                LetterIconDrawable(letter, color, iconSize)
            }
        icon.setImageDrawable(iconDrawable)
        cell.addView(icon)

        // App name below
        val name = TextView(this).apply {
            text = app.name
            setTextColor(0xFFE0E0E0.toInt())
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
            gravity = Gravity.CENTER
            maxLines = 2
            ellipsize = TextUtils.TruncateAt.END
            layoutParams = LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT,
                LinearLayout.LayoutParams.WRAP_CONTENT
            ).apply { topMargin = dp(6) }
        }
        cell.addView(name)

        cell.isClickable = true
        cell.isFocusable = true
        val outValue = TypedValue()
        theme.resolveAttribute(android.R.attr.selectableItemBackground, outValue, true)
        cell.foreground = getDrawable(outValue.resourceId)

        cell.setOnClickListener {
            if (app.exec.startsWith("__builtin:")) {
                handleBuiltinAction(app.exec)
            } else {
                nativeLaunchApp(app.exec)
            }
        }

        return cell
    }

    private fun handleBuiltinAction(action: String) {
        if (action == "__builtin:debug") {
            startActivity(Intent(this, DebugActivity::class.java).apply {
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_NEW_DOCUMENT)
            })
        }
    }

    override fun finish() {
        moveTaskToBack(true)
    }

    private fun dp(value: Int): Int =
        TypedValue.applyDimension(
            TypedValue.COMPLEX_UNIT_DIP, value.toFloat(), resources.displayMetrics
        ).toInt()

    private fun scanDesktopFiles(): List<DesktopEntry> {
        val apps = mutableListOf<DesktopEntry>()
        val seen = mutableSetOf<String>()

        val appsDirs = listOf(
            "$rootfs/usr/share/applications",
            "$rootfs/var/lib/flatpak/exports/share/applications",
            "$rootfs/home/alarm/.local/share/flatpak/exports/share/applications",
            "$rootfs/home/alarm/.local/share/applications",
        )

        for (dirPath in appsDirs) {
            val dir = File(dirPath)
            if (!dir.isDirectory) continue
            val files = dir.listFiles { _, name -> name.endsWith(".desktop") } ?: continue

            for (file in files) {
                val baseName = file.name.replace(".desktop", "")
                if (baseName in ignoreList) continue
                if (baseName in seen) continue
                seen.add(baseName)

                val entry = parseDesktopFile(file)
                if (entry != null) apps.add(entry)
            }
        }

        // Add extra hardcoded entries
        apps.addAll(extraApps)

        apps.sortWith(compareBy(String.CASE_INSENSITIVE_ORDER) { it.name })
        // Built-in debug info entry (at end of list)
        apps.add(DesktopEntry("Debug Info", "__builtin:debug", null))
        return apps
    }

    private fun loadIcon(iconName: String?, targetSize: Int): Drawable? {
        if (iconName.isNullOrEmpty()) return null

        // If it's a bundled APK drawable (@drawable/name), load from resources
        if (iconName.startsWith("@drawable/")) {
            val resName = iconName.substring("@drawable/".length)
            val resId = resources.getIdentifier(resName, "drawable", packageName)
            if (resId != 0) return getDrawable(resId)
            return null
        }

        // If it's an absolute path, try it directly
        if (iconName.startsWith("/")) {
            return decodeIcon(File(rootfs + iconName), targetSize)
        }

        val iconsBase = "$rootfs/usr/share/icons"
        for (theme in ICON_THEMES) {
            // Search sized directories in descending order
            for (size in ICON_SIZES) {
                val sizeDir = "${size}x${size}"
                for (subdir in ICON_SUBDIRS) {
                    for (ext in ICON_EXTENSIONS) {
                        val f = File("$iconsBase/$theme/$sizeDir/$subdir/$iconName$ext")
                        val d = decodeIcon(f, targetSize)
                        if (d != null) return d
                    }
                }
            }
            // Try scalable
            for (subdir in ICON_SUBDIRS) {
                for (ext in ICON_EXTENSIONS) {
                    val f = File("$iconsBase/$theme/scalable/$subdir/$iconName$ext")
                    val d = decodeIcon(f, targetSize)
                    if (d != null) return d
                }
            }
        }

        // Try pixmaps
        for (ext in ICON_EXTENSIONS) {
            val f = File("$rootfs/usr/share/pixmaps/$iconName$ext")
            val d = decodeIcon(f, targetSize)
            if (d != null) return d
        }

        return null
    }

    private fun decodeIcon(file: File, targetSize: Int): Drawable? {
        if (!file.exists()) return null
        if (file.name.endsWith(".svg")) return decodeSvgIcon(file, targetSize)
        return try {
            var bmp = BitmapFactory.decodeFile(file.absolutePath) ?: return null
            if (bmp.width != targetSize || bmp.height != targetSize) {
                bmp = Bitmap.createScaledBitmap(bmp, targetSize, targetSize, true)
            }
            BitmapDrawable(resources, bmp)
        } catch (_: Exception) {
            null
        }
    }

    private fun decodeSvgIcon(file: File, targetSize: Int): Drawable? = try {
        file.inputStream().use { fis ->
            val svg = com.caverock.androidsvg.SVG.getFromInputStream(fis)
            val bmp = Bitmap.createBitmap(targetSize, targetSize, Bitmap.Config.ARGB_8888)
            val canvas = Canvas(bmp)
            svg.documentWidth = targetSize.toFloat()
            svg.documentHeight = targetSize.toFloat()
            svg.renderToCanvas(canvas)
            BitmapDrawable(resources, bmp)
        }
    } catch (_: Exception) {
        null
    }

    private fun parseDesktopFile(file: File): DesktopEntry? {
        val content = try {
            String(Files.readAllBytes(file.toPath()))
        } catch (_: Exception) {
            return null
        }

        val df = DesktopFileParser.parse(content)

        if (df.getString("Type") != "Application") return null
        if (df.getBoolean("NoDisplay") || df.getBoolean("Hidden") || df.getBoolean("Terminal"))
            return null

        val name = df.getString("Name") ?: return null
        var exec = df.getString("Exec") ?: return null

        // Strip field codes (%f, %F, %u, %U, etc.)
        exec = exec.replace(Regex("%[fFuUdDnNickvm]"), "").trim()

        val icon = df.getString("Icon")
        return DesktopEntry(name, exec, icon)
    }

    private data class DesktopEntry(val name: String, val exec: String, val icon: String?)

    /** Draws a rounded square with a centered letter as an app icon placeholder. */
    private class LetterIconDrawable(
        private val letter: String,
        color: Int,
        private val size: Int,
    ) : Drawable() {
        private val bgPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply { this.color = color }
        private val textPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            this.color = Color.WHITE
            typeface = Typeface.DEFAULT_BOLD
            textSize = size * 0.45f
            textAlign = Paint.Align.CENTER
        }

        override fun draw(canvas: Canvas) {
            val radius = size * 0.22f
            canvas.drawRoundRect(
                RectF(0f, 0f, size.toFloat(), size.toFloat()), radius, radius, bgPaint
            )
            val y = size / 2f - (textPaint.descent() + textPaint.ascent()) / 2f
            canvas.drawText(letter, size / 2f, y, textPaint)
        }

        override fun setAlpha(alpha: Int) { bgPaint.alpha = alpha }
        override fun setColorFilter(cf: ColorFilter?) { bgPaint.colorFilter = cf }

        @Deprecated("Deprecated in Java")
        override fun getOpacity(): Int = PixelFormat.TRANSLUCENT
        override fun getIntrinsicWidth(): Int = size
        override fun getIntrinsicHeight(): Int = size
    }

    companion object {
        // Persist launcher config across Activity recreation (process stays alive via foreground service)
        private var savedIgnoreList: Array<String>? = null
        private var savedExtraApps: Array<DesktopEntry>? = null

        private val ICON_COLORS = intArrayOf(
            0xFF4285F4.toInt(), 0xFFEA4335.toInt(), 0xFFFBBC05.toInt(), 0xFF34A853.toInt(),
            0xFF9C27B0.toInt(), 0xFFFF5722.toInt(), 0xFF00BCD4.toInt(), 0xFF607D8B.toInt(),
            0xFFE91E63.toInt(), 0xFF3F51B5.toInt(), 0xFF009688.toInt(), 0xFF795548.toInt(),
        )

        private val ICON_EXTENSIONS = arrayOf(".png", ".svg", ".xpm")
        private val ICON_SIZES = intArrayOf(256, 128, 64, 48, 32, 24, 22)
        private val ICON_THEMES = arrayOf("hicolor", "AdwaitaLegacy", "Adwaita")
        private val ICON_SUBDIRS = arrayOf("apps", "legacy")

        init {
            System.loadLibrary("android_wayland_launcher")
        }

        @JvmStatic
        private external fun nativeLaunchApp(command: String)
    }
}
