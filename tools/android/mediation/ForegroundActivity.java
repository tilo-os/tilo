// SPDX-License-Identifier: MIT
package org.tilo.android.foreground;

import android.app.Activity;
import android.app.Notification;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.widget.TextView;
import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.List;

/** Only disposable granted documents; the identical APK runs before and after. */
public final class ForegroundActivity extends Activity {
    static void append(File file, String value) {
        try (FileOutputStream out = new FileOutputStream(file, true)) {
            out.write((value + "\n").getBytes(StandardCharsets.UTF_8));
        } catch (Exception e) { throw new RuntimeException(e); }
    }
    private void record(String value) { append(new File(getFilesDir(), "foreground.txt"), value); }
    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        File visits = new File(getFilesDir(), "visits.txt");
        int previous = 0;
        try { if (visits.exists()) previous = Files.readAllLines(visits.toPath()).size(); }
        catch (Exception e) { throw new RuntimeException(e); }
        append(visits, "visit=" + (previous + 1));
        record("visit=" + (previous + 1));
        TextView view = new TextView(this);
        view.setText("Scoped foreground worker\nVisit " + (previous + 1));
        setContentView(view);
        for (String permission : new String[] {"android.permission.CAMERA",
                "android.permission.READ_EXTERNAL_STORAGE", "android.permission.WRITE_EXTERNAL_STORAGE",
                "android.permission.FOREGROUND_SERVICE"}) {
            if (checkSelfPermission(permission) != PackageManager.PERMISSION_DENIED)
                throw new IllegalStateException("blanket permission granted: " + permission);
        }
        record("blanket-permissions=denied");
        if (new File(getFilesDir(), "revoke-fixture").exists()) {
            try {
                List<String> paths = Files.readAllLines(new File(getFilesDir(), "scope-inputs").toPath());
                Uri input = Uri.fromFile(new File(paths.get(0)));
                boolean denied = false;
                try (InputStream stream = getContentResolver().openInputStream(input)) { stream.read(); }
                catch (Exception expected) { denied = true; }
                if (!denied) throw new IllegalStateException("revoked source read allowed");
                record("revoked-read=denied");
                denied = false;
                try (OutputStream stream = getContentResolver().openOutputStream(Uri.fromFile(new File(paths.get(1))), "wa")) {
                    stream.write(1);
                } catch (Exception expected) { denied = true; }
                if (!denied) throw new IllegalStateException("revoked destination write allowed");
                record("revoked-write=denied");
                denied = false;
                try (Cursor cursor = getContentResolver().query(input, null, null, null, null)) {
                    denied = cursor == null;
                } catch (Exception expected) { denied = true; }
                if (!denied) throw new IllegalStateException("revoked metadata allowed");
                record("revoked-metadata=denied");
                record("revoked-relaunch=complete;service-not-started");
            } catch (Exception e) { throw new RuntimeException(e); }
            view.setText("Revoked document authority denied\nPrivate visits preserved");
            return;
        }
        NotificationManager manager = (NotificationManager)getSystemService(NOTIFICATION_SERVICE);
        manager.notify(60, new Notification.Builder(this).setContentTitle("Foreground transport control")
                .setContentText("Existing filtered provider").build());
        startService(new Intent(this, Worker.class));
    }
    @Override protected void onPause() { append(new File(getFilesDir(), "lifecycle.txt"), "pause"); super.onPause(); }
    @Override protected void onStop() { append(new File(getFilesDir(), "lifecycle.txt"), "stop"); super.onStop(); }
    @Override protected void onDestroy() { append(new File(getFilesDir(), "lifecycle.txt"), "destroy"); super.onDestroy(); }

    public static final class Worker extends Service {
        private volatile boolean stopped;
        private void record(String value) { append(new File(getFilesDir(), "foreground.txt"), value); }
        @Override public void onCreate() { record("service-create;main=" + (Looper.myLooper() == Looper.getMainLooper())); }
        @Override public IBinder onBind(Intent intent) { return null; }
        @Override public int onStartCommand(Intent intent, int flags, int id) {
            Notification notice = new Notification.Builder(this).setContentTitle("Android foreground fixture")
                    .setContentText("Scoped worker active").setOngoing(true).build();
            boolean rejected = false;
            try { startForeground(0, notice); } catch (IllegalArgumentException expected) { rejected = true; }
            record("zero-id-rejected=" + rejected);
            rejected = false;
            try { startForeground(61, null); } catch (IllegalArgumentException expected) { rejected = true; }
            record("null-notification-rejected=" + rejected);
            try { startForeground(61, notice); }
            catch (SecurityException expected) {
                record("foreground=denied;no-notification-capability");
                stopSelf();
                return START_NOT_STICKY;
            }
            record("foreground=returned;start=" + id);
            record("service-start;intent=" + (intent == null ? "null" : "present") + ";id=" + id);
            new Thread(new Runnable() {
                @Override public void run() {
                    try {
                        File inputs = new File(getFilesDir(), "scope-inputs");
                        for (int i = 0; !inputs.exists() && i < 600 && !stopped; i++) Thread.sleep(100);
                        if (stopped || !inputs.exists()) return;
                        List<String> paths = Files.readAllLines(inputs.toPath());
                        try (InputStream input = getContentResolver().openInputStream(Uri.fromFile(new File(paths.get(0))));
                             OutputStream output = getContentResolver().openOutputStream(Uri.fromFile(new File(paths.get(1))), "wa")) {
                            if (input.read() != 'T') throw new IllegalStateException("incorrect scoped source");
                            record("scoped-descriptors=open");
                            for (int i = 1; i <= 3000 && !stopped && !new File(getFilesDir(), "finish-worker").exists(); i++) {
                                output.write(("tick=" + i + "\n").getBytes(StandardCharsets.UTF_8));
                                output.flush();
                                Thread.sleep(100);
                            }
                        }
                        record("scoped-descriptors=closed");
                    } catch (Exception e) { record("worker-failed=" + e.getClass().getSimpleName()); }
                    finally {
                        new Handler(Looper.getMainLooper()).post(new Runnable() {
                            @Override public void run() { stopForeground(true); stopSelf(); }
                        });
                    }
                }
            }, "scoped-foreground-worker").start();
            record("return=START_STICKY");
            return START_STICKY;
        }
        @Override public void onDestroy() { stopped = true; record("service-destroy;main=" + (Looper.myLooper() == Looper.getMainLooper())); }
    }
}
