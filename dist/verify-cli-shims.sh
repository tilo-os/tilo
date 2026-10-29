#!/usr/bin/env bash
# Fail when a packaged command shim cannot run.
#
# `tilo-install` places one copy of `tilo-cli-shim` per command an app declares,
# each beside a `<command>.tilo-shim` sidecar naming the package and command it
# runs. A shim whose sidecar is missing only prints "sidecar ... unreadable"
# when someone types the command, so a package that ships one is broken; so is
# a sidecar whose shim is gone. Checked at build and after installation.
set -euo pipefail

[[ $# == 1 ]] || { echo "usage: $0 SHIM_DIR" >&2; exit 2; }
dir=${1%/}
[[ -d $dir ]] || { echo "not a directory: $dir" >&2; exit 2; }

status=0
shopt -s nullglob
for entry in "$dir"/*; do
    name=${entry##*/}
    if [[ $name == *.tilo-shim ]]; then
        command=${entry%.tilo-shim}
        [[ -f $command && -x $command ]] || {
            echo "sidecar without its command shim: $entry" >&2
            status=1
        }
        continue
    fi
    [[ -f $entry ]] || continue
    sidecar=$entry.tilo-shim
    if [[ ! -f $sidecar ]]; then
        echo "command shim without its sidecar: $entry (expected $sidecar)" >&2
        status=1
        continue
    fi
    [[ -x $entry ]] || { echo "command shim is not executable: $entry" >&2; status=1; }
    for key in package_id command; do
        grep -Eq "^[[:space:]]*$key[[:space:]]*=[[:space:]]*\"[^\"]+\"" "$sidecar" || {
            echo "sidecar names no $key: $sidecar" >&2
            status=1
        }
    done
done
exit "$status"
