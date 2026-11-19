#!/usr/bin/env bash
# Boot a preloaded installer medium and prove it reaches its installer.
#
#   server-installer-smoke.sh MEDIUM_DIR HOST_DIGEST OUT
#
# MEDIUM_DIR is what tools/ci/publish/installer-medium.sh wrote: disk.qcow2 and
# the source.json that names the image it was made from and the image it
# preloads as the install target. HOST_DIGEST is the server image's OCI digest.
#
# The medium boots in a KVM guest with a blank target disk, and the check waits
# until the graphical installer answers on its console (the same
# tilo-control protocol its own buttons use). It then proves the medium's
# preloaded target is the server image: repository, digest, architecture and the
# update origin the installed machine will follow. Booting runs the medium on a
# writable overlay, so the artifact itself is never modified.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/publish/lib.sh"
cd "$repo_root"

medium=${1:?usage: server-installer-smoke.sh MEDIUM_DIR HOST_DIGEST OUT}
host_digest=${2:?missing server image digest}
out=${3:?missing output directory}
[[ -f $medium/disk.qcow2 ]] || die "$medium holds no installer medium"
[[ -f $medium/source.json ]] || die "$medium holds no source.json"
[[ $host_digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "server image digest is invalid: $host_digest"

rm -rf "$out"
mkdir -p "$out"
storage="$(dirname "$out")/.$(basename "$out")-storage"
rm -rf "$storage"
mkdir -p "$storage"

export TILO_VM_CYCLE="server-installer-smoke-$$"
export TILO_RUN="$storage/run"
export TILO_INSTALLER_DISK="$medium/disk.qcow2"
export TILO_INSTALLER_WORK_DISK="$storage/installer-work.qcow2"
export TILO_TARGET_DISK="$storage/target.qcow2"
export TILO_TARGET_SIZE="${TILO_TARGET_SIZE:-20G}"
export TILO_BOOT_WAIT_SECS="${TILO_BOOT_WAIT_SECS:-1500}"
# The installer guest is sized to this machine: asking for more memory than
# the host has leaves the guest too small to start the graphical installer.
memory=$(awk '/^MemTotal:/ { print int($2 / 1024 / 2) }' /proc/meminfo 2>/dev/null || true)
[[ $memory =~ ^[0-9]+$ ]] || memory=2048
(( memory > 4096 )) && memory=4096
(( memory < 2048 )) && memory=2048
export TILO_MEM="${TILO_MEM:-$memory}"
vm="$repo_root/os/dev/vm.sh"

stop() {
  "$vm" stop >/dev/null 2>&1 || true
  rm -rf "$storage"
}
trap stop EXIT
"$vm" boot-installer --2d || die "the medium did not start in a KVM guest"
if ! python3 "$repo_root/os/dev/installer-console.py" --socket "$("$vm" console)" \
  wait installer --timeout "$TILO_BOOT_WAIT_SECS" --save "$out/console-state.json"; then
  # The guest's serial is the only evidence a boot that never reached the
  # installer leaves, and the trap below removes it with the storage.
  cp "$TILO_RUN/serial.log" "$out/serial.log" 2>/dev/null || true
  tail -n 120 "$out/serial.log" >&2 2>/dev/null || true
  die "the medium never reached the graphical installer"
fi
cp "$TILO_RUN/serial.log" "$out/serial.log" 2>/dev/null || true
"$vm" stop >/dev/null 2>&1 || true

python3 - "$out/console-state.json" "$medium/source.json" "$medium/disk.qcow2" \
  "$host_digest" "$out/verdict.json" <<'PY'
import hashlib
import json
from pathlib import Path
import sys

state_path, source_path, medium_path, host_digest, destination = sys.argv[1:]
state = json.loads(Path(state_path).read_text())
source = json.loads(Path(source_path).read_text())
with Path(medium_path).open("rb") as medium:
    medium_digest = hashlib.file_digest(medium, "sha256").hexdigest()
require = {
    "the installer answered": state.get("app") == "installer",
    "the medium is fresh": medium_digest == source.get("medium_sha256"),
    "the medium preloads a target": isinstance(source.get("target"), dict),
}
target = source.get("target") or {}
require["the target is the server image"] = target.get("repository") == "registry.tilo.so/tilo/tilo-host"
require["the target is this run's server image"] = target.get("target_digest") == host_digest
require["the target declares its architecture"] = target.get("architecture") == "x86_64"
require["the target names its update origin"] = (
    target.get("update_origin", "").startswith("registry.tilo.so/tilo/tilo-host:"))
failed = [name for name, ok in require.items() if not ok]
if failed:
    raise SystemExit("server installer medium: " + "; ".join(failed))
Path(destination).write_text(json.dumps({
    "schema": "tilo.server-installer-smoke.v1",
    "installer_step": state.get("step"),
    "target": target,
}, indent=2, sort_keys=True) + "\n")
PY
printf 'the medium reached its installer with %s as the target\n' "$host_digest"
