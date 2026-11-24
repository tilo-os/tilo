package so.tilo.android;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ApplicationInfo;
import android.graphics.drawable.Icon;
import android.net.Uri;

/**
 * Posts and withdraws the app's notifications on one channel.
 *
 * <p>A notification carries a link its app opens ({@code tilo-phone://…}): the launch intent
 * carries it as {@link #EXTRA_TOLD}, and the app reads it as a tap. Its buttons are actions, each
 * opening its own link the same way.
 */
public final class TiloNotifications {
    /** The launch extra a notification's link, or a button's, is handed to the app in. */
    public static final String EXTRA_TOLD = "tilo.phone.told";

    private static final String CHANNEL = "tilo";
    private static final int ID = 0;

    private TiloNotifications() {}

    /**
     * Show a notification under {@code tag}, with the links of its buttons and of itself; returns
     * why it was not shown, or null.
     */
    public static String show(
            Context context,
            String tag,
            String title,
            String body,
            String activation,
            String[] buttonLabels,
            String[] buttonLinks) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager == null) {
            return "this device has no notification service";
        }
        ApplicationInfo app = context.getApplicationInfo();
        if (manager.getNotificationChannel(CHANNEL) == null) {
            manager.createNotificationChannel(
                    new NotificationChannel(
                            CHANNEL,
                            app.loadLabel(context.getPackageManager()),
                            NotificationManager.IMPORTANCE_DEFAULT));
        }
        if (!manager.areNotificationsEnabled()) {
            return "notifications are turned off for this app";
        }
        NotificationChannel channel = manager.getNotificationChannel(CHANNEL);
        if (channel != null && channel.getImportance() == NotificationManager.IMPORTANCE_NONE) {
            return "this app's notifications are turned off";
        }
        Notification.Builder builder =
                new Notification.Builder(context, CHANNEL)
                        .setSmallIcon(
                                app.icon != 0 ? app.icon : android.R.drawable.ic_dialog_info)
                        .setContentTitle(title)
                        .setContentText(body)
                        .setStyle(new Notification.BigTextStyle().bigText(body))
                        .setAutoCancel(true);
        Intent launch =
                context.getPackageManager().getLaunchIntentForPackage(context.getPackageName());
        if (launch != null) {
            builder.setContentIntent(
                    pending(context, launch, activation, tag.hashCode()));
            int icon = app.icon != 0 ? app.icon : android.R.drawable.ic_dialog_info;
            for (int i = 0; i < buttonLabels.length && i < buttonLinks.length; i++) {
                builder.addAction(
                        new Notification.Action.Builder(
                                        Icon.createWithResource(context, icon),
                                        buttonLabels[i],
                                        pending(context, launch, buttonLinks[i], tag.hashCode() + i
                                                        + 1))
                                .build());
            }
        }
        manager.notify(tag, ID, builder.build());
        return null;
    }

    /** The activity a tap on the notification, or on one of its buttons, starts. */
    private static PendingIntent pending(
            Context context, Intent launch, String link, int request) {
        Intent intent = new Intent(launch);
        if (link != null && !link.isEmpty()) {
            intent.putExtra(EXTRA_TOLD, link);
            intent.setData(Uri.parse(link));
        }
        intent.addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP);
        return PendingIntent.getActivity(
                context,
                request,
                intent,
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
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
