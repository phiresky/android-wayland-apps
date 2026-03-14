package io.github.phiresky.wayland_android

import android.app.Activity
import android.os.Bundle
import android.text.InputType
import android.view.GestureDetector
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.Surface
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import android.widget.FrameLayout
import android.widget.PopupMenu
import java.lang.ref.WeakReference
import java.util.concurrent.ConcurrentHashMap

/**
 * Each Wayland XDG toplevel gets its own instance of this Activity.
 * The native compositor creates EGL surfaces from the SurfaceView
 * and renders each client's buffer to its corresponding Activity.
 */
class WaylandWindowActivity : Activity(), SurfaceHolder.Callback {

    private var windowId = -1
    private lateinit var surfaceView: SurfaceView
    private lateinit var menuAnchor: View
    private var longPressActive = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        windowId = intent.getIntExtra("window_id", -1)
        if (windowId < 0) {
            finish()
            return
        }

        // SDK 35 forces edge-to-edge; opt out so content doesn't render behind system bars
        window.setDecorFitsSystemWindows(true)

        // Custom SurfaceView that presents itself as a text editor so the
        // Android soft keyboard can attach to it when requested.
        surfaceView = object : SurfaceView(this) {
            override fun onCheckIsTextEditor(): Boolean = true

            override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
                outAttrs.inputType = InputType.TYPE_CLASS_TEXT
                outAttrs.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN
                return BaseInputConnection(this, false)
            }
        }
        surfaceView.isFocusable = true
        surfaceView.isFocusableInTouchMode = true

        // Wrap in FrameLayout so we can position a tiny anchor view for the popup menu.
        val container = FrameLayout(this)
        container.addView(surfaceView)
        menuAnchor = View(this)
        menuAnchor.layoutParams = FrameLayout.LayoutParams(1, 1)
        container.addView(menuAnchor)
        setContentView(container)

        surfaceView.holder.addCallback(this)

        instances[windowId] = WeakReference(this)

        // Long-press detector: shows context menu with right-click and keyboard options.
        val gestureDetector = GestureDetector(
            this,
            object : GestureDetector.SimpleOnGestureListener() {
                override fun onLongPress(e: MotionEvent) {
                    longPressActive = true
                    // Release the left button that was sent on ACTION_DOWN.
                    nativeOnTouchEvent(windowId, MotionEvent.ACTION_UP, e.x, e.y)
                    showLongPressMenu(e.x, e.y)
                }
            }
        )

        // Handle touch on the SurfaceView directly so coordinates are relative
        // to the rendering surface, not the Activity window (which includes
        // DeX title bar / window chrome).
        surfaceView.setOnTouchListener { _, event ->
            gestureDetector.onTouchEvent(event)

            if (longPressActive) {
                // Suppress touch events while the popup menu is showing.
                // Resume normal forwarding once the finger lifts.
                val action = event.actionMasked
                if (action == MotionEvent.ACTION_UP || action == MotionEvent.ACTION_CANCEL) {
                    longPressActive = false
                }
                return@setOnTouchListener true
            }

            nativeOnTouchEvent(windowId, event.action, event.x, event.y)
        }
    }

    /** Show a context menu at the long-press position with right-click and keyboard options. */
    private fun showLongPressMenu(x: Float, y: Float) {
        // Position the invisible anchor at the touch point.
        menuAnchor.x = x
        menuAnchor.y = y

        val imm = getSystemService(INPUT_METHOD_SERVICE) as? InputMethodManager
        val keyboardVisible = imm != null && imm.isActive(surfaceView)

        val popup = PopupMenu(this, menuAnchor)
        popup.menu.add(0, 1, 0, "Right click")
        popup.menu.add(0, 2, 0, if (keyboardVisible) "Hide keyboard" else "Show keyboard")
        popup.setOnMenuItemClickListener { item ->
            when (item.itemId) {
                1 -> {
                    nativeRightClick(windowId, x, y)
                    true
                }
                2 -> {
                    if (imm != null) {
                        if (keyboardVisible) {
                            imm.hideSoftInputFromWindow(surfaceView.windowToken, 0)
                        } else {
                            surfaceView.requestFocus()
                            imm.showSoftInput(surfaceView, InputMethodManager.SHOW_IMPLICIT)
                        }
                    }
                    true
                }
                else -> false
            }
        }
        popup.show()
    }

    override fun surfaceCreated(holder: SurfaceHolder) {
        if (windowId < 0) return // Guard against stale restored activities
        nativeSurfaceCreated(windowId, holder.surface)
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        nativeSurfaceChanged(windowId, width, height)
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        nativeSurfaceDestroyed(windowId)
    }

    override fun onDestroy() {
        super.onDestroy()
        instances.remove(windowId)
        // isFinishing = true when user closed the window (back, X button, finish())
        // isFinishing = false when Android is destroying for config change / memory
        nativeWindowClosed(windowId, isFinishing)
    }

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        if (nativeOnKeyEvent(windowId, keyCode, event.action, event.metaState)) {
            return true
        }
        return super.onKeyDown(keyCode, event)
    }

    override fun onKeyUp(keyCode: Int, event: KeyEvent): Boolean {
        if (nativeOnKeyEvent(windowId, keyCode, event.action, event.metaState)) {
            return true
        }
        return super.onKeyUp(keyCode, event)
    }

    companion object {
        private val instances = ConcurrentHashMap<Int, WeakReference<WaylandWindowActivity>>()

        init {
            System.loadLibrary("android_wayland_launcher")
        }

        /** Look up a live Activity by window ID, cleaning up stale references. */
        private fun getByWindowId(windowId: Int): WaylandWindowActivity? {
            val ref = instances[windowId] ?: return null
            val activity = ref.get()
            if (activity == null) instances.remove(windowId)
            return activity
        }

        /**
         * Finish the Activity for the given window ID.
         * Called from native code when the Wayland client destroys its toplevel.
         */
        @JvmStatic
        fun finishByWindowId(windowId: Int) {
            val activity = getByWindowId(windowId) ?: return
            activity.runOnUiThread { activity.finish() }
        }

        /**
         * Show or hide the Android soft keyboard on the Activity for the given window.
         * Called from native code when a Wayland client enables/disables text_input_v3.
         */
        @JvmStatic
        fun setSoftKeyboardVisible(windowId: Int, visible: Boolean) {
            val activity = getByWindowId(windowId) ?: return
            activity.runOnUiThread {
                val imm = activity.getSystemService(INPUT_METHOD_SERVICE) as? InputMethodManager
                    ?: return@runOnUiThread
                if (visible) {
                    activity.surfaceView.requestFocus()
                    imm.showSoftInput(activity.surfaceView, InputMethodManager.SHOW_IMPLICIT)
                } else {
                    imm.hideSoftInputFromWindow(activity.surfaceView.windowToken, 0)
                }
            }
        }

        // Native methods implemented in Rust
        @JvmStatic
        private external fun nativeSurfaceCreated(windowId: Int, surface: Surface)

        @JvmStatic
        private external fun nativeSurfaceChanged(windowId: Int, width: Int, height: Int)

        @JvmStatic
        private external fun nativeSurfaceDestroyed(windowId: Int)

        @JvmStatic
        private external fun nativeWindowClosed(windowId: Int, isFinishing: Boolean)

        @JvmStatic
        private external fun nativeOnTouchEvent(windowId: Int, action: Int, x: Float, y: Float): Boolean

        @JvmStatic
        private external fun nativeOnKeyEvent(windowId: Int, keyCode: Int, action: Int, metaState: Int): Boolean

        @JvmStatic
        private external fun nativeRightClick(windowId: Int, x: Float, y: Float)
    }
}
