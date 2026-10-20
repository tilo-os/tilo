#!/usr/bin/env bash
# What t.toml's `tilo-vm-pack-*` checks run for dist/dev/t-builder.sh: the
# `tilo-vm layer-pack` command the builder left in .tilo-vm-pack/args.json,
# run by TILO_VM (the tilo-vm-binary-<arch> check's build, so a swap doesn't
# compile tilo-vm again) against the uploaded worktree. The framed layer it
# prints is the check's output; the build's own output goes to the log.
# usage: t-pack.sh TILO_VM
set -euo pipefail
tilo_vm=${1:?usage: t-pack.sh TILO_VM}
mapfile -t argv < <(python3 -c 'import json; [print(a) for a in json.load(open(".tilo-vm-pack/args.json"))]')
[[ ${argv[0]:-} == layer-pack ]] || { echo "t-pack: only layer-pack runs here, not ${argv[0]:-nothing}" >&2; exit 1; }
out=/target/tilo-vm-pack
rm -rf "$out"
mkdir -p "$out"
"$tilo_vm" "${argv[@]}" > "$out/layer.txt"
