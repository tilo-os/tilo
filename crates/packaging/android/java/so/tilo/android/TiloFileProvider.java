package so.tilo.android;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.content.Context;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;
import java.io.File;
import java.io.FileNotFoundException;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.UUID;

/**
 * Serves files the app hands to another app ("open with"), read-only.
 *
 * <p>Each shared file is a descriptor the native side already opened and
 * checked; the URI names it by an unguessable token, so the provider can
 * serve nothing else. The provider is not exported: the receiving app reads
 * only through the per-URI grant on the launching intent.
 */
public final class TiloFileProvider extends ContentProvider {
    private static final class Shared {
        final int fd;
        final String name;
        final String mimeType;

        Shared(int fd, String name, String mimeType) {
            this.fd = fd;
            this.name = name;
            this.mimeType = mimeType;
        }
    }

    private static final Map<String, Shared> SHARED = new HashMap<>();

    public static String authority(Context context) {
        return context.getPackageName() + ".tilo.files";
    }

    /** Share the open descriptor {@code fd}, which the provider keeps for the process lifetime. */
    public static Uri share(Context context, int fd, String name, String mimeType) {
        String token = UUID.randomUUID().toString();
        synchronized (SHARED) {
            SHARED.put(token, new Shared(fd, name, mimeType));
        }
        return new Uri.Builder()
                .scheme("content")
                .authority(authority(context))
                .appendPath(token)
                .appendPath(name)
                .build();
    }

    static void unshare(Uri uri) {
        List<String> segments = uri.getPathSegments();
        if (!segments.isEmpty()) {
            synchronized (SHARED) {
                SHARED.remove(segments.get(0));
            }
        }
    }

    private static Shared lookup(Uri uri) throws FileNotFoundException {
        List<String> segments = uri.getPathSegments();
        Shared shared = null;
        if (segments.size() == 2) {
            synchronized (SHARED) {
                shared = SHARED.get(segments.get(0));
            }
        }
        if (shared == null) {
            throw new FileNotFoundException("not a shared file: " + uri);
        }
        return shared;
    }

    private static File file(Shared shared) {
        return new File("/proc/self/fd/" + shared.fd);
    }

    @Override
    public boolean onCreate() {
        return true;
    }

    @Override
    public String getType(Uri uri) {
        try {
            return lookup(uri).mimeType;
        } catch (FileNotFoundException e) {
            return null;
        }
    }

    @Override
    public ParcelFileDescriptor openFile(Uri uri, String mode) throws FileNotFoundException {
        if (!"r".equals(mode)) {
            throw new FileNotFoundException("shared files are read-only");
        }
        return ParcelFileDescriptor.open(file(lookup(uri)), ParcelFileDescriptor.MODE_READ_ONLY);
    }

    @Override
    public Cursor query(Uri uri, String[] projection, String selection, String[] selectionArgs, String sortOrder) {
        Shared shared;
        try {
            shared = lookup(uri);
        } catch (FileNotFoundException e) {
            return null;
        }
        String[] columns = projection != null
                ? projection
                : new String[] {OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE};
        MatrixCursor cursor = new MatrixCursor(columns, 1);
        Object[] row = new Object[columns.length];
        for (int i = 0; i < columns.length; i++) {
            if (OpenableColumns.DISPLAY_NAME.equals(columns[i])) {
                row[i] = shared.name;
            } else if (OpenableColumns.SIZE.equals(columns[i])) {
                row[i] = file(shared).length();
            }
        }
        cursor.addRow(row);
        return cursor;
    }

    @Override
    public Uri insert(Uri uri, ContentValues values) {
        throw new UnsupportedOperationException("shared files are read-only");
    }

    @Override
    public int delete(Uri uri, String selection, String[] selectionArgs) {
        throw new UnsupportedOperationException("shared files are read-only");
    }

    @Override
    public int update(Uri uri, ContentValues values, String selection, String[] selectionArgs) {
        throw new UnsupportedOperationException("shared files are read-only");
    }
}
