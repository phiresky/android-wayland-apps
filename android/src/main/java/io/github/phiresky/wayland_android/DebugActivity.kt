package io.github.phiresky.wayland_android

import android.app.Activity
import android.graphics.Typeface
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.TypedValue
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.Switch
import android.widget.TextView

/**
 * Full-screen activity showing live compositor debug info (clients, toplevels, FPS).
 * Auto-refreshes every second while visible. Includes a Vulkan/GLES render mode toggle.
 */
class DebugActivity : Activity() {

    private lateinit var content: TextView
    private lateinit var logContent: TextView
    private lateinit var logScroll: ScrollView
    private var userScrolled = false
    private var programmaticScroll = false
    private val handler = Handler(Looper.getMainLooper())
    private val refresh = object : Runnable {
        override fun run() {
            val status = MainActivity.getLastStatus()
            content.text = status.ifEmpty { "No compositor data yet" }
            logContent.text = nativeGetDebugLog()
            if (!userScrolled) {
                logContent.post {
                    programmaticScroll = true
                    logScroll.scrollTo(0, logContent.height)
                    programmaticScroll = false
                }
            }
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

        // Vulkan/GLES toggle
        val toggle = Switch(this).apply {
            text = "Vulkan rendering (new windows)"
            setTextColor(0xFFCCCCCC.toInt())
            isChecked = nativeGetVulkanRendering()
            setPadding(dp(16), dp(4), dp(16), dp(4))
            setOnCheckedChangeListener { _, isChecked ->
                nativeSetVulkanRendering(isChecked)
                text = if (isChecked) "Vulkan rendering (new windows)" else "GLES rendering (new windows)"
            }
        }
        root.addView(toggle)

        // PipeWire toggle
        val pipewireToggle = Switch(this).apply {
            text = "PipeWire (requires restart)"
            setTextColor(0xFFCCCCCC.toInt())
            isChecked = nativeGetPipewireEnabled()
            setPadding(dp(16), dp(4), dp(16), dp(4))
            setOnCheckedChangeListener { _, isChecked ->
                nativeSetPipewireEnabled(isChecked)
            }
        }
        root.addView(pipewireToggle)

        // Scrollable status content
        val scroll = ScrollView(this)

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
        root.addView(scroll, LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f
        ))

        // Log section title
        val logTitle = TextView(this).apply {
            text = "Tracing Log"
            setTextColor(0xFFE0E0E0.toInt())
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 18f)
            typeface = Typeface.DEFAULT_BOLD
            setPadding(dp(16), dp(12), dp(16), dp(4))
        }
        root.addView(logTitle)

        // Scrollable log content
        logScroll = ScrollView(this).apply {
            layoutParams = LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, 0, 2f
            )
            setOnScrollChangeListener { v, _, scrollY, _, _ ->
                if (programmaticScroll) return@setOnScrollChangeListener
                val sv = v as ScrollView
                val child = sv.getChildAt(0) ?: return@setOnScrollChangeListener
                val atBottom = scrollY + sv.height >= child.height - dp(16)
                userScrolled = !atBottom
            }
        }
        logContent = TextView(this).apply {
            typeface = Typeface.MONOSPACE
            setTextColor(0xFF99CC99.toInt())
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 11f)
            setPadding(dp(16), dp(8), dp(16), dp(16))
            setTextIsSelectable(true)
            text = nativeGetDebugLog()
        }
        logScroll.addView(logContent)
        root.addView(logScroll)

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

    private external fun nativeSetVulkanRendering(enabled: Boolean)
    private external fun nativeGetVulkanRendering(): Boolean
    private external fun nativeSetPipewireEnabled(enabled: Boolean)
    private external fun nativeGetPipewireEnabled(): Boolean
    private external fun nativeGetDebugLog(): String

    companion object {
        init {
            System.loadLibrary("android_wayland_launcher")
        }
    }
}
