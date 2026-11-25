package so.tilo.android;

import android.content.ContentResolver;
import android.content.Context;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.os.CancellationSignal;
import android.os.ParcelFileDescriptor;
import android.provider.DocumentsContract;
import android.provider.DocumentsContract.Document;
import android.provider.DocumentsContract.Root;
import android.provider.DocumentsProvider;
import android.util.Log;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileNotFoundException;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.HashMap;
import java.util.HashSet;
import java.util.Map;
import java.util.Set;
import org.json.JSONException;
import org.json.JSONObject;

/**
 * The person's Tilo workspaces as roots of the system file picker: the Files app and every other
 * app's file chooser browse the workspace's folders and open, add, rename and delete its files,
 * as they do any provider's.
 *
 * <p>A root is one of the person's workspaces and a document is one path in it, named by
 * {@code <workspace id>:<path>}. The bytes and the tree are the device daemon's, run in this
 * process by {@code tilo-phone}: a file opened for reading is read from the workspace, and one
 * opened for writing is written back to it when the caller closes it, so it syncs to the person's
 * other devices. A workspace change made elsewhere refreshes the folder being looked at.
 *
 * <p>Rust answers the methods here: {@code Java_so_tilo_android_TiloWorkspaceFiles_<method>}
 * in the app's library.
 */
public final class TiloWorkspaceFiles extends DocumentsProvider {
    private static final String TAG = "TiloDocuments";

    private static final String[] DEFAULT_ROOT_PROJECTION =
            new String[] {
                Root.COLUMN_ROOT_ID,
                Root.COLUMN_DOCUMENT_ID,
                Root.COLUMN_TITLE,
                Root.COLUMN_FLAGS,
                Root.COLUMN_MIME_TYPES,
            };

    private static final String[] DEFAULT_DOCUMENT_PROJECTION =
            new String[] {
                Document.COLUMN_DOCUMENT_ID,
                Document.COLUMN_DISPLAY_NAME,
                Document.COLUMN_MIME_TYPE,
                Document.COLUMN_SIZE,
                Document.COLUMN_LAST_MODIFIED,
                Document.COLUMN_FLAGS,
            };

    /** How often the provider looks whether a workspace's files moved. */
    private static final long WATCH_EVERY_MS = 3000;

    /** How long after the last look at a folder the watch stops asking: nobody is showing one. */
    private static final long WATCHING_FOR_MS = 60_000;

    private String authority;
    private volatile long lastLook;
    private final Set<String> served = new HashSet<>();
    private final Map<String, String> revisions = new HashMap<>();

    @Override
    public boolean onCreate() {
        Context context = getContext();
        if (context == null) {
            return false;
        }
        try {
            System.loadLibrary(TiloActivity.libraryName(context));
            if (!nativeStart(context)) {
                Log.w(TAG, "the phone's device did not start, so no workspace is offered");
                return false;
            }
        } catch (UnsatisfiedLinkError e) {
            // An APK whose library does not keep workspace files carries no such provider in its
            // manifest; one that does and failed to is a build to look at, and offers nothing.
            Log.w(TAG, "this app's library does not serve workspace files", e);
            return false;
        }
        authority = context.getPackageName() + ".tilo.workspace";
        watch();
        return true;
    }

    @Override
    public Cursor queryRoots(String[] projection) {
        lastLook = System.currentTimeMillis();
        String[] columns = projection != null ? projection : DEFAULT_ROOT_PROJECTION;
        MatrixCursor cursor = new MatrixCursor(columns);
        String[] roots = nativeRoots();
        if (roots == null) {
            return cursor;
        }
        for (String row : roots) {
            try {
                JSONObject root = new JSONObject(row);
                MatrixCursor.RowBuilder builder = cursor.newRow();
                for (String column : columns) {
                    if (Root.COLUMN_ROOT_ID.equals(column) || Root.COLUMN_DOCUMENT_ID.equals(column)) {
                        builder.add(root.getString("id"));
                    } else if (Root.COLUMN_TITLE.equals(column)) {
                        builder.add(root.getString("name"));
                    } else if (Root.COLUMN_FLAGS.equals(column)) {
                        builder.add(Root.FLAG_SUPPORTS_CREATE | Root.FLAG_SUPPORTS_IS_CHILD);
                    } else if (Root.COLUMN_MIME_TYPES.equals(column)) {
                        builder.add("*/*");
                    } else {
                        builder.add(null);
                    }
                }
            } catch (JSONException e) {
                Log.w(TAG, "a workspace root did not read", e);
            }
        }
        return cursor;
    }

    @Override
    public Cursor queryDocument(String documentId, String[] projection) throws FileNotFoundException {
        lastLook = System.currentTimeMillis();
        String[] columns = projection != null ? projection : DEFAULT_DOCUMENT_PROJECTION;
        MatrixCursor cursor = new MatrixCursor(columns, 1);
        JSONObject document = read(nativeDocument(documentId));
        if (document == null) {
            throw new FileNotFoundException("no document " + documentId);
        }
        addDocument(cursor, columns, document);
        return cursor;
    }

    @Override
    public Cursor queryChildDocuments(String parentDocumentId, String[] projection, String sortOrder)
            throws FileNotFoundException {
        lastLook = System.currentTimeMillis();
        String[] columns = projection != null ? projection : DEFAULT_DOCUMENT_PROJECTION;
        MatrixCursor cursor = new MatrixCursor(columns);
        synchronized (served) {
            served.add(parentDocumentId);
        }
        String[] children;
        try {
            children = nativeChildren(parentDocumentId);
        } catch (IOException e) {
            throw notFound(e);
        }
        if (children == null) {
            return cursor;
        }
        for (String row : children) {
            JSONObject child = read(row);
            if (child != null) {
                addDocument(cursor, columns, child);
            }
        }
        return cursor;
    }

    @Override
    public Cursor querySearchDocuments(String rootId, String query, String[] projection) {
        // Search across a workspace is the system's, over what the workspace indexes; a provider
        // that offered its own would duplicate it.
        String[] columns = projection != null ? projection : DEFAULT_DOCUMENT_PROJECTION;
        return new MatrixCursor(columns);
    }

    @Override
    public boolean isChildDocument(String parentDocumentId, String documentId) {
        return documentId.equals(parentDocumentId)
                || documentId.startsWith(parentDocumentId + ":")
                || documentId.startsWith(parentDocumentId + "/");
    }

    @Override
    public ParcelFileDescriptor openDocument(
            String documentId, String mode, CancellationSignal signal) throws FileNotFoundException {
        if ("r".equals(mode)) {
            return openForRead(documentId);
        }
        if (mode.startsWith("w")) {
            return openForWrite(documentId);
        }
        throw new FileNotFoundException("Tilo documents open for reading or writing, not " + mode);
    }

    @Override
    public String createDocument(String parentDocumentId, String mimeType, String displayName)
            throws FileNotFoundException {
        try {
            String documentId =
                    nativeCreate(
                            parentDocumentId,
                            displayName,
                            Document.MIME_TYPE_DIR.equals(mimeType));
            changed(documentId);
            return DocumentsContract.buildDocumentUri(authority, documentId).toString();
        } catch (IOException e) {
            throw notFound(e);
        }
    }

    @Override
    public void deleteDocument(String documentId) throws FileNotFoundException {
        try {
            nativeDelete(documentId);
            changed(documentId);
        } catch (IOException e) {
            throw notFound(e);
        }
    }

    @Override
    public String renameDocument(String documentId, String displayName) throws FileNotFoundException {
        try {
            String renamed = nativeRename(documentId, displayName);
            changed(documentId);
            changed(renamed);
            return DocumentsContract.buildDocumentUri(authority, renamed).toString();
        } catch (IOException e) {
            throw notFound(e);
        }
    }

    private ParcelFileDescriptor openForRead(String documentId) throws FileNotFoundException {
        byte[] bytes;
        try {
            bytes = nativeRead(documentId);
        } catch (IOException e) {
            throw notFound(e);
        }
        if (bytes == null) {
            throw new FileNotFoundException("no bytes read for " + documentId);
        }
        Context context = getContext();
        if (context == null) {
            throw new FileNotFoundException("the provider has no context");
        }
        File cached;
        try {
            cached = File.createTempFile("tilo-doc-", "", context.getCacheDir());
        } catch (IOException e) {
            throw notFound(e);
        }
        try (OutputStream out = new FileOutputStream(cached)) {
            out.write(bytes);
        } catch (IOException e) {
            throw notFound(e);
        }
        try {
            ParcelFileDescriptor descriptor =
                    ParcelFileDescriptor.open(cached, ParcelFileDescriptor.MODE_READ_ONLY);
            // Unlinked while it is open: the reader keeps its bytes and the cache keeps nothing.
            if (!cached.delete()) {
                Log.w(TAG, "a document's read copy stays in the cache");
            }
            return descriptor;
        } catch (IOException e) {
            throw notFound(e);
        }
    }

    /**
     * The caller writes into a pipe and, as it closes its end, the bytes go back to the workspace:
     * a file it saved is in the workspace by the time its close returned.
     */
    private ParcelFileDescriptor openForWrite(String documentId) throws FileNotFoundException {
        ParcelFileDescriptor[] pipe;
        try {
            pipe = ParcelFileDescriptor.createPipe();
        } catch (IOException e) {
            throw notFound(e);
        }
        ParcelFileDescriptor source = pipe[0];
        ParcelFileDescriptor target = pipe[1];
        Thread writer =
                new Thread(
                        () -> {
                            ByteArrayOutputStream bytes = new ByteArrayOutputStream();
                            try (InputStream in = new ParcelFileDescriptor.AutoCloseInputStream(source)) {
                                byte[] buffer = new byte[64 * 1024];
                                int read;
                                while ((read = in.read(buffer)) != -1) {
                                    bytes.write(buffer, 0, read);
                                }
                                nativeWrite(documentId, bytes.toByteArray());
                                changed(documentId);
                            } catch (IOException e) {
                                Log.w(TAG, "a document's bytes did not reach the workspace", e);
                            }
                        },
                        "tilo-document-write");
        writer.start();
        return target;
    }

    /** Look whether each workspace's files moved, and wake the folders that showed them. */
    private void watch() {
        Thread watcher =
                new Thread(
                        () -> {
                            while (!Thread.currentThread().isInterrupted()) {
                                try {
                                    Thread.sleep(WATCH_EVERY_MS);
                                } catch (InterruptedException e) {
                                    return;
                                }
                                if (System.currentTimeMillis() - lastLook > WATCHING_FOR_MS) {
                                    // Nothing has shown a folder for a minute: nothing to refresh.
                                    continue;
                                }
                                String[] roots = nativeRoots();
                                if (roots == null) {
                                    continue;
                                }
                                for (String row : roots) {
                                    String workspace = workspaceOf(row);
                                    if (workspace.isEmpty()) {
                                        continue;
                                    }
                                    String revision = nativeRevision(workspace);
                                    if (revision == null || revision.isEmpty()) {
                                        continue;
                                    }
                                    String last;
                                    synchronized (revisions) {
                                        last = revisions.put(workspace, revision);
                                    }
                                    if (last != null && !last.equals(revision)) {
                                        changed(workspace);
                                    }
                                }
                            }
                        },
                        "tilo-documents-watch");
        watcher.setDaemon(true);
        watcher.start();
    }

    private static String workspaceOf(String rootRow) {
        try {
            return new JSONObject(rootRow).optString("id", "");
        } catch (JSONException e) {
            return "";
        }
    }

    /** Tell whoever shows the workspace or a document of it that it changed. */
    private void changed(String documentId) {
        Context context = getContext();
        if (context == null || documentId.isEmpty()) {
            return;
        }
        ContentResolver resolver = context.getContentResolver();
        String workspace = documentId;
        int separator = documentId.indexOf(':');
        if (separator >= 0) {
            workspace = documentId.substring(0, separator);
        }
        resolver.notifyChange(DocumentsContract.buildDocumentUri(authority, documentId), null);
        resolver.notifyChange(DocumentsContract.buildChildDocumentsUri(authority, documentId), null);
        if (!workspace.equals(documentId)) {
            resolver.notifyChange(DocumentsContract.buildChildDocumentsUri(authority, workspace), null);
        }
        Set<String> shown;
        synchronized (served) {
            shown = new HashSet<>(served);
        }
        for (String id : shown) {
            if (id.startsWith(workspace + ":") || id.equals(workspace)) {
                resolver.notifyChange(DocumentsContract.buildChildDocumentsUri(authority, id), null);
            }
        }
    }

    private static void addDocument(MatrixCursor cursor, String[] columns, JSONObject document) {
        MatrixCursor.RowBuilder builder = cursor.newRow();
        for (String column : columns) {
            if (Document.COLUMN_DOCUMENT_ID.equals(column)) {
                builder.add(document.optString("id", ""));
            } else if (Document.COLUMN_DISPLAY_NAME.equals(column)) {
                builder.add(document.optString("name", ""));
            } else if (Document.COLUMN_MIME_TYPE.equals(column)) {
                builder.add(document.optString("mime", "*/*"));
            } else if (Document.COLUMN_SIZE.equals(column)) {
                builder.add(value(document, "size"));
            } else if (Document.COLUMN_LAST_MODIFIED.equals(column)) {
                builder.add(value(document, "modified"));
            } else if (Document.COLUMN_FLAGS.equals(column)) {
                builder.add(flags(document));
            } else {
                builder.add(null);
            }
        }
    }

    private static Object value(JSONObject document, String key) {
        long value = document.optLong(key, 0);
        return value == 0 ? null : value;
    }

    private static int flags(JSONObject document) {
        if (document.optBoolean("directory", false)) {
            return Document.FLAG_DIR_SUPPORTS_CREATE;
        }
        return Document.FLAG_SUPPORTS_WRITE
                | Document.FLAG_SUPPORTS_DELETE
                | Document.FLAG_SUPPORTS_RENAME;
    }

    private static JSONObject read(String document) {
        if (document == null) {
            return null;
        }
        try {
            return new JSONObject(document);
        } catch (JSONException e) {
            Log.w(TAG, "a document did not read", e);
            return null;
        }
    }

    private static FileNotFoundException notFound(IOException cause) {
        FileNotFoundException missing = new FileNotFoundException(cause.getMessage());
        missing.initCause(cause);
        return missing;
    }

    /** The app's library, named as its activity's manifest names it. */
    private static native boolean nativeStart(Context context);

    private static native String[] nativeRoots();

    private static native String nativeDocument(String documentId);

    private static native String[] nativeChildren(String documentId) throws IOException;

    private static native byte[] nativeRead(String documentId) throws IOException;

    private static native void nativeWrite(String documentId, byte[] bytes) throws IOException;

    private static native String nativeCreate(String parentDocumentId, String name, boolean directory)
            throws IOException;

    private static native void nativeDelete(String documentId) throws IOException;

    private static native String nativeRename(String documentId, String name) throws IOException;

    private static native String nativeRevision(String workspaceId);
}
