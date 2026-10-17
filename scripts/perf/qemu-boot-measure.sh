#!/usr/bin/env bash
# Headless QEMU boot timing from the serial console.
#
# Boots the given qcow2, waits for multi-user.target or graphical.target,
# measures the kernel-log timestamp at that point, and shuts the VM down.
#
# Usage:  scripts/perf/qemu-boot-measure.sh <qcow2> <label>
#
# Emits a JSON result blob on stdout so callers can diff two runs
# without regex-scraping the serial console.
#
# Requires: qemu-system-x86_64, edk2-ovmf, kvm.

set -euo pipefail

# Leave empty to run QEMU and its tools on the host. Set this when the tools
# live in a development distrobox, for example `TILO_DISTROBOX=tilo-app`.
DISTROBOX="${TILO_DISTROBOX:-}"
run_in_env() {
    if [ -n "$DISTROBOX" ]; then
        distrobox enter "$DISTROBOX" -- "$@"
    else
        "$@"
    fi
}

QCOW="${1:-}"
LABEL="${2:-run}"
[ -f "$QCOW" ] || { echo "usage: $0 <qcow2> <label>" >&2; exit 2; }

OUT_DIR="${TILO_PERF_OUT:-$PWD/target/perf}"
mkdir -p "$OUT_DIR"
SNAP="$OUT_DIR/$LABEL.snapshot.qcow2"
CONSOLE="$OUT_DIR/$LABEL.console.log"
RESULT="$OUT_DIR/$LABEL.json"
# A failed repeat must not leave a previous successful measurement behind.
rm -f "$RESULT"

# --- Prep a snapshot so the base image never gets touched -------------------
# The qcow2 base stays read-only; this snapshot holds everything the VM
# writes during the boot measurement (systemd journal, resolv.conf,
# etc). We delete it at exit so consecutive runs are clean.
run_in_env \
    qemu-img create -f qcow2 -F qcow2 -b "$QCOW" "$SNAP" >/dev/null

# --- Boot in a fresh environment process ------------------------------------
# `-monitor unix:...` gives us a control socket to shut the VM down
# without needing the guest to cooperate; `-serial file:...` captures
# every byte of the serial console.
MONITOR="$OUT_DIR/$LABEL.monitor.sock"
rm -f "$MONITOR" "$CONSOLE"

run_in_env bash -c "
set -euo pipefail
qemu-system-x86_64 \
    -M q35 -m 2048 -smp 2 \
    -enable-kvm -cpu host \
    -drive if=pflash,format=raw,readonly=on,file=/usr/share/edk2/ovmf/OVMF_CODE.fd \
    -drive file=$SNAP,format=qcow2,if=virtio \
    -netdev user,id=n0 -device virtio-net-pci,netdev=n0 \
    -serial file:$CONSOLE \
    -monitor unix:$MONITOR,server,nowait \
    -display none \
    -daemonize \
    -pidfile $OUT_DIR/$LABEL.pid
" 2>&1

QEMU_PID="$(cat "$OUT_DIR/$LABEL.pid")"
trap 'kill "$QEMU_PID" 2>/dev/null || true; rm -f "$SNAP" "$MONITOR" "$OUT_DIR/$LABEL.pid"' EXIT

# --- Wait for the guest to reach a stable state -----------------------------
# Poll the console for "Reached target Multi-User System" (or the graphical
# target on a desktop image). Bail after 180 s so a stuck boot doesn't
# hang the script forever.
DEADLINE=$(( SECONDS + 180 ))
STATE="timeout"
while [ $SECONDS -lt $DEADLINE ]; do
    # Two accepted patterns:
    #   1. Bare log line: `systemd[1]: Reached target multi-user.target - ...`
    #   2. ANSI-coloured `[OK] Reached target ...multi-user.target...` — sed the
    #      colour codes off before matching so we don't miss it.
    if grep -qE "Kernel panic|Failed to load kernel" "$CONSOLE" 2>/dev/null; then
        STATE="panic"
        break
    fi
    if ! kill -0 "$QEMU_PID" 2>/dev/null; then
        STATE="exited"
        break
    fi
    if sed 's/\x1b\[[0-9;]*[mGKH]//g' "$CONSOLE" 2>/dev/null | \
        grep -qE "Reached target [^-]*(multi-user\.target|graphical\.target)"; then
        STATE="reached-target"
        break
    fi
    sleep 2
done

# Allow late-service output to reach the serial console before parsing it.
sleep 5

# --- Read an observed boot timestamp; never substitute zero -----------------
BOOT_TIME_MS=""
if [ "$STATE" = "reached-target" ]; then
    if ! BOOT_TIME_MS="$(sed 's/\x1b\[[0-9;]*[mGKH]//g' "$CONSOLE" | \
        awk '
            match($0, /\[[[:space:]]*[0-9]+\.[0-9]+\]/) {
                last_ts = substr($0, RSTART + 1, RLENGTH - 2)
                observed = 1
            }
            /Reached target [^-]*(multi-user\.target|graphical\.target)/ {
                if (observed) {
                    printf "%d", last_ts * 1000
                    found = 1
                }
                exit
            }
            END { if (!found) exit 1 }
        ')"; then
        STATE="missing-timing"
    fi
fi

# --- Shutdown --------------------------------------------------------------
# `system_powerdown` sends an ACPI shutdown request through QEMU's monitor
# socket; this avoids relying on a guest login or an SSH service.
if [ -S "$MONITOR" ]; then
    echo "system_powerdown" | run_in_env \
        nc -U -q1 "$MONITOR" >/dev/null 2>&1 || true
fi

# Give it 20 s to shut down cleanly; if it doesn't, kill.
for _ in $(seq 1 20); do
    kill -0 "$QEMU_PID" 2>/dev/null || break
    sleep 1
done
kill -9 "$QEMU_PID" 2>/dev/null || true

# Report measurement failures only after best-effort guest shutdown.
if [ "$STATE" != "reached-target" ] || [[ ! "$BOOT_TIME_MS" =~ ^[0-9]+$ ]]; then
    echo "boot measurement failed: $STATE; see $CONSOLE" >&2
    exit 1
fi

# --- Emit result -----------------------------------------------------------
cat > "$RESULT" <<EOF
{
  "label": "$LABEL",
  "state": "$STATE",
  "boot_ms": $BOOT_TIME_MS,
  "console_log": "$CONSOLE"
}
EOF
cat "$RESULT"
