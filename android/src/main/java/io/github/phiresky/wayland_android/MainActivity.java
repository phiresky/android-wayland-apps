package io.github.phiresky.wayland_android;

import android.app.NativeActivity;
import android.os.Bundle;
import android.view.View;
import android.view.ViewGroup;
import android.widget.FrameLayout;

/**
 * Subclass of NativeActivity that forces a first frame draw to dismiss
 * the Android 12+ splash screen before native setup blocks android_main.
 */
public class MainActivity extends NativeActivity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        // Force the window to draw a frame, which dismisses the splash screen.
        // NativeActivity has taken the surface via takeSurface(), so normal
        // content views won't trigger a frame. Adding a view and requesting
        // layout forces the decor view to draw.
        View placeholder = new View(this);
        placeholder.setBackgroundColor(0xFF111111);
        addContentView(placeholder,
                new ViewGroup.LayoutParams(
                        ViewGroup.LayoutParams.MATCH_PARENT,
                        ViewGroup.LayoutParams.MATCH_PARENT));

        // reportFullyDrawn tells the system we've drawn meaningful content
        reportFullyDrawn();
    }
}
