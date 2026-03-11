package io.github.phiresky.wayland_android;

import android.app.Activity;
import android.os.Bundle;
import android.view.MotionEvent;
import android.view.Surface;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.view.KeyEvent;
import android.view.WindowManager;

import androidx.core.view.WindowCompat;
import androidx.core.view.WindowInsetsCompat;
import androidx.core.view.WindowInsetsControllerCompat;

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

        // Edge-to-edge: let content draw behind system bars
        WindowCompat.setDecorFitsSystemWindows(getWindow(), false);
        getWindow().getAttributes().layoutInDisplayCutoutMode =
                WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;

        surfaceView = new SurfaceView(this);
        setContentView(surfaceView);
        surfaceView.getHolder().addCallback(this);

        // Immersive fullscreen: hide status bar and navigation bar
        WindowInsetsControllerCompat insetsController =
                WindowCompat.getInsetsController(getWindow(), surfaceView);
        insetsController.hide(WindowInsetsCompat.Type.systemBars());
        insetsController.setSystemBarsBehavior(
                WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
    }

    @Override
    public void surfaceCreated(SurfaceHolder holder) {
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
        nativeWindowClosed(windowId);
    }

    @Override
    public boolean onTouchEvent(MotionEvent event) {
        if (nativeOnTouchEvent(windowId, event.getAction(), event.getX(), event.getY())) {
            return true;
        }
        return super.onTouchEvent(event);
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
    private static native void nativeWindowClosed(int windowId);
    private static native boolean nativeOnTouchEvent(int windowId, int action, float x, float y);
    private static native boolean nativeOnKeyEvent(int windowId, int keyCode, int action, int metaState);
}
