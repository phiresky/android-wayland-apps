package io.github.phiresky.wayland_android;

import android.app.Activity;
import android.os.Bundle;
import android.view.GestureDetector;
import android.view.MotionEvent;
import android.view.Surface;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.view.View;
import android.view.KeyEvent;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;
import android.widget.FrameLayout;
import android.widget.PopupMenu;
import java.lang.ref.WeakReference;
import java.util.concurrent.ConcurrentHashMap;

/**
 * Each Wayland XDG toplevel gets its own instance of this Activity.
 * The native compositor creates EGL surfaces from the SurfaceView
 * and renders each client's buffer to its corresponding Activity.
 */
public class WaylandWindowActivity extends Activity implements SurfaceHolder.Callback {
    private static final ConcurrentHashMap<Integer, WeakReference<WaylandWindowActivity>> sInstances =
            new ConcurrentHashMap<>();

    private int windowId = -1;
    private SurfaceView surfaceView;
    private View menuAnchor;
    private boolean longPressActive = false;

    static {
        System.loadLibrary("android_wayland_launcher");
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        windowId = getIntent().getIntExtra("window_id", -1);
        if (windowId < 0) {
            finish();
            return;
        }

        // SDK 35 forces edge-to-edge; opt out so content doesn't render behind system bars
        getWindow().setDecorFitsSystemWindows(true);

        // Custom SurfaceView that presents itself as a text editor so the
        // Android soft keyboard can attach to it when requested.
        surfaceView = new SurfaceView(this) {
            @Override
            public boolean onCheckIsTextEditor() {
                return true;
            }

            @Override
            public InputConnection onCreateInputConnection(EditorInfo outAttrs) {
                outAttrs.inputType = android.text.InputType.TYPE_CLASS_TEXT;
                outAttrs.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN;
                return new BaseInputConnection(this, false);
            }
        };
        surfaceView.setFocusable(true);
        surfaceView.setFocusableInTouchMode(true);

        // Wrap in FrameLayout so we can position a tiny anchor view for the popup menu.
        FrameLayout container = new FrameLayout(this);
        container.addView(surfaceView);
        menuAnchor = new View(this);
        menuAnchor.setLayoutParams(new FrameLayout.LayoutParams(1, 1));
        container.addView(menuAnchor);
        setContentView(container);

        surfaceView.getHolder().addCallback(this);

        sInstances.put(windowId, new WeakReference<>(this));

        // Long-press detector: shows context menu with right-click and keyboard options.
        final int wid = windowId;
        GestureDetector gestureDetector = new GestureDetector(this,
                new GestureDetector.SimpleOnGestureListener() {
                    @Override
                    public void onLongPress(MotionEvent e) {
                        longPressActive = true;
                        // Release the left button that was sent on ACTION_DOWN.
                        nativeOnTouchEvent(wid, MotionEvent.ACTION_UP, e.getX(), e.getY());
                        showLongPressMenu(e.getX(), e.getY());
                    }
                });

        // Handle touch on the SurfaceView directly so coordinates are relative
        // to the rendering surface, not the Activity window (which includes
        // DeX title bar / window chrome).
        surfaceView.setOnTouchListener((v, event) -> {
            gestureDetector.onTouchEvent(event);

            if (longPressActive) {
                // Suppress touch events while the popup menu is showing.
                // Resume normal forwarding once the finger lifts.
                int action = event.getActionMasked();
                if (action == MotionEvent.ACTION_UP || action == MotionEvent.ACTION_CANCEL) {
                    longPressActive = false;
                }
                return true;
            }

            return nativeOnTouchEvent(windowId, event.getAction(), event.getX(), event.getY());
        });
    }

    /** Show a context menu at the long-press position with right-click and keyboard options. */
    private void showLongPressMenu(float x, float y) {
        // Position the invisible anchor at the touch point.
        menuAnchor.setX(x);
        menuAnchor.setY(y);

        PopupMenu popup = new PopupMenu(this, menuAnchor);
        popup.getMenu().add(0, 1, 0, "Right click");
        popup.getMenu().add(0, 2, 0, "Show keyboard");
        popup.setOnMenuItemClickListener(item -> {
            switch (item.getItemId()) {
                case 1:
                    nativeRightClick(windowId, x, y);
                    return true;
                case 2:
                    InputMethodManager imm =
                            (InputMethodManager) getSystemService(INPUT_METHOD_SERVICE);
                    if (imm != null) {
                        surfaceView.requestFocus();
                        imm.showSoftInput(surfaceView, InputMethodManager.SHOW_IMPLICIT);
                    }
                    return true;
            }
            return false;
        });
        popup.show();
    }

    @Override
    public void surfaceCreated(SurfaceHolder holder) {
        if (windowId < 0) return; // Guard against stale restored activities
        nativeSurfaceCreated(windowId, holder.getSurface());
    }

    @Override
    public void surfaceChanged(SurfaceHolder holder, int format, int width, int height) {
        nativeSurfaceChanged(windowId, width, height);
    }

    @Override
    public void surfaceDestroyed(SurfaceHolder holder) {
        nativeSurfaceDestroyed(windowId);
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        sInstances.remove(windowId);
        // isFinishing() = true when user closed the window (back, X button, finish())
        // isFinishing() = false when Android is destroying for config change / memory
        nativeWindowClosed(windowId, isFinishing());
    }

    /**
     * Finish the Activity for the given window ID.
     * Called from native code when the Wayland client destroys its toplevel.
     */
    public static void finishByWindowId(int windowId) {
        WeakReference<WaylandWindowActivity> ref = sInstances.get(windowId);
        if (ref == null) return;
        WaylandWindowActivity activity = ref.get();
        if (activity == null) {
            sInstances.remove(windowId);
            return;
        }
        activity.runOnUiThread(activity::finish);
    }

    /**
     * Show or hide the Android soft keyboard on the Activity for the given window.
     * Called from native code when a Wayland client enables/disables text_input_v3.
     */
    public static void setSoftKeyboardVisible(int windowId, boolean visible) {
        WeakReference<WaylandWindowActivity> ref = sInstances.get(windowId);
        if (ref == null) return;
        WaylandWindowActivity activity = ref.get();
        if (activity == null) {
            sInstances.remove(windowId);
            return;
        }
        activity.runOnUiThread(() -> {
            InputMethodManager imm =
                    (InputMethodManager) activity.getSystemService(INPUT_METHOD_SERVICE);
            if (imm == null) return;
            if (visible) {
                activity.surfaceView.requestFocus();
                imm.showSoftInput(activity.surfaceView, InputMethodManager.SHOW_IMPLICIT);
            } else {
                imm.hideSoftInputFromWindow(activity.surfaceView.getWindowToken(), 0);
            }
        });
    }

    @Override
    public boolean onKeyDown(int keyCode, KeyEvent event) {
        if (nativeOnKeyEvent(windowId, keyCode, event.getAction(), event.getMetaState())) {
            return true;
        }
        return super.onKeyDown(keyCode, event);
    }

    @Override
    public boolean onKeyUp(int keyCode, KeyEvent event) {
        if (nativeOnKeyEvent(windowId, keyCode, event.getAction(), event.getMetaState())) {
            return true;
        }
        return super.onKeyUp(keyCode, event);
    }

    // Native methods implemented in Rust
    private static native void nativeSurfaceCreated(int windowId, Surface surface);
    private static native void nativeSurfaceChanged(int windowId, int width, int height);
    private static native void nativeSurfaceDestroyed(int windowId);
    private static native void nativeWindowClosed(int windowId, boolean isFinishing);
    private static native boolean nativeOnTouchEvent(int windowId, int action, float x, float y);
    private static native boolean nativeOnKeyEvent(int windowId, int keyCode, int action, int metaState);
    private static native void nativeRightClick(int windowId, float x, float y);
}
