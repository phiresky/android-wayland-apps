package io.github.phiresky.wayland_android;

import android.app.Activity;
import android.app.NativeActivity;
import android.content.Intent;
import android.os.Bundle;
import android.view.Gravity;
import android.widget.Button;
import android.widget.LinearLayout;

/**
 * Main launcher activity with buttons to start Wayland applications.
 * The compositor (NativeActivity) is started on demand.
 */
public class LauncherActivity extends Activity {

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        LinearLayout layout = new LinearLayout(this);
        layout.setOrientation(LinearLayout.VERTICAL);
        layout.setGravity(Gravity.CENTER);
        layout.setPadding(64, 64, 64, 64);

        Button terminalButton = new Button(this);
        terminalButton.setText("Launch Terminal");
        terminalButton.setTextSize(20);
        terminalButton.setOnClickListener(v -> {
            Intent intent = new Intent(this, NativeActivity.class);
            startActivity(intent);
        });

        layout.addView(terminalButton);
        setContentView(layout);
    }
}
