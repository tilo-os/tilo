// SPDX-License-Identifier: MIT
package org.tilo.android.services;

import android.app.Activity;
import android.app.Service;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.os.Bundle;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.widget.TextView;
import java.io.File;
import java.io.FileOutputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;

/** Owned, unchanged before/after APK: two starts, stale stop, current stop. */
public final class ServiceActivity extends Activity {
    private void record(String name, String value) {
        append(new File(getFilesDir(), name), value);
    }
    static void append(File file, String value) {
        try (FileOutputStream out = new FileOutputStream(file, true)) {
            out.write((value + "\n").getBytes(StandardCharsets.UTF_8));
        } catch (Exception e) { throw new RuntimeException(e); }
    }
    @Override public void onCreate(Bundle saved) {
        super.onCreate(saved);
        File visits = new File(getFilesDir(), "visits.txt");
        int previous = 0;
        try {
            if (visits.exists()) previous = Files.readAllLines(visits.toPath()).size();
        } catch (Exception e) { throw new RuntimeException(e); }
        record("visits.txt", "visit=" + (previous + 1));
        record("services.txt", "activity-create=" + (previous + 1));
        record("services.txt", "camera-denied=" +
                (checkSelfPermission("android.permission.CAMERA") == PackageManager.PERMISSION_DENIED));
        TextView view = new TextView(this);
        view.setText("Private worker lifecycle\nVisit " + (previous + 1));
        setContentView(view);
        startService(new Intent(this, Worker.class));
        startService(new Intent(this, Worker.class));
    }
    @Override protected void onPause() { record("activity-lifecycle.txt", "pause"); super.onPause(); }
    @Override protected void onStop() { record("activity-lifecycle.txt", "stop"); super.onStop(); }
    @Override protected void onDestroy() { record("activity-lifecycle.txt", "destroy"); super.onDestroy(); }

    public static final class Worker extends Service {
        private int first;
        private int starts;
        private void record(String value) { append(new File(getFilesDir(), "services.txt"), value); }
        @Override public void onCreate() { record("service-create;main=" + main()); }
        private boolean main() { return Looper.myLooper() == Looper.getMainLooper(); }
        @Override public int onStartCommand(Intent intent, int flags, final int id) {
            record("start=" + id + ";main=" + main());
            if (++starts == 1) first = id;
            else new Handler(Looper.getMainLooper()).post(new Runnable() {
                @Override public void run() {
                    record("stale-stop=" + stopSelfResult(first));
                    record("current-stop=" + stopSelfResult(id));
                }
            });
            return START_NOT_STICKY;
        }
        @Override public IBinder onBind(Intent intent) { return null; }
        @Override public void onDestroy() { record("service-destroy;main=" + main()); }
    }
}
