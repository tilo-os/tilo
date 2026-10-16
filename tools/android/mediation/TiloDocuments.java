// SPDX-License-Identifier: MIT
package android.app;

import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;

/** Metadata for existing file grants; this never creates document authority. */
public final class TiloDocuments {
    private TiloDocuments() {}
    private static native Object[] queryFileNative(String path, String[] columns);

    public static Cursor queryFile(Uri uri, String[] projection) {
        String path = uri.getPath();
        if (path == null || !path.startsWith("/"))
            throw new IllegalArgumentException("file URI requires an absolute path");
        String[] columns = projection == null
            ? new String[] {"_display_name", "_size", "mime_type"} : projection.clone();
        if (columns.length > 32) throw new IllegalArgumentException("too many metadata columns");
        for (String column : columns)
            if (column == null || column.length() > 128)
                throw new IllegalArgumentException("invalid metadata column");
        TiloChooser.loadBridge();
        Object[] row = queryFileNative(path, columns);
        if (row == null) return null;
        MatrixCursor cursor = new MatrixCursor(columns);
        cursor.addRow(row);
        return cursor;
    }
}
