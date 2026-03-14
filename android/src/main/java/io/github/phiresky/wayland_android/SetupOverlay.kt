package io.github.phiresky.wayland_android

import android.app.Activity
import android.graphics.PixelFormat
import android.graphics.Typeface
import android.os.Handler
import android.os.Looper
import android.util.TypedValue
import android.view.Gravity
import android.view.WindowManager
import android.widget.ScrollView
import android.widget.TextView
import java.util.Collections

/**
 * Adds a full-screen log overlay on top of the MainActivity's window
 * during first-run proot setup. Uses WindowManager to place the view above
 * the native rendering surface.
 */
object SetupOverlay {

    private val lines: MutableList<String> = Collections.synchronizedList(mutableListOf())

    @Volatile
    private var logView: TextView? = null

    @Volatile
    private var scrollView: ScrollView? = null
    private var handler: Handler? = null
    private var lastFlushed = 0

    /** Called from native code via JNI. Adds the overlay on the UI thread. */
    @JvmStatic
    fun show(activity: Activity) {
        lines.clear()
        lastFlushed = 0
        activity.runOnUiThread {
            val sv = ScrollView(activity)
            val lv = TextView(activity).apply {
                setTextColor(0xFFCCCCCC.toInt())
                setBackgroundColor(0xFF111111.toInt())
                typeface = Typeface.MONOSPACE
                setTextSize(TypedValue.COMPLEX_UNIT_SP, 11f)
                setPadding(32, 48, 32, 32)
                text = "=== Setup starting ===\n"
            }
            sv.addView(lv)

            val params = WindowManager.LayoutParams(
                WindowManager.LayoutParams.MATCH_PARENT,
                WindowManager.LayoutParams.MATCH_PARENT,
                WindowManager.LayoutParams.TYPE_APPLICATION_PANEL,
                WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE
                    or WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN,
                PixelFormat.OPAQUE
            ).apply {
                gravity = Gravity.FILL
                token = activity.window.decorView.windowToken
            }

            activity.windowManager.addView(sv, params)

            logView = lv
            scrollView = sv
            handler = Handler(Looper.getMainLooper()).also {
                it.post(::flushLines)
            }
        }
    }

    private fun flushLines() {
        val lv = logView ?: return
        val sv = scrollView ?: return

        synchronized(lines) {
            for (i in lastFlushed until lines.size) {
                lv.append(lines[i] + "\n")
            }
            lastFlushed = lines.size
        }
        sv.post { sv.fullScroll(ScrollView.FOCUS_DOWN) }
        handler?.postDelayed(::flushLines, 100)
    }

    /** Called from native code via JNI to append a log line. Thread-safe. */
    @JvmStatic
    fun appendLog(line: String) {
        lines.add(line)
    }

    /** Called from native code via JNI when setup is complete. Removes the overlay. */
    @JvmStatic
    fun hide(activity: Activity) {
        activity.runOnUiThread {
            // Flush remaining lines before removing the overlay.
            flushLines()
            handler?.removeCallbacksAndMessages(null)
            scrollView?.let { sv ->
                try {
                    activity.windowManager.removeView(sv)
                } catch (_: Exception) {
                    // View might not be attached
                }
            }
            logView = null
            scrollView = null
            handler = null
        }
    }
}
