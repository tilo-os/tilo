// SPDX-License-Identifier: MIT
package org.tilo.android.acceptance;

import android.app.Activity;
import android.app.Notification;
import android.app.NotificationManager;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.net.Uri;
import android.os.Bundle;
import android.widget.TextView;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;

/** Runs only with explicitly granted, disposable acceptance documents. */
public final class IntegrationActivity extends Activity {
    private TextView status;
    private boolean revoked;
    private NotificationManager notifications;
    private void record(String name, String text) {
        try (FileOutputStream out = new FileOutputStream(new File(getFilesDir(), name), true)) {
            out.write((text + "\n").getBytes(StandardCharsets.UTF_8));
        } catch (Exception failure) { throw new RuntimeException(failure); }
    }
    private void require(boolean condition, String message) {
        if (!condition) throw new IllegalStateException(message);
    }
    private String read(InputStream input) throws Exception {
        require(input != null, "no document stream");
        try (InputStream in = input; ByteArrayOutputStream out = new ByteArrayOutputStream()) {
            byte[] buffer = new byte[256];
            int count;
            while ((count = in.read(buffer)) != -1) {
                require(out.size() + count <= 4096, "oversized fixture document");
                out.write(buffer, 0, count);
            }
            return new String(out.toByteArray(), StandardCharsets.UTF_8);
        }
    }
    private void storageStillDenied() {
        require(checkSelfPermission("android.permission.READ_EXTERNAL_STORAGE") == PackageManager.PERMISSION_DENIED,
                "document granted blanket read permission");
        require(checkSelfPermission("android.permission.WRITE_EXTERNAL_STORAGE") == PackageManager.PERMISSION_DENIED,
                "document granted blanket write permission");
    }
    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        status = new TextView(this);
        status.setText("Waiting for approved test documents");
        status.setContentDescription("Approved Android document status");
        require("Approved Android document status".contentEquals(status.getContentDescription()),
                "Android content description did not round trip");
        setContentView(status);
        storageStillDenied();
        revoked = new File(getFilesDir(), "revoke-fixture").isFile();
        if (revoked) {
            try {
                String paths = read(new FileInputStream(new File(getFilesDir(), "selected-paths.txt")));
                for (String path : paths.split("\n")) {
                    boolean denied = false;
                    try (InputStream in = getContentResolver().openInputStream(Uri.fromFile(new File(path)))) {
                        if (in != null) in.read();
                    } catch (Exception expected) { denied = true; }
                    require(denied, "revoked document remains readable");
                    denied = false;
                    try (OutputStream out = getContentResolver().openOutputStream(Uri.fromFile(new File(path)))) {
                        if (out != null) out.write(1);
                    } catch (Exception expected) { denied = true; }
                    require(denied, "revoked document remains writable");
                }
                record("integration.txt", "revoked-document-read=denied");
                record("integration.txt", "revoked-document-write=denied");
            } catch (Exception failure) { throw new RuntimeException(failure); }
        } else {
            notifications = (NotificationManager)getSystemService(NOTIFICATION_SERVICE);
            require(notifications.areNotificationsEnabled(), "Tilo notification provider unavailable");
            notifications.notify(31, new Notification.Builder(this).setContentTitle("Android integration fixture")
                    .setContentText("Approved document round trip").setOngoing(true).build());
            record("integration.txt", "notification=submitted;provider=filtered-Tilo");
        }
        startActivityForResult(new Intent("android.intent.action.OPEN_DOCUMENT").setType("text/plain"), 41);
    }
    @Override protected void onActivityResult(int code, int result, Intent data) {
        if (revoked) {
            require(code == 41 && result == RESULT_CANCELED && (data == null || data.getData() == null),
                    "revoked picker returned authority");
            record("integration.txt", "revoked-picker=cancelled;uri=none");
            record("integration.txt", "revocation-complete=true");
            status.setText("Revoked document access denied");
            return;
        }
        require(result == RESULT_OK && data != null && data.getData() != null, "approved picker did not succeed");
        Uri uri = data.getData();
        require("file".equals(uri.getScheme()), "unexpected document authority");
        record("selected-paths.txt", uri.getPath());
        try {
            if (code == 41) {
                require("Tilo approved Android fixture\n".equals(read(getContentResolver().openInputStream(uri))),
                        "incorrect document content");
                boolean readOnly = false;
                try (OutputStream out = getContentResolver().openOutputStream(uri)) {
                    if (out != null) out.write(1);
                } catch (Exception expected) { readOnly = true; }
                require(readOnly, "read-only grant permitted a write");
                boolean siblingDenied = false;
                try (InputStream in = new FileInputStream(new File(new File(uri.getPath()).getParentFile(), "private-canary"))) {
                    in.read();
                } catch (Exception expected) { siblingDenied = true; }
                require(siblingDenied, "ungranted sibling was exposed");
                record("integration.txt", "document-read=confirmed;write=denied;sibling=denied");
                startActivityForResult(new Intent("android.intent.action.CREATE_DOCUMENT").setType("text/plain")
                        .putExtra(Intent.EXTRA_TITLE, "android-output.txt"), 42);
            } else {
                require(code == 42, "unexpected document request code");
                try (OutputStream out = getContentResolver().openOutputStream(uri)) {
                    require(out != null, "no output stream");
                    out.write("Android document write confirmed\n".getBytes(StandardCharsets.UTF_8));
                }
                storageStillDenied();
                notifications.cancel(31);
                record("integration.txt", "document-write=confirmed;blanket-storage=denied");
                record("integration.txt", "notification=withdraw-requested");
                record("integration.txt", "complete=true");
                status.setText("Approved document read/write\nStorage remains scoped\nNotification withdrawn");
            }
        } catch (Exception failure) { throw new RuntimeException(failure); }
    }
    @Override protected void onPause() { record("integration-lifecycle.txt", "pause"); super.onPause(); }
    @Override protected void onStop() { record("integration-lifecycle.txt", "stop"); super.onStop(); }
    @Override protected void onDestroy() { record("integration-lifecycle.txt", "destroy"); super.onDestroy(); }
}
