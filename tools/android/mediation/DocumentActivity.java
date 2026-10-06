// SPDX-License-Identifier: MIT
package org.tilo.android.acceptance;

import android.app.Activity;
import android.content.ContentResolver;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.os.CancellationSignal;
import android.os.OperationCanceledException;
import android.widget.TextView;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;

/** One immutable SDK fixture exercises ordinary document-reader/editor APIs. */
public final class DocumentActivity extends Activity {
    private Uri source;
    private int failures;
    private TextView status;
    private static final String SOURCE = "Tilo Android document metadata\n";

    private String expectedName() throws Exception {
        File configuration = new File(getFilesDir(), "expected-document-name.txt");
        if (!configuration.exists()) return "android-π-📄.txt";
        byte[] bytes = new byte[128];
        try (FileInputStream input = new FileInputStream(configuration)) {
            int count = input.read(bytes);
            require(count > 0 && input.read() == -1, "unbounded expected filename");
            String name = new String(bytes, 0, count, StandardCharsets.UTF_8);
            require(name.matches("Shared-[0-9a-f]{12}"), "invalid exact-record fixture name");
            return name;
        }
    }

    private void record(String line) {
        try (FileOutputStream out = new FileOutputStream(new File(getFilesDir(), "documents.txt"), true)) {
            out.write((line + "\n").getBytes(StandardCharsets.UTF_8));
        } catch (Exception failure) { throw new RuntimeException(failure); }
    }

    private static void require(boolean value, String message) {
        if (!value) throw new IllegalStateException(message);
    }

    private String read(Uri uri) throws Exception {
        ByteArrayOutputStream bytes = new ByteArrayOutputStream();
        try (InputStream in = getContentResolver().openInputStream(uri)) {
            require(in != null, "missing stream");
            byte[] block = new byte[128];
            int count;
            while ((count = in.read(block)) != -1) {
                require(bytes.size() + count <= 4096, "unbounded fixture data");
                bytes.write(block, 0, count);
            }
        }
        return new String(bytes.toByteArray(), StandardCharsets.UTF_8);
    }

    private void write(Uri uri, String mode, String text) throws Exception {
        try (OutputStream out = getContentResolver().openOutputStream(uri, mode)) {
            require(out != null, "missing write stream");
            out.write(text.getBytes(StandardCharsets.UTF_8));
        }
    }

    private void metadata() throws Exception {
        ContentResolver resolver = getContentResolver();
        try (Cursor cursor = resolver.query(source, null, null, null, null)) {
            require(cursor != null && cursor.getCount() == 1 && cursor.moveToFirst(), "missing metadata");
            require(expectedName().equals(cursor.getString(cursor.getColumnIndexOrThrow("_display_name"))),
                    "expected document name changed");
            require(cursor.getLong(cursor.getColumnIndexOrThrow("_size")) == SOURCE.getBytes(StandardCharsets.UTF_8).length,
                    "incorrect byte size");
        }
        String[] columns = {"_size", "_display_name", "unknown"};
        try (Cursor cursor = resolver.query(source, columns, (Bundle)null, null)) {
            require(cursor != null && cursor.moveToFirst() && cursor.getColumnCount() == 3, "projection changed");
            require(cursor.getLong(0) == SOURCE.getBytes(StandardCharsets.UTF_8).length && cursor.isNull(2),
                    "projection values changed");
        }
        CancellationSignal cancelled = new CancellationSignal();
        cancelled.cancel();
        try {
            resolver.query(source, null, (Bundle)null, cancelled);
            throw new IllegalStateException("cancelled query ran");
        } catch (OperationCanceledException expected) {}
        String[] oversized = new String[33];
        java.util.Arrays.fill(oversized, "_size");
        try {
            resolver.query(source, oversized, null, null, null);
            throw new IllegalStateException("unbounded projection accepted");
        } catch (IllegalArgumentException expected) {}
        require(resolver.query(Uri.parse(source.toString() + "%00ignored"), null, null, null, null) == null,
                "embedded NUL path accepted");
        record("metadata=" + (expectedName().startsWith("Shared-") ? "exact-record-name" : "unicode-name")
                + ",byte-size,projection,cancellation,bounds");
    }

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        status = new TextView(this);
        status.setContentDescription("Android document resource status");
        status.setText("Opening an approved document");
        setContentView(status);
        if (new File(getFilesDir(), "documents-revoke-test.txt").exists()) {
            revoked();
            return;
        }
        startActivityForResult(new Intent(Intent.ACTION_OPEN_DOCUMENT).setType("text/plain"), 41);
    }

    @Override protected void onActivityResult(int code, int result, Intent data) {
        super.onActivityResult(code, result, data);
        if (result != RESULT_OK || data == null || data.getData() == null) {
            record("failure=picker-refused");
            return;
        }
        try {
            if (code == 41) {
                source = data.getData();
                require(SOURCE.equals(read(source)), "source read changed");
                try { metadata(); }
                catch (Throwable failure) {
                    failures++;
                    record("metadata-failure=" + failure.getClass().getName());
                }
                record("source-read=confirmed");
                Intent save = new Intent(Intent.ACTION_CREATE_DOCUMENT).setType("text/plain");
                save.putExtra(Intent.EXTRA_TITLE, "android-modes.txt");
                startActivityForResult(save, 42);
            } else if (code == 42) {
                Uri destination = data.getData();
                try {
                    write(destination, "wt", "one"); require("one".equals(read(destination)), "truncate failed");
                    write(destination, "wa", "-two"); require("one-two".equals(read(destination)), "append failed");
                    write(destination, "rw", "ONE"); require("ONE-two".equals(read(destination)), "read-write truncated");
                    write(destination, "rwt", "final\n"); require("final\n".equals(read(destination)), "read-write truncate failed");
                    try {
                        write(destination, "invalid", "bad");
                        throw new IllegalStateException("invalid mode accepted");
                    } catch (IllegalArgumentException expected) {}
                    require("final\n".equals(read(destination)), "invalid mode changed output");
                    record("write-modes=wt,wa,rw,rwt;invalid-denied");
                    for (String mode : new String[] {"wt", "wa", "rw", "rwt"}) {
                        try {
                            write(source, mode, "bad");
                            throw new IllegalStateException("read-only write accepted");
                        } catch (java.io.FileNotFoundException expected) {}
                    }
                    require(SOURCE.equals(read(source)), "source bytes changed");
                    Uri sibling = Uri.fromFile(new File(new File(source.getPath()).getParentFile(), "private-canary"));
                    require(getContentResolver().query(sibling, null, null, null, null) == null,
                            "ungranted sibling metadata disclosed");
                    try {
                        read(sibling);
                        throw new IllegalStateException("sibling read accepted");
                    } catch (java.io.FileNotFoundException expected) {}
                    record("isolation=read-only-modes,sibling-read,sibling-metadata-denied");
                    try (FileOutputStream out = new FileOutputStream(new File(getFilesDir(), "document-uris.txt"))) {
                        out.write((source.toString() + "\n" + destination.toString() + "\n").getBytes(StandardCharsets.UTF_8));
                    }
                } catch (Throwable failure) {
                    failures++;
                    record("write-failure=" + failure.getClass().getName());
                }
                status.setText(failures == 0 ? "Document metadata and modes confirmed" : "Document APIs incompatible");
                record(failures == 0 ? "complete=true" : "complete=false");
            }
        } catch (Throwable failure) {
            record("failure=" + failure.getClass().getName());
        }
    }

    private void revoked() {
        try {
            byte[] bytes = new byte[4096];
            int count;
            try (FileInputStream in = new FileInputStream(new File(getFilesDir(), "document-uris.txt"))) {
                count = in.read(bytes);
                require(count > 0 && in.read() == -1, "invalid saved fixture URIs");
            }
            String[] uris = new String(bytes, 0, count, StandardCharsets.UTF_8).split("\n");
            require(uris.length == 2, "missing saved fixture URIs");
            for (String value : uris) {
                Uri uri = Uri.parse(value);
                require(getContentResolver().query(uri, null, null, null, null) == null,
                        "revoked metadata visible");
                try { read(uri); throw new IllegalStateException("revoked read accepted"); }
                catch (java.io.FileNotFoundException expected) {}
                try { write(uri, "wt", "bad"); throw new IllegalStateException("revoked write accepted"); }
                catch (java.io.FileNotFoundException expected) {}
            }
            status.setText("Revoked documents denied");
            record("revocation=both-metadata,reads,writes-denied");
            record("revocation-complete=true");
        } catch (Throwable failure) { record("failure=" + failure.getClass().getName()); }
    }

    @Override protected void onPause() { record("lifecycle=pause"); super.onPause(); }
    @Override protected void onStop() { record("lifecycle=stop"); super.onStop(); }
    @Override protected void onDestroy() { record("lifecycle=destroy"); super.onDestroy(); }
}
