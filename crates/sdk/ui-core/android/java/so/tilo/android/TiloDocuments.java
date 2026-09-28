package so.tilo.android;

import android.content.ActivityNotFoundException;
import android.content.ContentResolver;
import android.content.Context;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;
import android.webkit.MimeTypeMap;
import java.io.FileNotFoundException;

/** Storage Access Framework requests for the native file picker. */
public final class TiloDocuments {
    private TiloDocuments() {}

    public static Intent openIntent(String[] mimeTypes, boolean multiple) {
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        if (mimeTypes.length == 1) {
            intent.setType(mimeTypes[0]);
        } else {
            intent.setType("*/*");
            intent.putExtra(Intent.EXTRA_MIME_TYPES, mimeTypes);
        }
        intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, multiple);
        return intent;
    }

    public static Intent createIntent(String mimeType, String suggestedName) {
        Intent intent = new Intent(Intent.ACTION_CREATE_DOCUMENT);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        intent.setType(mimeType);
        intent.putExtra(Intent.EXTRA_TITLE, suggestedName);
        return intent;
    }

    /** The MIME type Android associates with {@code extension}, or null. */
    public static String mimeTypeForExtension(String extension) {
        return MimeTypeMap.getSingleton().getMimeTypeFromExtension(extension);
    }

    /**
     * Open the file behind {@code fd} with the app the user chose for its
     * type. On success the file provider keeps {@code fd}; when no app handles
     * the type the caller still owns it.
     */
    public static void view(Context context, int fd, String name) throws ActivityNotFoundException {
        String mimeType = null;
        int dot = name.lastIndexOf('.');
        if (dot >= 0) {
            mimeType = mimeTypeForExtension(name.substring(dot + 1).toLowerCase(java.util.Locale.ROOT));
        }
        if (mimeType == null) {
            mimeType = "application/octet-stream";
        }
        Uri uri = TiloFileProvider.share(context, fd, name, mimeType);
        Intent intent = new Intent(Intent.ACTION_VIEW);
        intent.setDataAndType(uri, mimeType);
        intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_ACTIVITY_NEW_TASK);
        try {
            context.startActivity(intent);
        } catch (ActivityNotFoundException e) {
            TiloFileProvider.unshare(uri);
            throw new ActivityNotFoundException("no app on this device opens " + mimeType + " files");
        }
    }

    /** The provider's display name for {@code uri}, or null when it reports none. */
    public static String displayName(Context context, String uri) {
        Cursor cursor = context.getContentResolver().query(
                Uri.parse(uri), new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null);
        if (cursor == null) {
            return null;
        }
        try {
            return cursor.moveToFirst() ? cursor.getString(0) : null;
        } finally {
            cursor.close();
        }
    }

    /** Open {@code uri} in {@code mode} and hand the descriptor's ownership to the caller. */
    public static int openFd(Context context, String uri, String mode) throws FileNotFoundException {
        ContentResolver resolver = context.getContentResolver();
        ParcelFileDescriptor descriptor = resolver.openFileDescriptor(Uri.parse(uri), mode);
        if (descriptor == null) {
            throw new FileNotFoundException("the provider returned no descriptor for " + uri);
        }
        return descriptor.detachFd();
    }
}
