package io.github.phiresky.wayland_android;

import android.app.Activity;
import android.os.Bundle;
import android.view.Surface;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.view.KeyEvent;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;
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
        setContentView(surfaceView);
        surfaceView.getHolder().addCallback(this);

        sInstances.put(windowId, new WeakReference<>(this));

        // Handle touch on the SurfaceView directly so coordinates are relative
        // to the rendering surface, not the Activity window (which includes
        // DeX title bar / window chrome).
        surfaceView.setOnTouchListener((v, event) -> {
            return nativeOnTouchEvent(windowId, event.getAction(), event.getX(), event.getY());
        });
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
}
