#!/bin/sh
# DNF/RPM record the transaction wall clock in every intermediate image layer.
# Installed as both dnf and rpm, only during rootfs assembly with frozen RPMs.
set -eu
test -n "${SOURCE_DATE_EPOCH:-}"
test -r /usr/libexec/tilo/build-clock.so
export LD_PRELOAD="/usr/libexec/tilo/build-clock.so${LD_PRELOAD:+:$LD_PRELOAD}"
tool="${0##*/}"
case "$tool" in dnf|rpm) ;; *) exit 125 ;; esac
exec "/usr/bin/$tool" "$@"
