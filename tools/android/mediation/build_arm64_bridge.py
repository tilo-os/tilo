#!/usr/bin/env python3
"""Cross-build only against signed ARM64 GTK/GLib inputs and existing JNI headers."""
import argparse
import os
from pathlib import Path
import shlex
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sysroot', type=Path, required=True)
    parser.add_argument('--jni-include', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    root = args.sysroot.resolve(strict=True)
    jni = args.jni_include.resolve(strict=True)
    if args.output.exists():
        raise ValueError('refusing existing bridge output')
    for header in (jni / 'jni.h', jni / 'linux/jni_md.h', root / 'usr/include/glib-2.0/glib.h'):
        if not header.is_file():
            raise ValueError('required signed development/JNI header missing: ' + str(header))
    environment = {**os.environ, 'PKG_CONFIG_SYSROOT_DIR': str(root),
                   'PKG_CONFIG_PATH': '',
                   'PKG_CONFIG_LIBDIR': str(root / 'usr/lib/pkgconfig') + ':' + str(root / 'usr/share/pkgconfig')}
    flags = shlex.split(subprocess.check_output(['pkg-config', '--cflags', '--libs', 'gtk4', 'gio-2.0'],
                                              env=environment, text=True))
    command = ['clang', '--target=aarch64-linux-musl', '--sysroot=' + str(root), '-fuse-ld=lld',
               '-std=c11', '-O2', '-Wall', '-Wextra', '-Werror', '-shared', '-fPIC', '-nostdlib',
               '-Wl,--no-undefined', '-Wl,-z,relro,-z,now,-z,noexecstack', '-I' + str(jni),
               '-I' + str(jni / 'linux'), str(Path(__file__).with_name('chooser.c')),
               *flags, '-lc', '-o', str(args.output)]
    subprocess.run(command, check=True)
    subprocess.run(['sha256sum', str(args.output)], check=True)


if __name__ == '__main__':
    main()
