// SPDX-License-Identifier: MIT
package org.tilo.android.acceptance;

import android.app.Activity;
import android.app.Notification;
import android.app.NotificationManager;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.os.Bundle;
import android.widget.TextView;
import java.io.File;
import java.io.FileOutputStream;
import java.nio.charset.StandardCharsets;

/** Deliberately requests no Tilo grants. Runs inside actual ART, not a JVM mock. */
public final class AcceptanceActivity extends Activity {
    private TextView status;
    private boolean permissions;
    private boolean picker;
    private void record(String name, String value) {
        try (FileOutputStream out = new FileOutputStream(new File(getFilesDir(), name), true)) {
            out.write((value + "\n").getBytes(StandardCharsets.UTF_8));
        } catch (Exception e) { throw new RuntimeException(e); }
    }
    private void require(boolean condition, String message) {
        if (!condition) throw new IllegalStateException(message);
    }
    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        status = new TextView(this);
        status.setText("Waiting for Android API results");
        setContentView(status);
        for (String permission : new String[] {"android.permission.READ_EXTERNAL_STORAGE",
                "android.permission.WRITE_EXTERNAL_STORAGE", "android.permission.CAMERA",
                "android.permission.RECORD_AUDIO", "android.permission.ACCESS_FINE_LOCATION"}) {
            require(checkSelfPermission(permission) == PackageManager.PERMISSION_DENIED, permission);
        }
        require(getPackageManager().checkPermission(null, getPackageName()) == PackageManager.PERMISSION_DENIED,
                "null permission was not denied");
        NotificationManager notifications = (NotificationManager)getSystemService(NOTIFICATION_SERVICE);
        require(!notifications.areNotificationsEnabled(), "unqualified notifications advertised enabled");
        require("freedesktop".equals(System.getenv("GNOTIFICATION_BACKEND")), "notification backend bypass");
        notifications.notify(7, new Notification.Builder(this).setContentTitle("Android acceptance")
                .setContentText("No host bus grant").build());
        notifications.cancel(7);
        record("acceptance.txt", "notification-capability=unavailable;backend=filtered-Tilo");
        requestPermissions(new String[] {"android.permission.CAMERA", "android.permission.WRITE_EXTERNAL_STORAGE"}, 19);
        startActivityForResult(new Intent("android.intent.action.OPEN_DOCUMENT").setType("text/plain"), 23);
    }
    @Override public void onRequestPermissionsResult(int code, String[] names, int[] grants) {
        require(code == 19 && names.length == 2 && grants.length == 2, "permission callback shape");
        require(grants[0] == PackageManager.PERMISSION_DENIED && grants[1] == PackageManager.PERMISSION_DENIED,
                "permission callback granted authority");
        permissions = true;
        record("acceptance.txt", "permissions=denied;callback=19");
        complete();
    }
    @Override protected void onActivityResult(int code, int result, Intent data) {
        require(code == 23 && result == RESULT_CANCELED, "ungranted picker must cancel");
        require(data == null || data.getData() == null, "cancel returned a path");
        picker = true;
        record("acceptance.txt", "picker=cancelled;callback=23;uri=none");
        complete();
    }
    private void complete() {
        if (permissions && picker) {
            status.setText("Permissions denied\nPicker cancelled\nNotifications unavailable");
            record("acceptance.txt", "complete=true");
        }
    }
    @Override protected void onPause() { record("lifecycle.txt", "pause"); super.onPause(); }
    @Override protected void onStop() { record("lifecycle.txt", "stop"); super.onStop(); }
    @Override protected void onDestroy() { record("lifecycle.txt", "destroy"); super.onDestroy(); }
}
