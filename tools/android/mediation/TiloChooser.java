// SPDX-License-Identifier: MIT
package android.app;

import android.net.Uri;
import java.io.File;
import java.nio.charset.StandardCharsets;
import org.json.JSONArray;
import org.json.JSONObject;

/** Uses only the launcher-authenticated, already-scoped chooser connection. */
public final class TiloChooser {
    private static long nextId = 1;
    private static boolean loaded;
    private static boolean broken;
    private static native byte[] exchange(byte[] request);
    private static native boolean notificationServiceAvailable();
    private static native void markOngoingNative(long builder);
    private static native void recoveryJournalNative(String services);

    public static void recoveryJournal(String services) {
        if (System.getenv("TILO_ANDROID_RECOVERY_FD") == null) return;
        loadBridge();
        recoveryJournalNative(services);
    }

    static void markOngoing(long builder) {
        loadBridge();
        markOngoingNative(builder);
    }

    static synchronized void loadBridge() {
        if (loaded) return;
        String library = System.getenv("TILO_ANDROID_CHOOSER_LIBRARY");
        if (library == null) throw new IllegalStateException("no Tilo mediation bridge");
        System.load(library);
        loaded = true;
    }

    static boolean notificationsAvailable() {
        try {
            loadBridge();
            return "freedesktop".equals(System.getenv("GNOTIFICATION_BACKEND")) &&
                    notificationServiceAvailable();
        } catch (Exception unavailable) { return false; }
    }

    private static synchronized String ask(int action, String mime, String name) throws Exception {
        if (broken) throw new IllegalStateException("chooser stream is unusable");
        try { return exchangeRequest(action, mime, name); }
        catch (Exception failure) { broken = true; throw failure; }
    }

    private static String exchangeRequest(int action, String mime, String name) throws Exception {
        loadBridge();
        if (action < 0 || action > 2) throw new IllegalArgumentException("unsupported chooser");
        long id = nextId++;
        JSONObject request = new JSONObject();
        request.put("type", "pick");
        request.put("id", id);
        request.put("mode", new String[] {"open", "save", "directory"}[action]);
        request.put("multiple", false);
        request.put("title", new String[] {"Open file", "Save file", "Select folder"}[action]);
        request.put("accept_label", "");
        if (action == 1 && name != null) request.put("suggested_name", name);
        JSONArray filters = new JSONArray();
        if (mime != null && !mime.contains("*")) {
            JSONObject filter = new JSONObject();
            filter.put("name", mime);
            filter.put("mimetypes", new JSONArray().put(mime));
            filters.put(filter);
        }
        request.put("filters", filters);
        byte[] replyBytes = exchange((request.toString() + "\n").getBytes(StandardCharsets.UTF_8));
        if (replyBytes == null) throw new IllegalStateException("chooser connection ended");
        JSONObject reply = new JSONObject(new String(replyBytes, StandardCharsets.UTF_8));
        if (reply.getLong("id") != id) throw new IllegalStateException("chooser reply ID mismatch");
        android.util.Log.i("TiloChooser", "reply from authenticated chooser: " + reply.getString("type"));
        String kind = reply.getString("type");
        if ("failed".equals(kind) || "cancelled".equals(kind) || "nobody".equals(kind)) return null;
        if (!"picked".equals(kind)) throw new IllegalStateException("unknown chooser reply");
        JSONArray paths = reply.getJSONArray("paths");
        if (paths.length() != 1) throw new IllegalStateException("expected one selected file");
        String path = paths.getString(0);
        File file = new File(path);
        if (!file.isAbsolute() || path.indexOf('\0') >= 0 ||
                !file.getCanonicalPath().equals(path))
            throw new IllegalStateException("invalid selected path");
        return Uri.fromFile(file).toString();
    }

    static void choose(final Activity activity, final int action, final String mime,
                       final String name, final int requestCode) {
        new Thread(new Runnable() {
            @Override public void run() {
                String selected = null;
                try { selected = ask(action, mime, name); }
                catch (Exception failure) { android.util.Log.w("TiloChooser", "request failed", failure); }
                final String uri = selected;
                activity.runOnUiThread(new Runnable() {
                    @Override public void run() {
                        if (!activity.isFinishing())
                            activity.fileChooserResultCallback(requestCode,
                                uri == null ? Activity.RESULT_CANCELED : Activity.RESULT_OK, action, uri);
                    }
                });
            }
        }, "tilo-android-chooser").start();
    }
}
