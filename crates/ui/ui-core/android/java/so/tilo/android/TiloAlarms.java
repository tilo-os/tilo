package so.tilo.android;

import android.app.AlarmManager;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.ApplicationInfo;
import android.net.Uri;
import java.util.Map;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

/**
 * Notifications the platform shows at a later moment whether or not the app runs then: an alarm
 * set for this phone rings while it is offline, asleep, or the app was ended.
 *
 * <p>Each is handed to {@link AlarmManager#setAlarmClock}, which wakes the device exactly and
 * shows the alarm in the status bar, with a broadcast to this receiver; the receiver shows it as
 * a full-screen alarm notification. What is armed is kept in the app's preferences, so a reboot
 * arms it again and the app can list what is still to come ({@link #scheduled}): one that is no
 * longer listed rang.
 */
public final class TiloAlarms extends BroadcastReceiver {
    private static final String PREFERENCES = "tilo.alarms";
    private static final String CHANNEL = "tilo.alarms";
    private static final String RING = "so.tilo.android.ALARM";
    private static final String TAG = "tag";
    private static final int ID = 1;

    /**
     * Arm the notification {@code tag} at {@code atMs} (ms since the epoch), replacing one armed
     * under the same tag; each button is a label and the URI the app is opened with when it is
     * pressed. Returns why it was not armed, or null.
     */
    public static String schedule(
            Context context,
            String tag,
            String title,
            String body,
            long atMs,
            String[] labels,
            String[] activations) {
        JSONObject alarm = new JSONObject();
        try {
            alarm.put("title", title);
            alarm.put("body", body);
            alarm.put("at", atMs);
            JSONArray buttons = new JSONArray();
            for (int i = 0; i < labels.length && i < activations.length; i++) {
                buttons.put(new JSONObject().put("label", labels[i]).put("uri", activations[i]));
            }
            alarm.put("buttons", buttons);
        } catch (JSONException e) {
            return "the alarm does not write: " + e.getMessage();
        }
        String refused = arm(context, tag, atMs);
        if (refused != null) {
            return refused;
        }
        preferences(context).edit().putString(tag, alarm.toString()).apply();
        return null;
    }

    /** Take back the notification armed under {@code tag}. Returns why not, or null. */
    public static String cancel(Context context, String tag) {
        AlarmManager alarms = context.getSystemService(AlarmManager.class);
        if (alarms == null) {
            return "this device keeps no alarms";
        }
        alarms.cancel(ringing(context, tag));
        preferences(context).edit().remove(tag).apply();
        return null;
    }

    /** What is armed and still to ring: one line per notification, its tag, a tab, its moment. */
    public static String scheduled(Context context) {
        StringBuilder out = new StringBuilder();
        for (Map.Entry<String, ?> entry : preferences(context).getAll().entrySet()) {
            try {
                long at = new JSONObject(String.valueOf(entry.getValue())).getLong("at");
                out.append(entry.getKey()).append('\t').append(at).append('\n');
            } catch (JSONException e) {
                // A record this version cannot read lists as nothing armed.
            }
        }
        return out.toString();
    }

    @Override
    public void onReceive(Context context, Intent intent) {
        if (Intent.ACTION_BOOT_COMPLETED.equals(intent.getAction())) {
            for (Map.Entry<String, ?> entry : preferences(context).getAll().entrySet()) {
                try {
                    long at = new JSONObject(String.valueOf(entry.getValue())).getLong("at");
                    arm(context, entry.getKey(), at);
                } catch (JSONException e) {
                    preferences(context).edit().remove(entry.getKey()).apply();
                }
            }
            return;
        }
        String tag = intent.getStringExtra(TAG);
        if (tag == null) {
            return;
        }
        SharedPreferences preferences = preferences(context);
        String kept = preferences.getString(tag, null);
        preferences.edit().remove(tag).apply();
        if (kept == null) {
            return;
        }
        try {
            ring(context, tag, new JSONObject(kept));
        } catch (JSONException e) {
            // A record this version cannot read rings nothing.
        }
    }

    private static String arm(Context context, String tag, long atMs) {
        AlarmManager alarms = context.getSystemService(AlarmManager.class);
        if (alarms == null) {
            return "this device keeps no alarms";
        }
        Intent launch = context.getPackageManager().getLaunchIntentForPackage(context.getPackageName());
        PendingIntent shows =
                launch == null
                        ? ringing(context, tag)
                        : PendingIntent.getActivity(
                                context, tag.hashCode(), launch, PendingIntent.FLAG_IMMUTABLE);
        try {
            alarms.setAlarmClock(
                    new AlarmManager.AlarmClockInfo(atMs, shows), ringing(context, tag));
        } catch (SecurityException e) {
            return "this app may not set alarms: " + e.getMessage();
        }
        return null;
    }

    private static void ring(Context context, String tag, JSONObject alarm) throws JSONException {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager == null) {
            return;
        }
        ApplicationInfo app = context.getApplicationInfo();
        if (manager.getNotificationChannel(CHANNEL) == null) {
            NotificationChannel channel =
                    new NotificationChannel(
                            CHANNEL,
                            app.loadLabel(context.getPackageManager()),
                            NotificationManager.IMPORTANCE_HIGH);
            channel.setBypassDnd(true);
            manager.createNotificationChannel(channel);
        }
        Intent launch = context.getPackageManager().getLaunchIntentForPackage(context.getPackageName());
        Notification.Builder builder =
                new Notification.Builder(context, CHANNEL)
                        .setSmallIcon(app.icon != 0 ? app.icon : android.R.drawable.ic_lock_idle_alarm)
                        .setContentTitle(alarm.getString("title"))
                        .setContentText(alarm.optString("body"))
                        .setCategory(Notification.CATEGORY_ALARM)
                        .setOngoing(true)
                        .setAutoCancel(true);
        if (launch != null) {
            PendingIntent opens =
                    PendingIntent.getActivity(
                            context, tag.hashCode(), launch, PendingIntent.FLAG_IMMUTABLE);
            builder.setContentIntent(opens).setFullScreenIntent(opens, true);
            JSONArray buttons = alarm.optJSONArray("buttons");
            for (int i = 0; buttons != null && i < buttons.length(); i++) {
                JSONObject button = buttons.getJSONObject(i);
                Intent pressed =
                        new Intent(launch)
                                .setAction(Intent.ACTION_VIEW)
                                .setData(Uri.parse(button.getString("uri")));
                builder.addAction(
                        new Notification.Action.Builder(
                                        null,
                                        button.getString("label"),
                                        PendingIntent.getActivity(
                                                context,
                                                (tag + i).hashCode(),
                                                pressed,
                                                PendingIntent.FLAG_IMMUTABLE))
                                .build());
            }
        }
        manager.notify(tag, ID, builder.build());
    }

    private static PendingIntent ringing(Context context, String tag) {
        Intent intent =
                new Intent(context, TiloAlarms.class)
                        .setAction(RING)
                        .setData(Uri.fromParts("tilo-alarm", tag, null))
                        .putExtra(TAG, tag);
        return PendingIntent.getBroadcast(
                context,
                0,
                intent,
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
    }

    private static SharedPreferences preferences(Context context) {
        return context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE);
    }
}
