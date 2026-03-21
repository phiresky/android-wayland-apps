package io.github.phiresky.wayland_android

import android.app.Activity
import android.os.Bundle
import android.text.InputType
import android.view.GestureDetector
import android.view.InputDevice
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
    /** Android InputType flags from Wayland content_type, used by onCreateInputConnection. */
    @Volatile var currentInputType = InputType.TYPE_CLASS_TEXT or
        InputType.TYPE_TEXT_FLAG_AUTO_CORRECT or InputType.TYPE_TEXT_FLAG_MULTI_LINE
    // Shared Editable across InputConnection instances so text context survives
    // IME restarts (Gboard restarts the connection before doing corrections).
    private val sharedEditable: android.text.Editable = android.text.SpannableStringBuilder().also {
        android.text.Selection.setSelection(it, 0)
    }
    /** Set to true when the compositor tells us to close (client destroyed its surface). */
    var closingByCompositor = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        windowId = intent.getIntExtra("window_id", -1)
        if (windowId < 0) {
            finish()
            return
        }

        // Custom SurfaceView that presents itself as a text editor so the
        // Android soft keyboard can attach to it when requested.
        surfaceView = object : SurfaceView(this) {
            override fun onCheckIsTextEditor(): Boolean = true

            // Intercept physical keyboard events BEFORE the IME framework sees them.
            // Without this, Gboard processes physical keys into commitText/setComposingText
            // AND the keys also arrive as wl_keyboard events, causing double input.
            override fun onKeyPreIme(keyCode: Int, event: KeyEvent): Boolean {
                val device = event.device
                if (device != null && !device.isVirtual
                    && device.keyboardType == InputDevice.KEYBOARD_TYPE_ALPHABETIC
                    && !event.isSystem) {
                    if (nativeOnKeyEvent(this@WaylandWindowActivity.windowId,
                            event.keyCode, event.action, event.metaState)) {
                        return true
                    }
                }
                return super.onKeyPreIme(keyCode, event)
            }

            override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
                outAttrs.inputType = this@WaylandWindowActivity.currentInputType
                outAttrs.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN or
                    EditorInfo.IME_ACTION_NONE
                val ed = this@WaylandWindowActivity.sharedEditable
                outAttrs.initialSelStart = android.text.Selection.getSelectionStart(ed)
                outAttrs.initialSelEnd = android.text.Selection.getSelectionEnd(ed)
                return WaylandInputConnection(this, this@WaylandWindowActivity.windowId, this@WaylandWindowActivity.sharedEditable)
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

        // SDK 35 ignores setDecorFitsSystemWindows(true) and enforces edge-to-edge.
        // Apply system bar insets as padding so the SurfaceView doesn't overlap them.
        container.setOnApplyWindowInsetsListener { v, insets ->
            val bars = insets.getInsets(android.view.WindowInsets.Type.systemBars())
            v.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            insets
        }

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
            // Physical mouse right-click: route to nativeRightClick instead of touch.
            if (event.buttonState and MotionEvent.BUTTON_SECONDARY != 0) {
                if (event.actionMasked == MotionEvent.ACTION_DOWN) {
                    nativeRightClick(windowId, event.x, event.y)
                }
                return@setOnTouchListener true
            }

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

        // Mouse hover events (movement without button pressed) come through
        // onHoverEvent, not onTouchEvent. Forward them for pointer tracking.
        surfaceView.setOnHoverListener { _, event ->
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
                    surfaceView.requestFocus()
                    val insetsController = surfaceView.windowInsetsController
                    if (insetsController != null) {
                        if (keyboardVisible) {
                            insetsController.hide(android.view.WindowInsets.Type.ime())
                        } else {
                            insetsController.show(android.view.WindowInsets.Type.ime())
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

    /**
     * Intercept user-initiated close (back button, DeX X button).
     * Instead of finishing immediately, ask the Wayland client to close.
     * The client may refuse (e.g. gedit's "save changes?" dialog).
     * The Activity only actually finishes when the compositor calls finishByWindowId().
     */
    override fun finish() {
        if (closingByCompositor) {
            super.finish()
        } else {
            nativeRequestClose(windowId)
        }
    }

    override fun onDestroy() {
        super.onDestroy()
        instances.remove(windowId)
        if (closingByCompositor) {
            // Compositor told us to close (client destroyed its surface) — clean up.
            nativeWindowClosed(windowId, true)
        } else if (isFinishing) {
            // User/system closed us (DeX X button, back, etc.) but we haven't asked
            // the client yet. Send a close request instead of killing the window.
            // If the client refuses (e.g. save dialog), the compositor will relaunch
            // a new Activity for this window.
            nativeRequestClose(windowId)
        } else {
            // Config change / memory pressure — keep toplevel alive.
            nativeWindowClosed(windowId, false)
        }
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

    /**
     * Custom InputConnection that forwards IME operations to the native compositor.
     * Sends full text with semantic type (composing/commit/delete) so the Rust side
     * can use text_input_v3 protocol when available, or fall back to key events.
     * Maintains an internal Editable (fullEditor=true) so autocorrect/prediction
     * have text context to work with.
     */
    private class WaylandInputConnection(
        private val view: View,
        private val windowId: Int,
        private val sharedEditable: android.text.Editable,
    ) : BaseInputConnection(view, true) {

        private var composingText = ""

        override fun getEditable(): android.text.Editable = sharedEditable

        /** Notify the IME about cursor position, composing region, and text content.
         *  Gboard relies on CursorAnchorInfo (not getTextBeforeCursor) to track editor state. */
        private fun notifyImeState() {
            val imm = view.context.getSystemService(android.content.Context.INPUT_METHOD_SERVICE)
                as? InputMethodManager ?: return
            val ed = editable
            val selStart = android.text.Selection.getSelectionStart(ed)
            val selEnd = android.text.Selection.getSelectionEnd(ed)
            val compStart = getComposingSpanStart(ed)
            val compEnd = getComposingSpanEnd(ed)
            imm.updateSelection(view, selStart, selEnd, compStart, compEnd)

            val builder = android.view.inputmethod.CursorAnchorInfo.Builder()
            builder.setSelectionRange(selStart, selEnd)
            builder.setMatrix(android.graphics.Matrix())
            if (compStart >= 0 && compEnd > compStart) {
                builder.setComposingText(compStart, ed.subSequence(compStart, compEnd))
            }
            imm.updateCursorAnchorInfo(view, builder.build())
        }

        override fun setComposingText(text: CharSequence, newCursorPosition: Int): Boolean {
            composingText = text.toString()
            nativeOnImeText(windowId, IME_COMPOSING, composingText, 0, 0)
            val result = super.setComposingText(text, newCursorPosition)
            notifyImeState()
            return result
        }

        override fun commitText(text: CharSequence, newCursorPosition: Int): Boolean {
            composingText = ""
            nativeOnImeText(windowId, IME_COMMIT, text.toString(), 0, 0)
            val result = super.commitText(text, newCursorPosition)
            notifyImeState()
            return result
        }

        override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
            if (beforeLength > 0 || afterLength > 0) {
                var deletedBefore = ""
                val editable = editable
                if (editable != null) {
                    val cursor = android.text.Selection.getSelectionStart(editable)
                    if (cursor >= 0) {
                        val start = (cursor - beforeLength).coerceAtLeast(0)
                        deletedBefore = editable.subSequence(start, cursor).toString()
                    }
                }
                nativeOnImeText(windowId, IME_DELETE, deletedBefore, beforeLength, afterLength)
            }
            val result = super.deleteSurroundingText(beforeLength, afterLength)
            notifyImeState()
            return result
        }

        override fun finishComposingText(): Boolean {
            if (composingText.isNotEmpty()) {
                nativeOnImeText(windowId, IME_COMMIT, composingText, 0, 0)
            }
            composingText = ""
            val result = super.finishComposingText()
            notifyImeState()
            return result
        }

        override fun setComposingRegion(start: Int, end: Int): Boolean {
            val editable = editable
            if (editable != null) {
                val s = start.coerceIn(0, editable.length)
                val e = end.coerceIn(0, editable.length)
                val regionStart = minOf(s, e)
                val regionEnd = maxOf(s, e)
                composingText = editable.subSequence(regionStart, regionEnd).toString()

                // Move cursor to end of composing region so subsequent operations
                // (backspaces, delete_surrounding_text) target the right text.
                val cursor = android.text.Selection.getSelectionStart(editable)
                if (cursor >= 0 && cursor != regionEnd) {
                    val delta = cursor - regionEnd
                    val keyCode = if (delta > 0)
                        KeyEvent.KEYCODE_DPAD_LEFT else KeyEvent.KEYCODE_DPAD_RIGHT
                    for (i in 0 until kotlin.math.abs(delta)) {
                        nativeOnKeyEvent(windowId, keyCode, KeyEvent.ACTION_DOWN, 0)
                        nativeOnKeyEvent(windowId, keyCode, KeyEvent.ACTION_UP, 0)
                    }
                }

                nativeOnImeText(windowId, IME_RECOMPOSE, composingText, 0, 0)
            }
            return super.setComposingRegion(start, end)
        }

        override fun replaceText(start: Int, end: Int, text: CharSequence, newCursorPosition: Int, textAttribute: android.view.inputmethod.TextAttribute?): Boolean {
            // API 34+: Gboard uses this for corrections (e.g. "hello" → "help").
            // Use arrow keys + backspace key events for the delete (works for both
            // text editors and terminals — terminals don't track surrounding text,
            // so text_input_v3 delete_surrounding_text is a no-op for them).
            val editable = editable
            val clampedEnd = if (editable != null) end.coerceAtMost(editable.length) else end
            // Move cursor to end of replacement range
            if (editable != null) {
                val cursor = android.text.Selection.getSelectionStart(editable)
                if (cursor >= 0 && cursor != clampedEnd) {
                    val delta = cursor - clampedEnd
                    val keyCode = if (delta > 0)
                        KeyEvent.KEYCODE_DPAD_LEFT else KeyEvent.KEYCODE_DPAD_RIGHT
                    for (i in 0 until kotlin.math.abs(delta)) {
                        nativeOnKeyEvent(windowId, keyCode, KeyEvent.ACTION_DOWN, 0)
                        nativeOnKeyEvent(windowId, keyCode, KeyEvent.ACTION_UP, 0)
                    }
                }
            }
            // Delete old text via backspace key events
            val deleteCount = clampedEnd - start
            for (i in 0 until deleteCount) {
                nativeOnKeyEvent(windowId, KeyEvent.KEYCODE_DEL, KeyEvent.ACTION_DOWN, 0)
                nativeOnKeyEvent(windowId, KeyEvent.KEYCODE_DEL, KeyEvent.ACTION_UP, 0)
            }
            // Insert new text via commit (uses text_input_v3 if available)
            nativeOnImeText(windowId, IME_COMMIT, text.toString(), 0, 0)
            val result = super.replaceText(start, end, text, newCursorPosition, textAttribute)
            notifyImeState()
            return result
        }

        override fun setSelection(start: Int, end: Int): Boolean {
            val editable = editable ?: return super.setSelection(start, end)
            val oldStart = android.text.Selection.getSelectionStart(editable)
            val result = super.setSelection(start, end)
            if (result && start == end && oldStart >= 0) {
                val delta = start - oldStart
                if (delta != 0) {
                    val keyCode = if (delta > 0)
                        KeyEvent.KEYCODE_DPAD_RIGHT else KeyEvent.KEYCODE_DPAD_LEFT
                    for (i in 0 until kotlin.math.abs(delta)) {
                        nativeOnKeyEvent(windowId, keyCode, KeyEvent.ACTION_DOWN, 0)
                        nativeOnKeyEvent(windowId, keyCode, KeyEvent.ACTION_UP, 0)
                    }
                }
            }
            return result
        }

        override fun sendKeyEvent(event: KeyEvent): Boolean {
            nativeOnKeyEvent(windowId, event.keyCode, event.action, event.metaState)
            return true
        }

        override fun requestCursorUpdates(cursorUpdateMode: Int): Boolean = true

        companion object {
            const val IME_COMPOSING = 0
            const val IME_COMMIT = 1
            const val IME_DELETE = 2
            const val IME_RECOMPOSE = 3
        }
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
            activity.runOnUiThread {
                activity.closingByCompositor = true
                activity.finish()
            }
        }

        /**
         * Show or hide the Android soft keyboard on the Activity for the given window.
         * Called from native code when a Wayland client enables/disables text_input_v3.
         */
        @JvmStatic
        fun setSoftKeyboardVisible(windowId: Int, visible: Boolean, androidInputType: Int) {
            val activity = getByWindowId(windowId) ?: return
            activity.runOnUiThread {
                val imm = activity.getSystemService(INPUT_METHOD_SERVICE) as? InputMethodManager
                    ?: return@runOnUiThread
                if (visible) {
                    val changed = activity.currentInputType != androidInputType
                    activity.currentInputType = androidInputType
                    activity.surfaceView.requestFocus()
                    if (changed) {
                        imm.restartInput(activity.surfaceView)
                    }
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
        private external fun nativeOnImeText(windowId: Int, imeType: Int, text: String, deleteBefore: Int, deleteAfter: Int)

        @JvmStatic
        private external fun nativeRequestClose(windowId: Int)

        @JvmStatic
        private external fun nativeRightClick(windowId: Int, x: Float, y: Float)
    }
}
