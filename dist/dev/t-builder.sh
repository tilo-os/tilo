#!/usr/bin/env bash
# A tilo-vm remote builder backed by t: `swap --builder remote` builds a
# device's layer on a t node of the device's architecture instead of on this
# machine, and the layer comes back through this host to the device.
#
# tilo-vm runs a remote builder as a cargo drop-in, from the worktree root:
#   dist/dev/t-builder.sh ARCH run --locked -p tilo-vm --bin tilo-vm -- layer-pack ARGS...
# This script hands `tilo-vm layer-pack ARGS...` to t.toml's
# `tilo-vm-pack-<arch>` check, which runs it on a node of that architecture
# against this worktree (uncommitted changes and tilo-vm's source inventory
# included) and keeps the framed layer it prints as the check's output. The
# script downloads that output and writes it to stdout, where tilo-vm reads
# the layer; everything t says goes to stderr.
#
# Register it once per architecture in ~/.config/tilo-vm/vms.toml:
#   [remote_builders]
#   aarch64 = ["dist/dev/t-builder.sh", "aarch64"]
#   x86_64 = ["dist/dev/t-builder.sh", "x86_64"]
# (paths are relative to the worktree being swapped from), then
#   tilo-vm swap shell --from worktree --builder remote
# A device swap picks the remote builder by default once one is registered.
set -euo pipefail

die() { printf 't-builder: %s\n' "$*" >&2; exit 1; }

arch=${1:?usage: t-builder.sh ARCH run ... -- layer-pack ARGS...}
shift
case $arch in
    aarch64|arm64) check=tilo-vm-pack-arm64 ;;
    x86_64|amd64) check=tilo-vm-pack-x86_64 ;;
    *) die "no t check builds for $arch" ;;
esac
# Everything up to `--` is the cargo invocation that builds tilo-vm itself;
# the check makes that build on the node. What follows is tilo-vm's command.
while (($#)) && [[ $1 != -- ]]; do shift; done
(($#)) || die "expected '-- layer-pack ...' after the cargo arguments"
shift
[[ ${1:-} == layer-pack ]] || die "only layer-pack runs on t, not ${1:-nothing}"
command -v t >/dev/null || die "t is not on PATH (docs/t.md)"

root=$PWD
[[ -f $root/os/components.toml ]] || die "run from a Tilo worktree root"
request=$root/.tilo-vm-pack
mkdir -p "$request"
# The check reads the command from here; the source inventory tilo-vm wrote
# beside it travels with the worktree.
python3 -c 'import json, sys; json.dump(sys.argv[1:], sys.stdout)' "$@" > "$request/args.json"
# The framed layer is large (hundreds of MB for the shell with its fonts):
# it waits on disk, not in a tmpfs /tmp, and goes once it is streamed.
cache=${XDG_CACHE_HOME:-$HOME/.cache}/tilo-vm
mkdir -p "$cache"
out=$(mktemp -d "$cache/t-builder.XXXXXX")
cleanup() { rm -rf "$out" "$request"; }
trap cleanup EXIT

printf 't-builder: building on t (%s)\n' "$check" >&2
if ! t --human outputs "$check" --to "$out" >&2; then
    die "the $check check failed; \`t log $check\` shows why"
fi
layer=$(find "$out" -name layer.txt -print -quit)
[[ -n $layer && -s $layer ]] || die "the $check check produced no layer"
cat "$layer"
