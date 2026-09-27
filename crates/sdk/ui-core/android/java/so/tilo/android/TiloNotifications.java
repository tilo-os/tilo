package so.tilo.android;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ApplicationInfo;

/** Posts and withdraws the app's notifications on one channel. */
public final class TiloNotifications {
    private static final String CHANNEL = "tilo";
    private static final int ID = 0;

    private TiloNotifications() {}

    /** Show a notification under {@code tag}; returns why it was not shown, or null. */
    public static String show(Context context, String tag, String title, String body) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager == null) {
            return "this device has no notification service";
        }
        ApplicationInfo app = context.getApplicationInfo();
        if (manager.getNotificationChannel(CHANNEL) == null) {
            manager.createNotificationChannel(new NotificationChannel(
                    CHANNEL, app.loadLabel(context.getPackageManager()), NotificationManager.IMPORTANCE_DEFAULT));
        }
        if (!manager.areNotificationsEnabled()) {
            return "notifications are turned off for this app";
        }
        NotificationChannel channel = manager.getNotificationChannel(CHANNEL);
        if (channel != null && channel.getImportance() == NotificationManager.IMPORTANCE_NONE) {
            return "this app's notifications are turned off";
        }
        Notification.Builder builder = new Notification.Builder(context, CHANNEL)
                .setSmallIcon(app.icon != 0 ? app.icon : android.R.drawable.ic_dialog_info)
                .setContentTitle(title)
                .setContentText(body)
                .setStyle(new Notification.BigTextStyle().bigText(body))
                .setAutoCancel(true);
        Intent launch = context.getPackageManager().getLaunchIntentForPackage(context.getPackageName());
        if (launch != null) {
            builder.setContentIntent(PendingIntent.getActivity(
                    context, 0, launch, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT));
        }
        manager.notify(tag, ID, builder.build());
        return null;
    }

    public static String withdraw(Context context, String tag) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager == null) {
            return "this device has no notification service";
        }
        manager.cancel(tag, ID);
        return null;
    }
}
