package so.tilo.android;

import android.app.Activity;
import android.content.ActivityNotFoundException;
import android.content.ClipData;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.net.Uri;
import java.util.HashMap;
import java.util.Map;

/**
 * Routes activity results and permission answers back to the native host
 * service that asked for them. {@link TiloActivity} forwards
 * {@code onActivityResult} and {@code onRequestPermissionsResult} here; a
 * request code this class did not hand out is left to the activity.
 */
public final class TiloActivityResults {
    private static final int FIRST_REQUEST_CODE = 0x7100;
    private static final int LAST_REQUEST_CODE = 0x7fff;

    private static final Map<Integer, Long> PENDING = new HashMap<>();
    private static int nextRequestCode = FIRST_REQUEST_CODE;

    private TiloActivityResults() {}

    private static native void deliverResult(long token, int resultCode, String[] uris, String error);

    private static native void deliverPermissions(long token, boolean granted);

    /** Start {@code intent} for a result that is delivered to {@code token}. */
    public static void startForResult(final Activity activity, final Intent intent, final long token) {
        activity.runOnUiThread(new Runnable() {
            @Override
            public void run() {
                int requestCode = register(token);
                try {
                    activity.startActivityForResult(intent, requestCode);
                } catch (ActivityNotFoundException e) {
                    take(requestCode);
                    deliverResult(token, Activity.RESULT_CANCELED, null,
                            "no app on this device handles " + intent.getAction());
                }
            }
        });
    }

    /**
     * Ask the user for {@code permissions}. The answer reaches {@code token}
     * as granted when the user allowed at least one of them, which is how a
     * precise-or-approximate location request resolves.
     */
    public static void requestPermissions(final Activity activity, final String[] permissions, final long token) {
        activity.runOnUiThread(new Runnable() {
            @Override
            public void run() {
                activity.requestPermissions(permissions, register(token));
            }
        });
    }

    public static boolean isGranted(Activity activity, String permission) {
        return activity.checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED;
    }

    public static boolean onActivityResult(int requestCode, int resultCode, Intent data) {
        Long token = take(requestCode);
        if (token == null) {
            return false;
        }
        deliverResult(token, resultCode, uris(data), null);
        return true;
    }

    public static boolean onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        Long token = take(requestCode);
        if (token == null) {
            return false;
        }
        boolean granted = false;
        for (int result : grantResults) {
            granted |= result == PackageManager.PERMISSION_GRANTED;
        }
        deliverPermissions(token, granted);
        return true;
    }

    private static String[] uris(Intent data) {
        if (data == null) {
            return new String[0];
        }
        ClipData clip = data.getClipData();
        if (clip != null && clip.getItemCount() > 0) {
            String[] uris = new String[clip.getItemCount()];
            for (int i = 0; i < uris.length; i++) {
                Uri uri = clip.getItemAt(i).getUri();
                uris[i] = uri == null ? "" : uri.toString();
            }
            return uris;
        }
        Uri uri = data.getData();
        return uri == null ? new String[0] : new String[] {uri.toString()};
    }

    private static synchronized int register(long token) {
        while (PENDING.containsKey(nextRequestCode)) {
            advance();
        }
        int requestCode = nextRequestCode;
        advance();
        PENDING.put(requestCode, token);
        return requestCode;
    }

    private static void advance() {
        nextRequestCode = nextRequestCode == LAST_REQUEST_CODE ? FIRST_REQUEST_CODE : nextRequestCode + 1;
    }

    private static synchronized Long take(int requestCode) {
        return PENDING.remove(requestCode);
    }
}
