package io.github.phiresky.wayland_android;

import android.app.Activity;
import android.os.Bundle;
import android.view.Surface;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.view.KeyEvent;

/**
 * Each Wayland XDG toplevel gets its own instance of this Activity.
 * The native compositor creates EGL surfaces from the SurfaceView
 * and renders each client's buffer to its corresponding Activity.
 */
public class WaylandWindowActivity extends Activity implements SurfaceHolder.Callback {
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

        surfaceView = new SurfaceView(this);
        setContentView(surfaceView);
        surfaceView.getHolder().addCallback(this);

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
        // isFinishing() = true when user closed the window (back, X button, finish())
        // isFinishing() = false when Android is destroying for config change / memory
        nativeWindowClosed(windowId, isFinishing());
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
