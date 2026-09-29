#!/bin/sh
# Remove only DNF diagnostics whose bytes depend on the transaction wall clock,
# ldconfig's aux-cache, which records the inode numbers and ctimes of the
# libraries every RPM transaction re-indexes, and fontconfig's caches: font
# package triggers and anything that loads fontconfig rewrite them, and which
# ones a layer ends up with differs between builds (releases 48898, 49265 and
# 49687 each had a cache or temporary file on one lane only). The rootfs writes
# the system font cache once, last. Called before each image-layer snapshot;
# an optional root is for the test.
set -eu
root="${1:-/}"
test -d "$root/usr/lib/sysimage/libdnf5"
test -d "$root/var/log"
rm -f -- \
    "$root/usr/lib/sysimage/libdnf5/transaction_history.sqlite" \
    "$root/usr/lib/sysimage/libdnf5/transaction_history.sqlite-wal" \
    "$root/usr/lib/sysimage/libdnf5/transaction_history.sqlite-shm" \
    "$root/var/log/dnf5.log" "$root"/var/log/dnf5.log.* \
    "$root/var/cache/ldconfig/aux-cache" \
    "$root"/usr/lib/fontconfig/cache/* \
    "$root"/usr/lib/fontconfig/cache/.[!.]* \
    "$root"/var/cache/fontconfig/*
