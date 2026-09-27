package so.tilo.android;

import android.app.NativeActivity;
import android.content.Intent;
import android.content.pm.ActivityInfo;
import android.content.pm.PackageManager;
import android.graphics.Insets;
import android.os.Build;
import android.os.Bundle;
import android.view.DisplayCutout;
import android.view.View;
import android.view.WindowInsets;

/**
 * The activity every Tilo app runs in on Android.
 *
 * <p>The app itself is Rust: {@link NativeActivity} loads the library named by the
 * {@code android.app.lib_name} meta-data and enters the {@code android_main} that
 * {@code ui::android_main!()} exports, which runs the app's ordinary {@code main}. This class
 * adds only what a native activity cannot do from Rust.
 *
 * <h2>Calling Rust from Java</h2>
 *
 * {@link #onCreate} loads the app library with {@link System#loadLibrary} before {@code
 * NativeActivity} starts it, so a {@code static native} method declared in any class of this
 * package resolves to the Rust function exported as {@code Java_so_tilo_android_<Class>_<method>}
 * ({@code #[unsafe(no_mangle)] pub extern "system" fn}). Such a method runs on the UI thread and
 * may run before {@code android_main} has started, so the Rust side records what it is given and
 * wakes the event loop rather than touching the toolkit directly. Rust calls Java through the
 * {@code JavaVM} and activity object that android-activity publishes in {@code ndk_context}.
 *
 * <h2>Extension points</h2>
 *
 * <ul>
 *   <li>{@link #installExtensions} attaches the package's other helpers, one line each.
 *   <li>Results Android delivers to the activity ({@code onActivityResult}, {@code
 *       onRequestPermissionsResult}, …) are overridden here and forwarded to their helper.
 * </ul>
 */
public class TiloActivity extends NativeActivity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        System.loadLibrary(libraryName());
        super.onCreate(savedInstanceState);
        getWindow().getDecorView().setOnApplyWindowInsetsListener(this::applyInsets);
        installExtensions();
    }

    private void installExtensions() {
        TiloTextInput.install(this);
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        // winit's event loop and the toolkit's process state can start only once per process; a
        // later launch must get a fresh one.
        System.exit(0);
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        if (!TiloActivityResults.onActivityResult(requestCode, resultCode, data)) {
            super.onActivityResult(requestCode, resultCode, data);
        }
    }

    @Override
    public void onRequestPermissionsResult(
            int requestCode, String[] permissions, int[] grantResults) {
        if (!TiloActivityResults.onRequestPermissionsResult(
                requestCode, permissions, grantResults)) {
            super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        }
    }

    private String libraryName() {
        try {
            ActivityInfo info =
                    getPackageManager()
                            .getActivityInfo(getComponentName(), PackageManager.GET_META_DATA);
            return info.metaData.getString(META_DATA_LIB_NAME);
        } catch (PackageManager.NameNotFoundException e) {
            throw new IllegalStateException("TiloActivity is not in its own manifest", e);
        }
    }

    /**
     * The window is drawn edge to edge; the app lays its content out inside whatever the system
     * bars, display cutout and soft keyboard leave uncovered. API 29 has only the since-deprecated
     * system-window insets.
     */
    @SuppressWarnings("deprecation")
    private WindowInsets applyInsets(View view, WindowInsets insets) {
        Insets safe;
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            safe =
                    Insets.max(
                            insets.getInsets(
                                    WindowInsets.Type.systemBars()
                                            | WindowInsets.Type.displayCutout()),
                            insets.getInsets(WindowInsets.Type.ime()));
        } else {
            safe = insets.getSystemWindowInsets();
            DisplayCutout cutout = insets.getDisplayCutout();
            if (cutout != null) {
                safe =
                        Insets.max(
                                safe,
                                Insets.of(
                                        cutout.getSafeInsetLeft(),
                                        cutout.getSafeInsetTop(),
                                        cutout.getSafeInsetRight(),
                                        cutout.getSafeInsetBottom()));
            }
        }
        nativeSafeAreaChanged(safe.left, safe.top, safe.right, safe.bottom);
        return view.onApplyWindowInsets(insets);
    }

    private static native void nativeSafeAreaChanged(int left, int top, int right, int bottom);
}
