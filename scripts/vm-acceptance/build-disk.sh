#!/usr/bin/env bash
# Build a dev disk (qcow2) of one commit on the remote builder, fetch it, and write the
# provenance record run.py checks: <disk>.provenance.json names the commit the image was
# built from, which the booted image cannot say about itself.
#
# usage: build-disk.sh <commit-ish> <out.qcow2>
#
#   TILO_DISK_BUILDER   ssh destination of the remote builder (e.g. fedora@builder)
#   TILO_DISK_CARGO     a cargo drop-in that runs `run` on that builder (e.g. "<drop-in> --on-builder")
#   TILO_DISK_CARGO_ENV prefix the drop-in strips from variables it forwards to the builder
#                       (empty: SOURCE_DATE_EPOCH is exported as it is)
#   TILO_DISK_SCRATCH   scratch directory on the builder for image storage and output
#   TILO_DISK_MIRRORS   glob of the drop-in's worktree mirrors on the builder, each with the
#                       commit's tree under src/ and its build under target/
#   TILO_DISK_OCI       an OCI layout already built from <commit> on the builder: skip the
#                       image build and write the disk from it
#   RPMS_REF, KERNEL_RPMS_REF   optional overrides; build-image.sh resolves both otherwise
#   TILO_DISK_KEEP      disks this script built to keep next to <out.qcow2>, newest first
#                       (default 3; `all` keeps every one); see prune_disks.py
#
# Nothing is compiled here: the OS image is built by `$TILO_DISK_CARGO run -p tilo-vm --
# image build`, the disk by os/dev/build-image.sh on the builder, and only the qcow2 is copied.
# The copy is a delta when another tilo-dev-*.qcow2 already sits next to the destination:
# that file is reflinked (or copied) into place first, and rsync --inplace updates it.
set -euo pipefail

# Virtual size in bytes, or no output when the header cannot be read.
qcow_virtual_size() {
  local path=$1 json
  command -v qemu-img >/dev/null 2>&1 || return 0
  json=$(qemu-img info -U --output=json -- "$path" 2>/dev/null) || return 0
  python3 -c 'import json,sys; print(json.load(sys.stdin).get("virtual-size", ""))' <<<"$json" 2>/dev/null || true
}

# True when $1's directory holds some other tilo-dev-*.qcow2.
qcow_seed_candidates() {
  local dest=$1 dir base f
  dir=$(dirname -- "$dest")
  base=$(basename -- "$dest")
  [ -d "$dir" ] || return 1
  for f in "$dir"/tilo-dev-*.qcow2; do
    [ -f "$f" ] || continue
    [ "$(basename -- "$f")" = "$base" ] && continue
    return 0
  done
  return 1
}

# Virtual size of a qcow2 on the builder. Empty when qemu-img is unavailable,
# so the fetch still seeds from the newest local disk.
remote_qcow_virtual_size() {
  local path=$1 json
  json=$(ssh "$TILO_DISK_BUILDER" "qemu-img info -U --output=json -- $(printf '%q' "$path")" 2>/dev/null) || return 0
  python3 -c 'import json,sys; print(json.load(sys.stdin).get("virtual-size", ""))' <<<"$json" 2>/dev/null || true
}

# If $dest is absent, copy the newest other tilo-dev-*.qcow2 in its directory
# onto it. $2 is the wanted virtual size in bytes: a same-size seed wins, and
# a different size is still used when nothing matches. The copy is a new file
# (reflink when the filesystem can) because rsync --inplace would mutate a
# hard link and corrupt the disk it was linked from.
seed_qcow_basis() {
  local dest=$1 want=${2:-} dir base f t vsize
  local best_any="" best_any_t=-1 best_match="" best_match_t=-1
  [ -e "$dest" ] && return 0
  dir=$(dirname -- "$dest")
  base=$(basename -- "$dest")
  [ -d "$dir" ] || return 0
  for f in "$dir"/tilo-dev-*.qcow2; do
    [ -f "$f" ] || continue
    [ "$(basename -- "$f")" = "$base" ] && continue
    t=$(stat -c %Y -- "$f")
    if [ "$t" -gt "$best_any_t" ]; then
      best_any=$f
      best_any_t=$t
    fi
    if [ -n "$want" ]; then
      vsize=$(qcow_virtual_size "$f")
      if [ "$vsize" = "$want" ] && [ "$t" -gt "$best_match_t" ]; then
        best_match=$f
        best_match_t=$t
      fi
    fi
  done
  if [ -n "$best_match" ]; then
    f=$best_match
  elif [ -n "$best_any" ]; then
    f=$best_any
  else
    return 0
  fi
  echo "== seed $(basename -- "$dest") from $(basename -- "$f")"
  cp --reflink=auto -- "$f" "$dest" || cp -- "$f" "$dest"
  # rsync skips a file whose size and mtime already match. A seed can share
  # the remote's size; stamp it so the delta check always runs.
  touch -d @0 -- "$dest"
}

# `source build-disk.sh --seed-self-test` loads the helpers above and stops.
if [ "${1:-}" = "--seed-self-test" ]; then
  return 0 2>/dev/null || exit 0
fi

[ $# -eq 2 ] || { sed -n '2,20p' "$0"; exit 2; }
: "${TILO_DISK_BUILDER:?set TILO_DISK_BUILDER to the ssh destination of the builder}"
: "${TILO_DISK_SCRATCH:?set TILO_DISK_SCRATCH to a scratch directory on the builder}"
: "${TILO_DISK_MIRRORS:?set TILO_DISK_MIRRORS to the glob of worktree mirrors on the builder}"
MIRRORS=$TILO_DISK_MIRRORS
out=$(realpath -m "$2")
repo=$(git rev-parse --show-toplevel)
sha=$(git -C "$repo" rev-parse --verify "$1^{commit}")
sde=$(git -C "$repo" log -1 --format=%ct "$sha")

wt=$(mktemp -d "${TMPDIR:-/tmp}/tilo-disk-XXXXXX")
cleanup() { git -C "$repo" worktree remove --force "$wt" >/dev/null 2>&1 || true; }
trap cleanup EXIT
git -C "$repo" worktree add --detach "$wt" "$sha" >/dev/null

marker=
if [ -z "${TILO_DISK_OCI:-}" ]; then
  echo "== image for ${sha:0:12} (SOURCE_DATE_EPOCH=$sde)"
  marker=$(ssh "$TILO_DISK_BUILDER" 'm=$(mktemp); echo "$m"')
  : "${TILO_DISK_CARGO:?set TILO_DISK_CARGO to a cargo drop-in that runs on the builder}"
  # shellcheck disable=SC2086 # TILO_DISK_CARGO is a command line: the drop-in and its flags
  (cd "$wt" && env "${TILO_DISK_CARGO_ENV:-}SOURCE_DATE_EPOCH=$sde" $TILO_DISK_CARGO \
    run -p tilo-vm -- image build)
fi

echo "== disk"
# ssh joins its arguments into one command line, so each is quoted for the remote shell.
remote_args=$(printf '%q ' "$TILO_DISK_SCRATCH" "$sde" "${RPMS_REF:-}" "${KERNEL_RPMS_REF:-}" \
  "${sha:0:12}" "$marker" "$MIRRORS" "${TILO_DISK_OCI:-}")
ssh -o ServerAliveInterval=30 "$TILO_DISK_BUILDER" "bash -s -- $remote_args" <<'REMOTE' | tee "$out.build.log"
set -euo pipefail
S=$1 SDE=$2 RPMS=$3 KERNEL=$4 TAG=$5 MARKER=$6 MIRRORS=$7 oci=$8
if [ -z "$oci" ]; then
  for d in $MIRRORS/target/tilo-vm/image; do
    if [ "$d/index.json" -nt "$MARKER" ]; then oci=$d; break; fi
  done
  rm -f "$MARKER"
fi
[ -n "$oci" ] || { echo "no OCI layout newer than this build under $MIRRORS" >&2; exit 1; }
echo "oci_layout=$oci"
src=${oci%/target/tilo-vm/image}/src
mkdir -p "$S/src" "$S/storage" "$S/disks"
[ -f "$S/storage.conf" ] || printf '[storage]\ndriver = "overlay"\ngraphroot = "%s/storage"\nrunroot = "/run/user/%s/tilo-disk-run"\n' "$S" "$(id -u)" > "$S/storage.conf"
export CONTAINERS_STORAGE_CONF=$S/storage.conf
runroot=$(sed -n 's/^runroot = "\(.*\)"/\1/p' "$S/storage.conf")
for d in os keys tools; do rsync -a --delete --exclude=__pycache__ "$src/$d/" "$S/src/$d/"; done
rm -rf "$S/out/qcow2"
podman rmi -f localhost/tilo-vm:latest >/dev/null 2>&1 || true
podman unshare skopeo copy --quiet "oci:$oci:tilo-vm" \
  "containers-storage:[overlay@$S/storage+$runroot]localhost/tilo-vm:latest"
echo "oci_digest=$(podman image inspect --format '{{.Digest}}' localhost/tilo-vm:latest)"
cd "$S/src"
[ -n "$RPMS" ] && export RPMS_REF=$RPMS
[ -n "$KERNEL" ] && export KERNEL_RPMS_REF=$KERNEL
SOURCE_DATE_EPOCH=$SDE TILO_IMAGE=localhost/tilo-vm:latest TILO_SKIP_BUILD=1 \
  TILO_OUTPUT=$S/out TILO_DISK_SIZE=64G TILO_ROOT_PODMAN_STORE=$S/root-podman TILO_DISTROBOX= \
  ./os/dev/build-image.sh
mv "$S/out/qcow2/disk.qcow2" "$S/disks/tilo-dev-$TAG.qcow2"
echo "disk=$S/disks/tilo-dev-$TAG.qcow2"
REMOTE

remote=$(sed -n 's/^disk=//p' "$out.build.log" | tail -1)
digest=$(sed -n 's/^oci_digest=//p' "$out.build.log" | tail -1)
# A destination that does not exist yet has no basis, so rsync would send the
# whole image. Seed one first. --inplace writes the delta into that file;
# --sparse is omitted because, with --inplace, it does not punch holes unless
# --whole-file is also set, and --whole-file turns the delta off.
if [ ! -e "$out" ] && qcow_seed_candidates "$out"; then
  seed_qcow_basis "$out" "$(remote_qcow_virtual_size "$remote")"
fi
echo "== fetch $remote"
rsync --inplace --partial --no-whole-file --progress "$TILO_DISK_BUILDER:$remote" "$out"
ssh "$TILO_DISK_BUILDER" rm -f "$remote"

python3 - "$out" "$sha" "$digest" "$repo" <<'PY'
import datetime, json, subprocess, sys
out, sha, digest, repo = sys.argv[1:]
git = lambda *a: subprocess.run(["git", "-C", repo, *a], capture_output=True, text=True).stdout.strip()
record = {
    "commit": sha,
    "subject": git("log", "-1", "--format=%s", sha),
    "commit_time": git("log", "-1", "--format=%cI", sha),
    "oci_digest": digest or None,
    "built_at": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
    "built_by": "scripts/vm-acceptance/build-disk.sh",
}
with open(out + ".provenance.json", "w") as f:
    json.dump(record, f, indent=2)
print(f"wrote {out}.provenance.json")
PY

# Each disk costs its full ~15 GiB, so older ones go once this one is complete.
python3 "$(dirname -- "$0")/prune_disks.py" --protect "$out" "$(dirname -- "$out")" ||
  echo "prune: failed; older disks next to $out were left in place" >&2
