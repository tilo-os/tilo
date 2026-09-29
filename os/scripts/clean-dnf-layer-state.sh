#!/bin/sh
# Remove only DNF diagnostics whose bytes depend on the transaction wall clock,
# ldconfig's aux-cache, which records the inode numbers and ctimes of the
# libraries every RPM transaction re-indexes, and the temporary files fc-cache
# runs from concurrent package scriptlets leave behind (random names; run
# 48898 and 49265 each had one on one lane only). Called before each
# image-layer snapshot; an optional root is for the test.
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
    "$root"/usr/lib/fontconfig/cache/*.TMP-* \
    "$root"/var/cache/fontconfig/*.TMP-*
