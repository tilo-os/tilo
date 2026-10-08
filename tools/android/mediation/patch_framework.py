#!/usr/bin/env python3
"""Apply a bounded integration to the exact qualified ATL source tree.

Upstream sources remain GPL-3.0-only. This script never fetches or builds.
"""
import argparse
import hashlib
from pathlib import Path
import shutil

PINS = {'src/api-impl/android/app/Service.java': '0f2601d83841e1e8f9a838742dcf87ec7fe85246cd9ab7adf19d035fd621724c',
 'src/api-impl/android/atl/ATLLoadedApp.java': 'c8b03faceddb16506780278e0da34f6da069bb995c7ff9e90ccbc99406bebf87',
 'src/api-impl/android/app/Activity.java': 'b696fd26674c5549fe9956511fde168840ae514d8e9f71525966a96bc21731e2',
 'src/api-impl/android/content/ContentResolver.java': 'bb5ee7805ee956a980c48755895fd0ddb66735d245ffbcae1e85f075e4075238',
 'src/api-impl/android/app/NotificationManager.java': 'ed4939335389b7efbf9e0c405d92db43a62a642ede049092e1cd983d3e29d9aa',
 'src/api-impl/android/content/pm/PackageManager.java': '0ce023e27cb636d9a3944ad83607b2bdc4f474385dcb2eab0c0b05db8465c583',
 'src/api-impl/android/view/View.java': '2efcb76f87b12ed72c3ac7b25971487efe3fea849a2f9ff141238cce62f4f25d',
 'src/api-impl/android/widget/TextView.java': 'd651d6894113c0ffedd1232b8a7c21f8e5e855448a2e89136c47beff0c6d20cf'}


def patch_services(root):
    service = root / 'src/api-impl/android/app/Service.java'
    text = service.read_text().replace("private int notification_id;", "private volatile int notification_id;")
    begin = text.index('\tpublic void startForeground(int id, Notification notification) {')
    end = text.index('\n\tpublic Application getApplication()', begin)
    text = text[:begin] + '''    public void startForeground(int id, Notification notification) {
        if (id == 0 || notification == null)
            throw new IllegalArgumentException("foreground service requires a nonzero id and notification");
        if (getApplicationInfo().targetSdkVersion >= 28 &&
                checkSelfPermission("android.permission.FOREGROUND_SERVICE") !=
                android.content.pm.PackageManager.PERMISSION_GRANTED)
            throw new SecurityException("foreground service permission is unavailable");
        NotificationManager manager = new NotificationManager();
        if (!manager.areNotificationsEnabled())
            throw new SecurityException("foreground service requires the filtered host notification provider");
        if (notification.style instanceof Notification.MediaStyle)
            throw new UnsupportedOperationException("foreground media service notification is not qualified");
        notification.ongoing = true;
        manager.notify(id, notification);
        if (notification_id != 0 && notification_id != id) manager.cancel(notification_id);
        notification_id = id;
        get_atl_loaded_app().tiloPublishRecovery();
    }

    public void stopForeground(boolean remove) {
        if (remove && notification_id != 0) {
            new NotificationManager().cancel(notification_id);
            notification_id = 0;
            get_atl_loaded_app().tiloPublishRecovery();
        }
    }

    public void stopForeground(int flags) {
        if (flags != 0 && flags != 1)
            throw new UnsupportedOperationException("foreground notification detach is not qualified");
        stopForeground(flags == 1);
    }

    public void tiloDestroyForeground() { stopForeground(true); }
    public boolean tiloHasForeground() { return notification_id != 0; }
''' + text[end:]
    text = text.replace('System.out.println("Service.stopSelf(" + startId + ") called");',
        'this.get_atl_loaded_app().tiloStopSelf(this, startId);')
    text = text.replace('System.out.println("Service.stopSelf() called");',
        'this.get_atl_loaded_app().tiloStopSelf(this, -1);')
    text = text.replace('''System.out.println("Service.stopSelfResult(" + startId + ") called");
\t\treturn true;''', 'return this.get_atl_loaded_app().tiloStopSelf(this, startId);')
    service.write_text(text)
    loaded = root / 'src/api-impl/android/atl/ATLLoadedApp.java'
    text = loaded.read_text()
    text = text.replace('private final HashMap<String, Service> running_services;', '''private final HashMap<String, Service> running_services;
    private final HashMap<String, Integer> tiloServiceStarts = new HashMap<>();
    private final HashMap<String, Integer> tiloPendingServiceStarts = new HashMap<>();
    private final HashMap<String, Integer> tiloCancelledServiceStarts = new HashMap<>();
    private final HashSet<Service> tiloStartedServices = new HashSet<>();
    private final HashMap<Service, Integer> tiloRestartModes = new HashMap<>();
    private final HashSet<String> tiloRecoveringServices = new HashSet<>();''')
    text = text.replace('final String className = component.getClassName();', '''final String className = component.getClassName();
        final int startId;
        synchronized (running_services) {
            int previous = tiloServiceStarts.containsKey(className) ? tiloServiceStarts.get(className) : 0;
            // Reserve before posting: an older completion must not stop a queued newer start.
            if (serviceConnection == null) {
                if (previous == Integer.MAX_VALUE)
                    throw new IllegalStateException("service start identifiers exhausted");
                startId = previous + 1;
                tiloServiceStarts.put(className, startId);
                tiloPendingServiceStarts.put(className, startId);
            } else startId = 0;
        }''')
    text = text.replace('Service service = running_services.get(className);', '''Service service;
                    synchronized (running_services) {
                        if (tiloStartCancelled(className, startId)) return;
                        service = running_services.get(className);
                    }''', 1)
    text = text.replace('''\t\t\t\t\t\tservice.onCreate();
\t\t\t\t\t\trunning_services.put(className, service);''', '''                        synchronized (running_services) {
                            if (tiloStartCancelled(className, startId)) return;
                            running_services.put(className, service);
                            if (serviceConnection == null) tiloStartedServices.add(service);
                        }
                        service.onCreate();''')
    text = text.replace('service.onStartCommand(intent, 0, 0);', '''synchronized (running_services) {
                            if (running_services.get(className) != service ||
                                    tiloStartCancelled(className, startId)) return;
                            tiloStartedServices.add(service);
                        }
                        int restartMode = service.onStartCommand(tiloRecoveringServices.remove(className) ? null : intent, 0, startId);
                        synchronized (running_services) {
                            if (running_services.get(className) == service)
                                tiloRestartModes.put(service, restartMode);
                        }
                        tiloPublishRecovery();
                        if (restartMode == 3)
                            android.util.Log.w("TiloService", "intent redelivery recovery is not qualified");''')
    text = text.replace('''\t\t\t\t\tSlog.e(TAG, "startService: failed to start service " + className, e);
\t\t\t\t}''', '''                    Slog.e(TAG, "startService: failed to start service " + className, e);
                } finally {
                    synchronized (running_services) {
                        Integer pending = tiloPendingServiceStarts.get(className);
                        if (serviceConnection == null && pending != null && pending.intValue() == startId)
                            tiloPendingServiceStarts.remove(className);
                    }
                }''', 1)
    begin = text.index('\tpublic boolean stopService(Intent intent) {')
    end = text.index('\n\tpublic void receiveBroadcast(', begin)
    text = text[:begin] + '''    public synchronized void tiloPublishRecovery() {
        StringBuilder entries = new StringBuilder();
        synchronized (running_services) {
            for (Service service : running_services.values()) {
                Integer mode = tiloRestartModes.get(service);
                if (mode != null && mode.intValue() == 1 && service.tiloHasForeground())
                    entries.append(service.getClass().getName()).append('\\n');
            }
        }
        android.app.TiloChooser.recoveryJournal(entries.toString());
    }

    public void tiloRecoverForegroundServices() {
        String entries = System.getenv("TILO_ANDROID_RECOVER_SERVICES");
        if (entries == null || entries.length() > 4095)
            throw new SecurityException("no launch-local recovery record");
        String[] names = entries.split("\\n");
        if (names.length == 0 || names.length > 16)
            throw new SecurityException("invalid recovery service count");
        for (String name : names) {
            boolean declared = false;
            for (PackageParser.Service service : pkg.services)
                if (service.info.enabled && name.equals(service.className)) declared = true;
            if (!declared || !name.matches("[A-Za-z_$][A-Za-z0-9_.$]{0,254}"))
                throw new SecurityException("recovery service is not declared by this APK");
            tiloRecoveringServices.add(name);
            startOrBindService(new Intent().setComponent(new ComponentName(pkg.packageName, name)), null);
        }
    }

    public boolean stopService(Intent intent) {
        if (intent.getComponent() == null) return false;
        Service service;
        boolean pending;
        synchronized (running_services) {
            String name = intent.getComponent().getClassName();
            pending = tiloCancelPendingStarts(name);
            service = running_services.get(name);
        }
        boolean stopped = service != null && tiloStopService(service, -1, false);
        return pending || stopped;
    }

    // Call only while holding running_services. Cancellation fences queued callbacks;
    // later starts have increasing identifiers and remain admissible.
    private boolean tiloStartCancelled(String name, int startId) {
        Integer cancelled = tiloCancelledServiceStarts.get(name);
        return startId != 0 && cancelled != null && startId <= cancelled.intValue();
    }

    private boolean tiloCancelPendingStarts(String name) {
        Integer pending = tiloPendingServiceStarts.remove(name);
        if (pending == null) return false;
        tiloCancelledServiceStarts.put(name, pending);
        tiloRecoveringServices.remove(name);
        return true;
    }

    public boolean tiloStopSelf(Service service, int startId) {
        return tiloStopService(service, startId, true);
    }

    private boolean tiloStopService(final Service service, int startId, boolean requireStarted) {
        String name = service.getClass().getName();
        synchronized (running_services) {
            if (running_services.get(name) != service) return false;
            Integer latest = tiloServiceStarts.get(name);
            if (requireStarted && !tiloStartedServices.contains(service)) return false;
            if (startId != -1 && (latest == null || latest.intValue() != startId)) return false;
            tiloCancelPendingStarts(name);
            running_services.remove(name);
            tiloStartedServices.remove(service);
            tiloRestartModes.remove(service);
        }
        tiloPublishRecovery();
        // Application callbacks always run on the main looper, including worker self-stops.
        if (Looper.myLooper() == Looper.getMainLooper()) {
            service.tiloDestroyForeground();
            service.onDestroy();
        }
        else new Handler(Looper.getMainLooper()).post(new Runnable() {
            @Override public void run() { service.tiloDestroyForeground(); service.onDestroy(); }
        });
        return true;
    }
''' + text[end:]
    loaded.write_text(text)


def patch(root):
    for name, expected in PINS.items():
        if hashlib.sha256((root / name).read_bytes()).hexdigest() != expected:
            raise ValueError('not the qualified, unmodified ATL source: ' + name)
    patch_services(root)
    activity = root / 'src/api-impl/android/app/Activity.java'
    text = activity.read_text()
    text = text.replace('public native void nativeFileChooser(int action, String type, String title, int requestCode);',
                        'public void nativeFileChooser(int action, String type, String title, int requestCode) {\n'
                        '\t\tTiloChooser.choose(this, action, type, title, requestCode);\n\t}')
    old = '\t\tSlog.w(TAG, "requestPermissions(" + Arrays.toString(permissions) + "): not handled");'
    text = text.replace(old, '''        if (permissions == null) throw new IllegalArgumentException("permissions must not be null");
        final String[] requested = permissions.clone();
        final int code = requestCode;
        runOnUiThread(new Runnable() {
            @Override public void run() {
                int[] results = new int[requested.length];
                Arrays.fill(results, android.content.pm.PackageManager.PERMISSION_DENIED);
                onRequestPermissionsResult(code, requested, results);
            }
        });''')
    text = text.replace('\tpublic boolean shouldShowRequestPermissionRationale(String permission) {\n\t\treturn true;',
                        '\tpublic void onRequestPermissionsResult(int requestCode, String[] permissions, int[] results) {}\n\n'
                        '\tpublic boolean shouldShowRequestPermissionRationale(String permission) {\n\t\treturn false;')
    text = text.replace('Uri uri = uriString != null ? Uri.parse(uriString) : null;',
        'if (System.getenv("TILO_ANDROID_RECOVER_SERVICES") != null)\n'
        '\t\t\treturn internalCreateActivity("android.app.TiloRecoveryActivity", native_window, new Intent());\n'
        '\t\tUri uri = uriString != null ? Uri.parse(uriString) : null;')
    activity.write_text(text)
    manager = root / 'src/api-impl/android/content/pm/PackageManager.java'
    text = manager.read_text()
    start = text.index('\tpublic int checkPermission(String permName, String pkgName) {')
    end = text.index('\n\t/**', start)
    text = text[:start] + '''    public int checkPermission(String permName, String pkgName) {
        // No Android permission has a qualified Tilo capability mapping yet.
        // A picker selection never confers a blanket storage permission.
        return PERMISSION_DENIED;
    }
''' + text[end:]
    manager.write_text(text)
    notification = root / 'src/api-impl/android/app/NotificationManager.java'
    text = notification.read_text().replace('public boolean areNotificationsEnabled() {\n\t\treturn true;',
                                            'public boolean areNotificationsEnabled() {\n\t\treturn TiloChooser.notificationsAvailable();')
    text = text.replace('public void notify(String tag, int id, Notification notification) {',
                        'public void notify(String tag, int id, Notification notification) {\n\t\tif (!areNotificationsEnabled()) return;')
    text = text.replace('public void cancel(String tag, final int id) {',
                        'public void cancel(String tag, final int id) {\n\t\tif (!areNotificationsEnabled()) return;')
    text = text.replace('long builder = nativeInitBuilder();',
                        'long builder = nativeInitBuilder();\n\t\tif (notification.ongoing) TiloChooser.markOngoing(builder);')
    notification.write_text(text)
    view = root / 'src/api-impl/android/view/View.java'
    text = view.read_text().replace('public void setContentDescription(CharSequence contentDescription) {}',
        '''private String tiloContentDescription;
    public void setContentDescription(CharSequence contentDescription) {
        tiloContentDescription = contentDescription == null ? null : contentDescription.toString();
        android.app.TiloAccessibility.describe(widget, tiloContentDescription,
            this instanceof android.widget.TextView);
    }''')
    text = text.replace('public CharSequence getContentDescription() {\n\t\treturn null;',
                        'public CharSequence getContentDescription() {\n\t\treturn tiloContentDescription;')
    view.write_text(text)
    text_view = root / 'src/api-impl/android/widget/TextView.java'
    text = text_view.read_text().replace('native_setText(text != null ? text.toString() : null);',
        '''native_setText(text != null ? text.toString() : null);
        CharSequence description = getContentDescription();
        if (description != null)
            android.app.TiloAccessibility.describe(widget, description.toString(), true);''')
    text_view.write_text(text)
    resolver = root / 'src/api-impl/android/content/ContentResolver.java'
    text = resolver.read_text()
    old = '''			MatrixCursor cursor = new MatrixCursor(projection);
			Object[] row = new Object[projection.length];
			native_query_file_info(uri.getPath(), projection, row);
			cursor.addRow(row);
			return cursor;'''
    if text.count(old) != 2:
        raise ValueError('unexpected qualified file query implementation')
    text = text.replace(old, '\t\t\treturn android.app.TiloDocuments.queryFile(uri, projection);')
    text = text.replace('''	public OutputStream openOutputStream(Uri uri) throws FileNotFoundException {
		ParcelFileDescriptor fd = openFileDescriptor(uri, "w");''', '''	public OutputStream openOutputStream(Uri uri) throws FileNotFoundException {
		return openOutputStream(uri, "w");
	}

	public OutputStream openOutputStream(Uri uri, String mode) throws FileNotFoundException {
		ParcelFileDescriptor fd = openFileDescriptor(uri, mode);''')
    for signature in (
        'public Cursor query(Uri uri, String[] projection, Bundle queryArgs, CancellationSignal cancellationSignal) {',
        'public Cursor query(Uri uri, String[] projection, String selection, String[] selectionArgs, String sortOrder, CancellationSignal cancellationSignal) {',
    ):
        text = text.replace(signature, signature + '\n\t\tif (cancellationSignal != null) cancellationSignal.throwIfCanceled();')
    resolver.write_text(text)
    shutil.copyfile(Path(__file__).with_name('TiloChooser.java'), root / 'src/api-impl/android/app/TiloChooser.java')
    shutil.copyfile(Path(__file__).with_name('TiloAccessibility.java'), root / 'src/api-impl/android/app/TiloAccessibility.java')
    shutil.copyfile(Path(__file__).with_name('TiloDocuments.java'), root / 'src/api-impl/android/app/TiloDocuments.java')
    shutil.copyfile(Path(__file__).with_name('TiloRecoveryActivity.java'), root / 'src/api-impl/android/app/TiloRecoveryActivity.java')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    patch(parser.parse_args().source)
