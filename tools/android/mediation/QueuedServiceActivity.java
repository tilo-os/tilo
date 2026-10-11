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
import java.nio.file.Files;

/** The same signed APK cancels queued starts, then restarts and stops real services. */
public final class QueuedServiceActivity extends Activity {
    private void record(String name, String value) {
        ServiceActivity.append(new File(getFilesDir(), name), value);
    }
    private Intent worker(String action) { return new Intent(this, Worker.class).setAction(action); }
    @Override public void onCreate(Bundle saved) {
        super.onCreate(saved);
        File visits = new File(getFilesDir(), "visits.txt");
        int previous;
        try { previous = visits.exists() ? Files.readAllLines(visits.toPath()).size() : 0; }
        catch (Exception e) { throw new RuntimeException(e); }
        record("visits.txt", "visit=" + (previous + 1));
        record("services.txt", "activity-create=" + (previous + 1));
        record("services.txt", "camera-denied=" +
                (checkSelfPermission("android.permission.CAMERA") == PackageManager.PERMISSION_DENIED));
        TextView view = new TextView(this);
        view.setText("Queued worker cancellation\nVisit " + (previous + 1));
        setContentView(view);
        startService(worker("cancelled-first"));
        startService(worker("cancelled-second"));
        record("services.txt", "queued-stop=" + stopService(worker("stop")));
        record("services.txt", "queued-repeat-stop=" + stopService(worker("stop")));
        startService(worker("restart"));
        final Handler handler = new Handler(Looper.getMainLooper());
        handler.post(new Runnable() {
            @Override public void run() {
                record("services.txt", "running-stop=" + stopService(worker("stop")));
                record("services.txt", "running-repeat-stop=" + stopService(worker("stop")));
                startService(worker("late-restart"));
                handler.post(new Runnable() {
                    @Override public void run() {
                        record("services.txt", "late-stop=" + stopService(worker("stop")));
                        record("services.txt", "late-repeat-stop=" + stopService(worker("stop")));
                        record("services.txt", "journey-complete");
                    }
                });
            }
        });
    }
    @Override protected void onPause() { record("activity-lifecycle.txt", "pause"); super.onPause(); }
    @Override protected void onStop() { record("activity-lifecycle.txt", "stop"); super.onStop(); }
    @Override protected void onDestroy() { record("activity-lifecycle.txt", "destroy"); super.onDestroy(); }

    public static final class Worker extends Service {
        private void record(String value) {
            ServiceActivity.append(new File(getFilesDir(), "services.txt"), value);
        }
        private boolean main() { return Looper.myLooper() == Looper.getMainLooper(); }
        @Override public void onCreate() { record("service-create;main=" + main()); }
        @Override public int onStartCommand(Intent intent, int flags, int id) {
            record("start=" + id + ";action=" + intent.getAction() + ";main=" + main());
            return START_NOT_STICKY;
        }
        @Override public IBinder onBind(Intent intent) { return null; }
        @Override public void onDestroy() { record("service-destroy;main=" + main()); }
    }
}
