package io.github.phiresky.wayland_android

import android.app.Activity
import android.graphics.Typeface
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.TypedValue
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView

/**
 * Full-screen activity showing live compositor debug info (clients, toplevels, FPS).
 * Auto-refreshes every second while visible.
 */
class DebugActivity : Activity() {

    private lateinit var content: TextView
    private val handler = Handler(Looper.getMainLooper())
    private val refresh = object : Runnable {
        override fun run() {
            val status = MainActivity.getLastStatus()
            content.text = status.ifEmpty { "No compositor data yet" }
            handler.postDelayed(this, 1000)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        window.setDecorFitsSystemWindows(true)
        window.statusBarColor = 0xFF1A1A2E.toInt()
        window.navigationBarColor = 0xFF1A1A2E.toInt()

        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(0xFF1A1A2E.toInt())
        }

        // Title bar
        val title = TextView(this).apply {
            text = "Compositor Status"
            setTextColor(0xFFE0E0E0.toInt())
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 20f)
            typeface = Typeface.DEFAULT_BOLD
            setPadding(dp(16), dp(16), dp(16), dp(12))
        }
        root.addView(title)

        // Scrollable status content
        val scroll = ScrollView(this).apply {
            layoutParams = LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT,
                LinearLayout.LayoutParams.MATCH_PARENT
            )
        }

        content = TextView(this).apply {
            typeface = Typeface.MONOSPACE
            setTextColor(0xFFCCCCCC.toInt())
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 13f)
            setPadding(dp(16), dp(8), dp(16), dp(16))
            setTextIsSelectable(true)
            val status = MainActivity.getLastStatus()
            text = status.ifEmpty { "No compositor data yet" }
        }

        scroll.addView(content)
        root.addView(scroll)
        setContentView(root)
    }

    override fun onResume() {
        super.onResume()
        handler.post(refresh)
    }

    override fun onPause() {
        super.onPause()
        handler.removeCallbacks(refresh)
    }

    private fun dp(value: Int): Int =
        TypedValue.applyDimension(
            TypedValue.COMPLEX_UNIT_DIP, value.toFloat(), resources.displayMetrics
        ).toInt()
}
