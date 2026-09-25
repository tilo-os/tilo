#!/usr/bin/env bash
set -euo pipefail

install_script="$(dirname "$0")/../usr/libexec/tilo-install"
function_text="$(tr -d '\r' < "$install_script" | sed -n '/^release_copy_scratch() {/,/^}/p')"
[ -n "$function_text" ] || exit 1
eval "$function_text"

ENC_MNT=/fake/encrypted-target
COPY_SCRATCH="$ENC_MNT/.tilo-bootc-copy-tmp"
COPY_SCRATCH_MOUNTED=1
trace=()
umount() { trace+=(umount); return 1; }
rm() { trace+=(rm); }
if release_copy_scratch; then
  echo 'scratch cleanup unexpectedly succeeded after unmount failed' >&2
  exit 1
fi
[ "${trace[*]}" = umount ]
[ "$COPY_SCRATCH_MOUNTED" -eq 1 ]
[ -n "$COPY_SCRATCH" ]

trace=()
umount() { trace+=(umount); return 0; }
release_copy_scratch
[ "${trace[*]}" = 'umount rm' ]
[ "$COPY_SCRATCH_MOUNTED" -eq 0 ]
[ -z "$COPY_SCRATCH" ]
