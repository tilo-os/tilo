#!/bin/sh
# Remove only DNF diagnostics whose bytes depend on the transaction wall clock.
# Called before each image-layer snapshot; an optional root is for the test.
set -eu
root="${1:-/}"
test -d "$root/usr/lib/sysimage/libdnf5"
test -d "$root/var/log"
rm -f -- \
    "$root/usr/lib/sysimage/libdnf5/transaction_history.sqlite" \
    "$root/usr/lib/sysimage/libdnf5/transaction_history.sqlite-wal" \
    "$root/usr/lib/sysimage/libdnf5/transaction_history.sqlite-shm" \
    "$root/var/log/dnf5.log" "$root"/var/log/dnf5.log.*
