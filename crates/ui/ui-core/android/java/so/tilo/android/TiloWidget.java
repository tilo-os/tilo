package so.tilo.android;

import android.app.PendingIntent;
import android.appwidget.AppWidgetManager;
import android.appwidget.AppWidgetProvider;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.net.Uri;
import android.util.Log;
import android.view.View;
import android.widget.RemoteViews;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.File;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;

/**
 * The home screen's card of what waits for the person: the snapshot the app's Rust writes ({@code data/tilo/widget.json}), in the words their notifications
 * show ({@code words_outside}), so a phone on a table shows no more than its lock screen. It is
 * drawn again wherever a notice stands or goes ({@link #refresh}), and a tap opens the thing.
 */
public class TiloWidget extends AppWidgetProvider {
    private static final String TAG = "TiloWidget";
    private static final String SNAPSHOT = "data/tilo/widget.json";
    private static final String WAITING = "Waiting for you";
    private static final String NOTHING = "Nothing waits for you";

    @Override
    public void onUpdate(Context context, AppWidgetManager manager, int[] ids) {
        for (int id : ids) draw(context, manager, id);
    }

    /** Draw every card again: what the person was told moved. */
    public static void refresh(Context context) {
        AppWidgetManager manager = AppWidgetManager.getInstance(context);
        for (int id : manager.getAppWidgetIds(new ComponentName(context, TiloWidget.class))) {
            draw(context, manager, id);
        }
    }

    private static int named(Context context, String name, String kind) {
        return context.getResources().getIdentifier(name, kind, context.getPackageName());
    }

    private static void show(Context context, RemoteViews views, String view, boolean visible) {
        views.setViewVisibility(named(context, view, "id"), visible ? View.VISIBLE : View.GONE);
    }

    private static void say(Context context, RemoteViews views, String view, String words) {
        views.setTextViewText(named(context, view, "id"), words);
    }

    private static void draw(Context context, AppWidgetManager manager, int widget) {
        RemoteViews views = new RemoteViews(context.getPackageName(), named(context, "tilo_widget", "layout"));
        JSONObject snapshot = snapshot(context);
        JSONArray items = snapshot.optJSONArray("items");
        int shown = 0;
        for (int index = 0; index < 3; index++) {
            String row = "row" + index;
            JSONObject item = items == null ? null : items.optJSONObject(index);
            show(context, views, row, item != null);
            if (item == null) continue;
            say(context, views, row + "_title", item.optString("title"));
            say(context, views, row + "_body", item.optString("body"));
            PendingIntent open = tap(context, item.optString("link"), widget * 4 + index);
            if (open != null) views.setOnClickPendingIntent(named(context, row, "id"), open);
            shown += 1;
        }
        say(context, views, "header", WAITING);
        say(context, views, "empty", NOTHING);
        show(context, views, "empty", shown == 0);
        manager.updateAppWidget(widget, views);
    }

    /** The snapshot the app wrote, or nothing where it wrote none. */
    private static JSONObject snapshot(Context context) {
        try {
            byte[] bytes = Files.readAllBytes(new File(context.getFilesDir(), SNAPSHOT).toPath());
            return new JSONObject(new String(bytes, StandardCharsets.UTF_8));
        } catch (Exception e) {
            Log.d(TAG, "nothing to show yet: " + e);
            return new JSONObject();
        }
    }

    /** What a tap on one line opens: a notification's link, or the app where a push named it. */
    private static PendingIntent tap(Context context, String link, int request) {
        Intent launch =
                context.getPackageManager().getLaunchIntentForPackage(context.getPackageName());
        if (launch == null) return null;
        Intent open = new Intent(launch);
        if (link != null && !link.isEmpty()) {
            open.putExtra(TiloNotifications.EXTRA_TOLD, link);
            open.setData(Uri.parse(link));
        }
        open.addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP);
        return PendingIntent.getActivity(
                context,
                request,
                open,
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
    }
}
