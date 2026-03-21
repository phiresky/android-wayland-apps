package io.github.phiresky.wayland_android

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.PixelFormat
import android.graphics.Typeface
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.provider.Settings
import android.util.Log
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.view.WindowManager
import android.widget.TextView

/**
 * Main entry point for the Wayland compositor.
 * Loads the native library and calls nativeInit() to start the compositor
 * on a background thread. Also hosts setup and status overlays.
 */
class MainActivity : Activity() {

    private var needsSetup = false
    private var overlayShown = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        // Force the window to draw a frame, which dismisses the splash screen.
        val placeholder = View(this)
        placeholder.setBackgroundColor(0xFF111111.toInt())
        setContentView(placeholder)
        reportFullyDrawn()

        // Initialize native compositor (only on first create, not config changes).
        if (savedInstanceState == null) {
            if (compositorStarted) {
                // Compositor already running — show the launcher directly.
                startActivity(
                    Intent(this, LauncherActivity::class.java)
                        .addFlags(Intent.FLAG_ACTIVITY_REORDER_TO_FRONT)
                )
            } else {
                // Restore persisted PipeWire toggle before nativeInit reads it
                val prefs = getSharedPreferences("compositor_prefs", MODE_PRIVATE)
                val pwEnabled = prefs.getBoolean("pipewire_enabled", false)
                nativeSetPipewireEnabled(pwEnabled)

                needsSetup = nativeInit(this)
                compositorStarted = true
            }
        }

        // Request full external storage access (Android 11+). Required so Linux apps
        // in proot can read/write files on shared storage (Downloads, etc.).
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R
            && !Environment.isExternalStorageManager()
        ) {
            startActivity(
                Intent(
                    Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
                    Uri.parse("package:$packageName")
                )
            )
        }

        // Request notification and camera permissions, then start the foreground service.
        // Camera permission causes Android to add the app to the 'camera' group, allowing
        // direct /dev/video* access from proot Linux apps (e.g. qv4l2, ffmpeg).
        val permsToRequest = mutableListOf<String>()
        if (checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
            != PackageManager.PERMISSION_GRANTED
        ) {
            permsToRequest.add(Manifest.permission.POST_NOTIFICATIONS)
        }
        if (checkSelfPermission(Manifest.permission.CAMERA)
            != PackageManager.PERMISSION_GRANTED
        ) {
            permsToRequest.add(Manifest.permission.CAMERA)
        }
        if (permsToRequest.isNotEmpty()) {
            requestPermissions(permsToRequest.toTypedArray(), 1)
        } else {
            startForegroundService(Intent(this, CompositorService::class.java))
        }
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        // Start service regardless — it works without notification permission,
        // the notification just won't be visible.
        startForegroundService(Intent(this, CompositorService::class.java))
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) {
            if (needsSetup && !overlayShown) {
                SetupOverlay.show(this)
                overlayShown = true
            }
            if (statusView == null) {
                addStatusOverlay()
            }
        }
    }

    private fun addStatusOverlay() {
        val view = TextView(this).apply {
            setTextColor(0xFFCCCCCC.toInt())
            setBackgroundColor(0xCC111111.toInt())
            typeface = Typeface.MONOSPACE
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 13f)
            setPadding(32, 32, 32, 32)
        }

        val params = WindowManager.LayoutParams(
            WindowManager.LayoutParams.WRAP_CONTENT,
            WindowManager.LayoutParams.WRAP_CONTENT,
            WindowManager.LayoutParams.TYPE_APPLICATION_PANEL,
            WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE
                or WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE,
            PixelFormat.TRANSLUCENT
        ).apply {
            gravity = Gravity.TOP or Gravity.START
            token = window.decorView.windowToken
        }

        windowManager.addView(view, params)
        statusView = view
    }

    override fun finish() {
        // Don't destroy the Activity — move to background instead.
        // The compositor runs independently on its own thread.
        moveTaskToBack(true)
    }

    override fun onDestroy() {
        super.onDestroy()
        statusView?.let { view ->
            try {
                windowManager.removeView(view)
            } catch (e: Exception) {
                // View may already be detached if the window was destroyed.
                Log.w("MainActivity", "removeView failed", e)
            }
            statusView = null
        }
    }

    companion object {
        @Volatile
        private var statusView: TextView? = null

        @Volatile
        private var lastStatus = ""

        private var compositorStarted = false

        init {
            System.loadLibrary("android_wayland_launcher")
        }

        // Native method: initializes compositor, returns true if first-run setup is needed.
        @JvmStatic
        private external fun nativeInit(activity: Activity): Boolean

        @JvmStatic
        private external fun nativeSetPipewireEnabled(enabled: Boolean)

        /** Called from native code via JNI to update the status text. */
        @JvmStatic
        fun updateStatus(text: String) {
            lastStatus = text
            val view = statusView
            view?.post { view.text = text }
        }

        /** Returns the last status text received from the compositor. */
        @JvmStatic
        fun getLastStatus(): String = lastStatus
    }
}
