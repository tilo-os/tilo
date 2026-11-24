package so.tilo.android;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.net.Uri;
import android.util.Log;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;

/**
 * Pushes from a UnifiedPush distributor the person installed, and the taps on what they showed.
 *
 * <p>The connector protocol is here in its own words, without the distributor's library: this
 * receiver is declared in the app's manifest for the connector's and the distributor's broadcasts,
 * asks which distributor the person chose, registers with it, keeps the endpoint it answers, and
 * receives each message. The distributor is an app of the person's; a push is encrypted for this
 * device's own key, so neither it nor the push service reads a word.
 *
 * <p>What a message carries is handed to Rust ({@link #nativePush}), which opens it with the key
 * the app keeps and answers the notification's tag, title and body. This class only shows it, and
 * only an app whose manifest declares {@code so.tilo.unified-push} does any of it.
 *
 * <p>A tap on a notification writes the link it opened into the app's files, and Rust reads it
 * there: a notification arrives when the app may not be running, so nothing may be held in memory.
 */
public class TiloPush extends BroadcastReceiver {
    /** This app's connection to its distributor, as the connector protocol names it. */
    public static final String INSTANCE = "tilo";

    /** The meta-data an app's manifest declares to take pushes; nothing else does. */
    private static final String DECLARED = "so.tilo.unified-push";

    private static final String REGISTER = "org.unifiedpush.android.connector.REGISTER";
    private static final String GET_DISTRIBUTOR =
            "org.unifiedpush.android.connector.GET_DISTRIBUTOR";
    private static final String REGISTRATION = "org.unifiedpush.android.connector.REGISTRATION";
    private static final String UNREGISTERED = "org.unifiedpush.android.connector.UNREGISTERED";
    private static final String MESSAGE = "org.unifiedpush.android.connector.MESSAGE";
    private static final String REGISTRATION_FAILED =
            "org.unifiedpush.android.connector.REGISTRATION_FAILED";
    private static final String DISTRIBUTOR = "org.unifiedpush.android.distributor.DISTRIBUTOR";

    private static final String PREFS = "tilo-push";
    private static final String DISTRIBUTOR_KEY = "distributor";
    private static final String TAG = "TiloPush";

    /**
     * Attach this app to the distributor its person chose, where its manifest declares pushes, and
     * read what a launch was tapped from.
     */
    public static void install(Context context, Intent intent) {
        if (!declared(context)) {
            return;
        }
        rememberTap(context, intent);
        ensureRegistered(context);
    }

    /** Ask the person's distributor to keep this app reachable, looking for it first if none is known. */
    public static void ensureRegistered(Context context) {
        String distributor = prefs(context).getString(DISTRIBUTOR_KEY, null);
        if (distributor == null) {
            distributor = distributorOf(context);
            if (distributor != null) {
                prefs(context).edit().putString(DISTRIBUTOR_KEY, distributor).apply();
            }
        }
        if (distributor == null) {
            // A distributor that answers the asking announces itself, and is kept above.
            context.sendBroadcast(new Intent(GET_DISTRIBUTOR));
            return;
        }
        Intent register = new Intent(REGISTER);
        register.setPackage(distributor);
        register.putExtra("instance", INSTANCE);
        context.sendBroadcast(register);
    }

    /**
     * The distributor installed on this device that takes this app's registration, if one is: the
     * connector protocol's own receiver, found where the system lists it.
     */
    private static String distributorOf(Context context) {
        PackageManager manager = context.getPackageManager();
        for (ResolveInfo receiver : manager.queryBroadcastReceivers(new Intent(REGISTER), 0)) {
            String packageName = receiver.activityInfo != null
                    ? receiver.activityInfo.packageName
                    : null;
            if (packageName != null && !packageName.equals(context.getPackageName())) {
                return packageName;
            }
        }
        return null;
    }

    /** Where the distributor leaves the endpoint it gave this app, which Rust hands to its host. */
    private static File pushDir(Context context) {
        File dir = new File(context.getFilesDir(), "tilo-push");
        if (!dir.isDirectory() && !dir.mkdirs() && !dir.isDirectory()) {
            Log.w(TAG, "the push directory could not be made: " + dir);
        }
        return dir;
    }

    private static SharedPreferences prefs(Context context) {
        return context.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
    }

    /** Whether this app's manifest declares that pushes reach it. */
    private static boolean declared(Context context) {
        try {
            ApplicationInfo info =
                    context.getPackageManager()
                            .getApplicationInfo(
                                    context.getPackageName(), PackageManager.GET_META_DATA);
            return info.metaData != null && info.metaData.getBoolean(DECLARED, false);
        } catch (PackageManager.NameNotFoundException e) {
            return false;
        }
    }

    private static void write(File file, String text) {
        try (FileOutputStream out = new FileOutputStream(file)) {
            out.write(text.getBytes(StandardCharsets.UTF_8));
        } catch (IOException e) {
            Log.w(TAG, "could not write " + file + ": " + e);
        }
    }

    /**
     * The link a tap opened, kept for Rust: `/tmp`-less, in the app's own files as everything else
     * here is. A launch that opened nothing leaves the last one, which was read already.
     */
    public static void rememberTap(Context context, Intent intent) {
        if (intent == null || !declared(context)) {
            return;
        }
        String link = intent.getStringExtra(TiloNotifications.EXTRA_TOLD);
        if (link != null && !link.isEmpty()) {
            write(new File(pushDir(context), "tapped"), link);
        }
    }

    @Override
    public void onReceive(Context context, Intent intent) {
        if (intent == null || !declared(context)) {
            return;
        }
        String action = intent.getAction();
        if (action == null) {
            return;
        }
        switch (action) {
            case DISTRIBUTOR:
                {
                    String distributor = intent.getStringExtra("distributor");
                    if (distributor == null && intent.getData() != null) {
                        Uri data = intent.getData();
                        distributor =
                                data.getSchemeSpecificPart() != null
                                        ? data.getSchemeSpecificPart()
                                        : data.getHost();
                    }
                    if (distributor == null || distributor.isEmpty()) {
                        Log.w(TAG, "a distributor answered naming no package");
                        return;
                    }
                    prefs(context).edit().putString(DISTRIBUTOR_KEY, distributor).apply();
                    ensureRegistered(context);
                    return;
                }
            case REGISTRATION:
                {
                    if (!INSTANCE.equals(intent.getStringExtra("instance"))) {
                        return;
                    }
                    String endpoint = intent.getStringExtra("endpoint");
                    if (endpoint == null || endpoint.isEmpty()) {
                        Log.w(TAG, "a distributor registered this app with no endpoint");
                        return;
                    }
                    write(new File(pushDir(context), "endpoint"), endpoint);
                    return;
                }
            case UNREGISTERED:
                {
                    File endpoint = new File(pushDir(context), "endpoint");
                    if (endpoint.delete()) {
                        Log.i(TAG, "the distributor took this app's endpoint back");
                    }
                    return;
                }
            case REGISTRATION_FAILED:
                Log.w(TAG, "the distributor could not register this app");
                return;
            case MESSAGE:
                show(context, intent);
                return;
            default:
                return;
        }
    }

    /** Show the message `intent` carried: Rust opens it with this device's key. */
    private static void show(Context context, Intent intent) {
        if (!INSTANCE.equals(intent.getStringExtra("instance"))) {
            return;
        }
        byte[] body = intent.getByteArrayExtra("bytes");
        if (body == null || body.length == 0) {
            byte[] text = intent.getByteArrayExtra("bytesMessage");
            body = text;
        }
        if (body == null || body.length == 0) {
            Log.w(TAG, "a push carried no bytes");
            return;
        }
        load(context);
        String[] shown;
        try {
            shown = nativePush(context.getFilesDir().getAbsolutePath(), body);
        } catch (RuntimeException | UnsatisfiedLinkError e) {
            Log.w(TAG, "a push could not be opened: " + e);
            return;
        }
        if (shown == null || shown.length < 3 || shown[0] == null) {
            Log.w(TAG, "a push is not one Tilo reads");
            return;
        }
        if (shown[1] == null || shown[1].isEmpty()) {
            // The thing a push showed waits no more.
            TiloNotifications.withdraw(context, shown[0]);
            return;
        }
        // A push says nothing of where the thing is: the tap opens Tilo, which reads the person's
        // own log and shows the same notification again, with its buttons and what it opens.
        String words = shown[2] != null ? shown[2] : "";
        TiloNotifications.show(
                context, shown[0], shown[1], words, "", new String[0], new String[0]);
    }

    /** Load the app's Rust library, which the activity loads too when it runs. */
    private static void load(Context context) {
        try {
            ApplicationInfo info =
                    context.getPackageManager()
                            .getApplicationInfo(
                                    context.getPackageName(), PackageManager.GET_META_DATA);
            String library = info.metaData.getString("android.app.lib_name");
            if (library != null && !library.isEmpty()) {
                System.loadLibrary(library);
            }
        } catch (PackageManager.NameNotFoundException | UnsatisfiedLinkError e) {
            Log.w(TAG, "the app's library did not load: " + e);
        }
    }

    /**
     * Open `body` with this device's key: its tag, title and body, or a tag alone for a gone
     * thing. `files` is the app's own files, where the key is kept.
     */
    private static native String[] nativePush(String files, byte[] body);
}
