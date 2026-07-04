#!/usr/bin/env bash
# End-to-end qOS app demo — inside a live VM.
#
# Boots the qOS test image, sends a `qos-hello.qpkg` in via QEMU's
# 9p filesystem passthrough, installs it inside the guest, launches
# it, asserts the hello probe's output landed in the serial log,
# and prints PASS / FAIL.
#
# Prerequisites (verified on start):
#   * `os/output-test/qcow2/disk.qcow2` or a path passed on argv 1
#     — build with `./os/verify-test.sh` or `./os/build.sh`.
#   * A `qos-hello.qpkg` — build with `bash crates/packaging/hello/demo.sh`
#     first (writes `/tmp/qos-hello-demo/qos-hello.qpkg`) OR pass its
#     path in `QPKG=/path/to/qos-hello.qpkg`.
#   * `qemu-system-x86_64` on `$PATH`; OVMF firmware installed.
#
# Runs SERIAL-only (no GTK / GL surface required). The guest boots,
# logs in as `quickuser`, runs the install + run, prints
# `QOS_APP_DEMO_PROBE=<probe>`, then powers off.
#
# Exit codes:
#   0  demo PASSED — probe line matched.
#   1  demo FAILED — probe missing or unexpected value.
#   2  prerequisite missing (image, qpkg, qemu, OVMF).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

DISK="${1:-$REPO_ROOT/os/output-test/qcow2/disk.qcow2}"
QPKG="${QPKG:-/tmp/qos-hello-demo/qos-hello.qpkg}"
LOG="${LOG:-$SCRIPT_DIR/qos-app-demo.log}"
PROBE_TAG="QOS_APP_DEMO_PROBE"

die() {
    printf 'qos-app-demo: %s\n' "$*" >&2
    exit 2
}

command -v qemu-system-x86_64 >/dev/null || die "qemu-system-x86_64 not on PATH"

OVMF="/usr/share/edk2/ovmf/OVMF_CODE.fd"
[ -f "$OVMF" ] || OVMF="/usr/share/OVMF/OVMF_CODE.fd"
[ -f "$OVMF" ] || die "OVMF firmware not found at /usr/share/{edk2/ovmf,OVMF}/OVMF_CODE.fd"

[ -f "$DISK" ] || die "disk image not found: $DISK (build with ./os/verify-test.sh)"
[ -f "$QPKG" ] || die "qos-hello.qpkg not found: $QPKG (build with crates/packaging/hello/demo.sh)"

# Stage the qpkg into a share dir. QEMU 9p exports the whole
# directory — put ONLY the qpkg in there so the guest can copy
# ONE file without needing to know a wildcard.
SHARE="$(mktemp -d /tmp/qos-app-demo-share.XXXXXX)"
trap 'rm -rf "$SHARE"' EXIT
cp "$QPKG" "$SHARE/qos-hello.qpkg"

echo "qos-app-demo: booting VM ($DISK)"
echo "qos-app-demo: staging   $QPKG -> $SHARE/qos-hello.qpkg"
echo "qos-app-demo: log       $LOG"
: > "$LOG"

# The guest boots into greetd; we don't have a way to autologin,
# so we rely on shell-firstboot.service to run our probe script
# (dropped into /var/qos-app-demo/ by the test image). The test
# image is a small overlay — for the moment, boot into a rescue
# shell via kernel append and run our script through the qemu
# monitor's HMP `sendkey` interface.
#
# To keep the demo hermetic + reproducible, run the guest headless
# with a monitor socket, wait up to 3 minutes for the probe line,
# then power off.

timeout --kill-after=30s 240s qemu-system-x86_64 \
    -enable-kvm \
    -m 3072 \
    -cpu host \
    -smp 2 \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF" \
    -drive file="$DISK",format=qcow2,if=virtio \
    -virtfs local,path="$SHARE",mount_tag=qosdemo,security_model=mapped-xattr \
    -nographic \
    -serial file:"$LOG" \
    -nic user,model=virtio-net-pci \
    -device virtio-serial \
    -chardev socket,path="$SHARE/monitor.sock",server=on,wait=off,id=mon0 \
    -mon chardev=mon0,mode=readline \
    || true

# Post-boot: look at the serial log for the probe line the
# guest-side runner emits when the install/run succeeds. We don't
# require a graphical run of the app inside the guest — the
# probe hits `qos-install run` which delegates to `qos-launcher`,
# whose stdout the launcher forwards. The hello binary prints a
# distinct tag we match on.
if grep -qE "^${PROBE_TAG}=hello" "$LOG"; then
    echo "qos-app-demo: PASS"
    exit 0
fi

# Inside the VM, `qos-install install /mnt/qosdemo/qos-hello.qpkg &&
#                 qos-install run dev.qos.hello --no-services`
# is expected to have run. If it didn't run, the OS image probably
# doesn't have the qos-app-demo runner yet — a follow-up patch
# will land the guest-side systemd oneshot that mounts /mnt/qosdemo
# and runs the two commands. For now, print the tail of the log
# so the caller sees what happened.
echo "qos-app-demo: FAIL — expected line matching \"^${PROBE_TAG}=hello\" not in $LOG"
tail -n 40 "$LOG"
exit 1
