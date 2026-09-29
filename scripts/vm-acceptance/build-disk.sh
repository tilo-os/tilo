#!/usr/bin/env bash
# Build a dev disk (qcow2) of one commit on the remote builder, fetch it, and write the
# provenance record run.py checks: <disk>.provenance.json names the commit the image was
# built from, which the booted image cannot say about itself.
#
# usage: build-disk.sh <commit-ish> <out.qcow2>
#
#   TILO_DISK_BUILDER   ssh destination of the rcargo builder (e.g. fedora@builder)
#   TILO_DISK_SCRATCH   scratch directory on the builder for image storage and output
#   TILO_DISK_MIRRORS   glob of rcargo worktree mirrors on the builder
#                       (default /srv/rcargo/p/*/w/*)
#   RPMS_REF, KERNEL_RPMS_REF   optional overrides; build-image.sh resolves both otherwise
#
# Nothing is compiled here: the OS image is built by `rcargo --on-builder run -p tilo-vm --
# image build`, the disk by os/dev/build-image.sh on the builder, and only the qcow2 is copied.
set -euo pipefail

[ $# -eq 2 ] || { sed -n '2,15p' "$0"; exit 2; }
: "${TILO_DISK_BUILDER:?set TILO_DISK_BUILDER to the builder's ssh destination}"
: "${TILO_DISK_SCRATCH:?set TILO_DISK_SCRATCH to a scratch directory on the builder}"
MIRRORS=${TILO_DISK_MIRRORS:-/srv/rcargo/p/*/w/*}
out=$(realpath -m "$2")
repo=$(git rev-parse --show-toplevel)
sha=$(git -C "$repo" rev-parse --verify "$1^{commit}")
sde=$(git -C "$repo" log -1 --format=%ct "$sha")

wt=$(mktemp -d "${TMPDIR:-/tmp}/tilo-disk-XXXXXX")
cleanup() { git -C "$repo" worktree remove --force "$wt" >/dev/null 2>&1 || true; }
trap cleanup EXIT
git -C "$repo" worktree add --detach "$wt" "$sha" >/dev/null

echo "== image for ${sha:0:12} (SOURCE_DATE_EPOCH=$sde)"
marker=$(ssh "$TILO_DISK_BUILDER" 'm=$(mktemp); echo "$m"')
(cd "$wt" && RCARGO_ENV_SOURCE_DATE_EPOCH=$sde rcargo --on-builder run -p tilo-vm -- image build)

echo "== disk"
# shellcheck disable=SC2087 # the builder expands its own variables
ssh -o ServerAliveInterval=30 "$TILO_DISK_BUILDER" bash -s -- "$TILO_DISK_SCRATCH" "$sde" \
  "${RPMS_REF:-}" "${KERNEL_RPMS_REF:-}" "${sha:0:12}" "$marker" "$MIRRORS" <<'REMOTE' | tee "$out.build.log"
set -euo pipefail
S=$1 SDE=$2 RPMS=$3 KERNEL=$4 TAG=$5 MARKER=$6 MIRRORS=$7
oci=$(for d in $MIRRORS/target/tilo-vm/image; do [ "$d/index.json" -nt "$MARKER" ] && echo "$d"; done | head -1)
rm -f "$MARKER"
[ -n "$oci" ] || { echo "no OCI layout newer than this build under $MIRRORS" >&2; exit 1; }
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
echo "== fetch $remote"
rsync --sparse --progress "$TILO_DISK_BUILDER:$remote" "$out"
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
