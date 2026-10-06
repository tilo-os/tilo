#!/usr/bin/env python3
"""Apply a bounded integration to the exact qualified ATL source tree.

Upstream sources remain GPL-3.0-only. This script never fetches or builds.
"""
import argparse
import hashlib
from pathlib import Path
import shutil

PINS = {'src/api-impl/android/app/Activity.java': 'b696fd26674c5549fe9956511fde168840ae514d8e9f71525966a96bc21731e2',
 'src/api-impl/android/app/NotificationManager.java': 'ed4939335389b7efbf9e0c405d92db43a62a642ede049092e1cd983d3e29d9aa',
 'src/api-impl/android/content/pm/PackageManager.java': '0ce023e27cb636d9a3944ad83607b2bdc4f474385dcb2eab0c0b05db8465c583'}


def patch(root):
    for name, expected in PINS.items():
        if hashlib.sha256((root / name).read_bytes()).hexdigest() != expected:
            raise ValueError('not the qualified, unmodified ATL source: ' + name)
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
                                            'public boolean areNotificationsEnabled() {\n\t\treturn false;')
    text = text.replace('public void notify(String tag, int id, Notification notification) {',
                        'public void notify(String tag, int id, Notification notification) {\n\t\tif (!areNotificationsEnabled()) return;')
    text = text.replace('public void cancel(String tag, final int id) {',
                        'public void cancel(String tag, final int id) {\n\t\tif (!areNotificationsEnabled()) return;')
    notification.write_text(text)
    shutil.copyfile(Path(__file__).with_name('TiloChooser.java'), root / 'src/api-impl/android/app/TiloChooser.java')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    patch(parser.parse_args().source)
