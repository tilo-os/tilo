#!/usr/bin/env bash
# A tilo-vm remote builder backed by t: `swap --builder remote` builds a
# device's layer on a t node of the device's architecture instead of on this
# machine, and the layer comes back through this host to the device.
#
# tilo-vm runs a remote builder as a cargo drop-in, from the worktree root:
#   dist/dev/t-builder.sh ARCH run --locked -p tilo-vm --bin tilo-vm -- layer-pack ARGS...
# This script hands `tilo-vm layer-pack ARGS...` to one of t.toml's
# `tilo-vm-pack-*-<arch>` checks, which runs it on a node of that architecture
# against this worktree (uncommitted changes and tilo-vm's source inventory
# included) and keeps the framed layer it prints as the check's output. The
# check is picked by the component's size, so each declares what its builds
# really take; tilo-vm itself is not built here but taken from the
# tilo-vm-binary-<arch> check. The script downloads that output and writes it
# to stdout, where tilo-vm reads the layer; everything t says goes to stderr.
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
    aarch64|arm64) arch=arm64 ;;
    x86_64|amd64) arch=x86_64 ;;
    *) die "no t check builds for $arch" ;;
esac
# Everything up to `--` is the cargo invocation that builds tilo-vm itself;
# t builds it in the tilo-vm-binary-<arch> check instead. What follows is
# tilo-vm's command.
while (($#)) && [[ $1 != -- ]]; do shift; done
(($#)) || die "expected '-- layer-pack ...' after the cargo arguments"
shift
[[ ${1:-} == layer-pack ]] || die "only layer-pack runs on t, not ${1:-nothing}"
component=
for ((i = 2; i < $#; i++)); do
    [[ ${!i} == --component ]] && { j=$((i + 1)); component=${!j}; }
done
[[ -n $component ]] || die "layer-pack names no --component"
# The app collection and its runtime are a thin-app build of every app; the
# shell (and the runtime host built from its package) is one large release
# binary; every other component is a release build of a smaller program.
case $component in
    apps+runtime) check=tilo-vm-pack-apps-$arch ;;
    shell|native-runtime-host) check=tilo-vm-pack-shell-$arch ;;
    *) check=tilo-vm-pack-$arch ;;
esac
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
# t's own output goes to stderr and, to learn the run's id, to a file beside the layer.
said=$out/t-said
if ! t --human outputs "$check" --to "$out" 2>&1 | tee "$said" >&2; then
    # The client may have lost the run (a server restart) while it goes on there: the pack is
    # uncached, so asking again would build it again from the start. Take that run's outputs.
    run=$(grep -o -m1 '^run r[0-9a-f]*' "$said" | cut -d' ' -f2 || true)
    [[ -n $run ]] || die "the $check check failed; \`t log $check\` shows why"
    printf 't-builder: following run %s for its layer\n' "$run" >&2
    t --human outputs "$check" --run "$run" --to "$out" >&2 \
        || die "the $check check failed in run $run; \`t log $check --run $run\` shows why"
fi
layer=$(find "$out" -name layer.txt -print -quit)
[[ -n $layer && -s $layer ]] || die "the $check check produced no layer"
cat "$layer"
