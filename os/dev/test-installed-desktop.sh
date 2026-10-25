#!/usr/bin/env bash
# The installed desktop as a person meets it, on a throwaway overlay of an
# installed dev disk: the sign-in screen, first use, the apps, input plugged in
# after login, the lock screen, and what makes the disk a dev VM.
#
# Every step drives the guest the way a person or its device admin does: keys
# reach it through QEMU's keyboard, the UI is read back over AT-SPI by an
# external client, and the admin's own ssh login runs the action checks. No
# test-only minting or bypass enters the image.
#
# Usage: os/dev/test-installed-desktop.sh DISK_QCOW2 [--out DIR] [--display gl|2d]
#                                         [--base-digest sha256:...]
#
# DISK_QCOW2 is never booted or modified. --base-digest is the OS image the dev
# image was built on, which tilo-vmd must report. The live system update is
# os-dev-gate's (tools/ci/publish/os-dev-gate.sh).
set -euo pipefail

dev_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$dev_dir/../.." && pwd)"
vm="$dev_dir/vm.sh"
base="${1:?usage: test-installed-desktop.sh DISK_QCOW2 [--out DIR] [--display gl|2d] [--base-digest sha256:...]}"
shift
out="$PWD/installed-desktop-artifacts"
display="${TILO_VM_DISPLAY:-gl}"
base_digest=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --out) out="${2:?--out needs a directory}"; shift 2 ;;
    --display) display="${2:?--display needs gl or 2d}"; shift 2 ;;
    --base-digest) base_digest="${2:?--base-digest needs a digest}"; shift 2 ;;
    *) printf 'unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
done
case "$display" in gl|2d) ;; *) echo '--display must be gl or 2d' >&2; exit 2 ;; esac
[ -f "$base" ] || { printf 'disk not found: %s\n' "$base" >&2; exit 1; }
base="$(cd "$(dirname "$base")" && pwd)/$(basename "$base")"
out="$(mkdir -p "$out" && cd "$out" && pwd)"

export TILO_USER="${TILO_USER:-quickuser}"
export TILO_PASS="${TILO_PASS:-tilo}"
arch="${TILO_VM_ARCH:-$(uname -m)}"
# Short: QEMU's monitor and console sockets live here (108-byte AF_UNIX limit).
export TILO_RUN="${TILO_DESKTOP_RUN:-/var/tmp/tilo-desktop-$arch}"
export TILO_SSH_PORT="${TILO_DESKTOP_SSH_PORT:-2222}"
export TILO_DISK="$TILO_RUN/disk.qcow2"

die() { printf 'installed desktop: %s\n' "$*" >&2; exit 1; }
note() { printf 'installed desktop: %s\n' "$*" >&2; }
session_journal() {
  "$vm" ssh 'journalctl -b --no-pager -o short-monotonic -n 300 _UID=$(id -u)' || true
}

rm -rf -- "$TILO_RUN"
install -d -m 0700 "$TILO_RUN"

# Any failing check leaves the guest's own account of it in the log: failed
# units and this boot's warnings (the user reads the system journal through
# wheel), before the VM goes away.
cleanup() {
  local rc=$?
  trap - EXIT
  kill "$sampler" 2>/dev/null || true
  if [ "$rc" -ne 0 ]; then
    TILO_SSH_BOUND=60 "$vm" ssh 'systemctl --failed --no-pager; journalctl -b --no-pager -o short-monotonic -p warning -n 200' || true
    # The session's own account: the compositor logs to its state directory.
    TILO_SSH_BOUND=60 "$vm" ssh 'journalctl -b --no-pager -o short-monotonic -n 400 _UID=$(id -u)' \
      > "$out/session-journal.txt" 2>&1 || true
    TILO_SSH_BOUND=60 "$vm" ssh 'cat "$HOME"/.local/state/tilo/session-logs/*.log' \
      > "$out/compositor-session.log" 2>&1 || true
  fi
  "$vm" stop >/dev/null 2>&1 || true
  cp -f "$TILO_RUN/qemu.log" "$TILO_RUN/serial.log" "$TILO_RUN"/*.png "$out/" 2>/dev/null || true
  rm -f -- "$TILO_DISK"
  printf '%s\n' "$rc" > "$out/exit-status.txt"
  exit "$rc"
}
sampler=
trap cleanup EXIT

"$vm" tool qemu-img create -f qcow2 -F qcow2 -b "$base" "$TILO_DISK" >/dev/null
"$vm" boot "--$display"
# The boot gate below counts the time the boot waited on this host (its disk,
# its CPUs) apart from the image's own, from what the guest records while it
# boots: from as soon as it answers SSH until five minutes after it started,
# twice a second, its uptime, I/O pressure total and CPU steal.
host_samples=/tmp/tilo-boot-host-samples
sample_host_wait() {
  local attempt
  for attempt in $(seq 1 150); do
    "$vm" ssh "while read -r up _ < /proc/uptime && [ \${up%.*} -lt 300 ]; do
        printf '%s %s %s\n' \$up \$(sed -n 's/^full .*total=//p' /proc/pressure/io) \
          \$(awk '\$1 == \"cpu\" { print \$9 }' /proc/stat) >> $host_samples; sleep 0.5; done" \
      >/dev/null 2>&1 && return 0
    sleep 1
  done
}
sample_host_wait >/dev/null 2>&1 &
sampler=$!
"$vm" wait
for attempt in $(seq 1 60); do
  "$vm" ssh true >/dev/null 2>&1 && break
  if [ "$attempt" = 60 ]; then
    "$vm" serial | tail -120
    die "no SSH after boot"
  fi
  sleep 2
done
kvm=0
python3 "$dev_dir/probe-kvm.py" "$arch" >/dev/null 2>&1 && kvm=1

# Fixed QEMU hardware makes this a stable regression gate. Keep a generous
# ceiling for shared-host variance while still catching a newly serialized
# network wait, initramfs stall, or service timeout. Before the login: it
# checks the greeter's login endpoint, which greetd removes once the session
# has started.
"$vm" with-tests "TILO_BOOT_USERSPACE_BUDGET_SEC=20 TILO_BOOT_HOST_SAMPLES=$host_samples \"\$TILO_GUEST_TESTS/boot-performance-gate.sh\""

# The sign-in screen as a person meets it, judged from the frames QEMU scans
# out: whole before any input and while the pointer moves, each keystroke on
# screen within the latency bound and repainting only the field, and a wrong
# passcode answered with a plain sentence on a steady frame.
"$vm" greeter-acceptance "$out/greeter"

# The disk holds one account, so the greeter shows its personal form: the
# account is chosen and the passcode field has focus.
"$vm" login --passcode-only "$TILO_PASS"

# A fresh installed user: the compositor starts directly into its trusted
# first-login identity ceremony. The three setup actions go through the AT-SPI
# provider, each resulting tree transaction is observed, and finishing uses
# ordinary keyboard input. The artifact poll is the barrier between Create and
# the final screen.
for attempt in $(seq 1 60); do
  "$vm" ssh 'busctl --user status org.freedesktop.impl.portal.desktop.tilo >/dev/null 2>&1' && break
  if [ "$attempt" = 60 ]; then
    "$vm" serial | tail -120
    session_journal
    die "the compositor did not reach first-login onboarding"
  fi
  sleep 1
done
sleep 2
# The session that sign-in started is on screen, fully painted.
"$vm" check-frame 05-after-login "$out/greeter"
# Password fields refuse Copy and Cut. Typed and copied with the real
# keyboard, ordinary chrome text reaches the session clipboard while the
# backup passphrase leaves it holding what it held.
"$vm" test native-atspi-smoke.sh oobe-copy-plain
"$vm" type tiloplainprobe
"$vm" keys ctrl-a ctrl-c
"$vm" test native-atspi-smoke.sh clipboard-is tiloplainprobe
"$vm" test native-atspi-smoke.sh oobe-copy-secret
"$vm" type tilosecretprobe42
"$vm" keys ctrl-a ctrl-c ctrl-x
"$vm" test native-atspi-smoke.sh clipboard-keeps tilo-clipboard-sentinel tilosecretprobe42
"$vm" test native-atspi-smoke.sh oobe-actions
for attempt in $(seq 1 60); do
  "$vm" ssh 'test -s "$HOME/.config/tilo/identity/primary.json" && test -s /var/lib/tilo/device/cert.bin' && break
  if [ "$attempt" = 60 ]; then
    "$vm" serial | tail -120
    session_journal
    die "the identity ceremony did not produce the certified chain"
  fi
  sleep 1
done
# The ready screen is finished with the keyboard: Tab reaches its primary
# action once the screen holds focus, and Return takes it.
"$vm" test native-atspi-smoke.sh oobe-ready
"$vm" keys tab
"$vm" test native-atspi-smoke.sh oobe-finish-focused
"$vm" keys ret
"$vm" test native-atspi-smoke.sh oobe-finished

# A desktop that reached its first session can still be degraded. No unit may
# have failed by now, and the rollback confirmation (which refuses to confirm
# a degraded generation) must have run and succeeded. Before the app checks,
# so none of them can mask it.
"$vm" ssh "printf '%s\n' '$TILO_PASS' | sudo -S -p '' /usr/libexec/tilo/boot-units-gate.sh"

# The compositor presented this login's first-use flow on the presenter its
# GPU allows, within budget. The guest renders with Mesa software Vulkan, so
# the budgets are a software renderer's; an emulated CPU only proves the
# contract.
if [ "$kvm" = 1 ]; then
  render_budgets='--render-budget-ms 1000 --frame-work-budget-ms 100'
else
  render_budgets='--render-budget-ms 10000 --frame-work-budget-ms 2000'
fi
"$vm" ssh "/usr/libexec/tilo-session-report --render-gate $render_budgets"

"$vm" test projection-boot-check.sh
# H.264 decode names the element the image really chose. Reported, not gating:
# the clip it decodes is written by an encoder only the opt-in codec pack
# carries, so a stock guest has no subject and the check fails saying so. Run
# it on a board whose SoC has a codec block with --require-hardware.
if "$vm" test h264-boot-check.py; then
  note 'H.264 decode named the element the image chose'
else
  note 'H.264 decode could not be observed (reported, not gating)'
fi
# AirPlay encrypted-media acceptance: phase-bounded deadlines on the shipped
# script. Its failure is reported and does not fail the desktop gate;
# OpenH264 arrives via the opt-in codec pack.
if "$vm" ssh /usr/libexec/tilo/airplay-encrypted-session-test.sh; then
  note 'AirPlay encrypted-session acceptance passed'
else
  note 'AirPlay encrypted-session acceptance failed (reported, not gating)'
fi
"$vm" test collaborative-files-image-contract.sh runtime
# The signed Settings and Files payloads, driven as an independent AT-SPI
# client.
"$vm" test installed-atspi-apps.sh
# The sandboxed Settings reads the device's network through the system
# authorities it is granted: its Network pane says what nmcli says.
"$vm" test installed-atspi-apps.sh settings-network
# The empty workspace's cards perform what they name.
"$vm" test installed-atspi-apps.sh card Notes Notes
"$vm" test installed-atspi-apps.sh 'card "Start a call" Chat --expect "Leave call"'
"$vm" test installed-atspi-apps.sh 'card Tasks Calendar --expect "Task list"'

# Browser renders a page through its Chromium engine runtime: it opens through
# the app palette's action, the page comes from a server in the guest, the
# address arrives on the real keyboard, and the loaded page's title is read
# back over AT-SPI.
page_log="$TILO_RUN/browser-page.log"
"$vm" test installed-atspi-apps.sh page > "$page_log" 2>&1 &
page_check=$!
page_url=""
for _ in $(seq 1 240); do
  page_url="$(sed -n 's/^ready //p' "$page_log")"
  [ -n "$page_url" ] && break
  kill -0 "$page_check" 2>/dev/null || break
  sleep 1
done
if [ -z "$page_url" ]; then
  cat "$page_log"
  die "Browser never offered its address field"
fi
"$vm" keys ctrl-l
"$vm" type "$page_url"
"$vm" keys ret
if ! wait "$page_check"; then
  cat "$page_log"
  session_journal
  die "Browser did not render the guest's page"
fi
cat "$page_log"

# Tabs are the desktop's. Ctrl+T on the real keyboard in an app opens another
# instance of it as a tab beside the first: one window group gains exactly one
# tab and a second process of the app joins the bus.
new_tab_gate() {
  local app=$1 log gate status=0
  log="$TILO_RUN/new-tab-$app.log"
  "$vm" test installed-atspi-apps.sh new-tab "$app" > "$log" 2>&1 &
  gate=$!
  for _ in $(seq 1 180); do
    grep -q '^READY' "$log" && break
    kill -0 "$gate" 2>/dev/null || break
    sleep 1
  done
  if ! grep -q '^READY' "$log"; then
    wait "$gate" || true
    cat "$log"
    die "$app did not open for the Ctrl+T gate"
  fi
  "$vm" keys ctrl-t
  wait "$gate" || status=$?
  cat "$log"
  [ "$status" = 0 ] || die "Ctrl+T in $app did not open it as a desktop tab"
}
new_tab_gate Files
new_tab_gate Browser
"$vm" test native-atspi-smoke.sh desktop
# Quick Settings reaches the system: the power profile changed there reads
# back from the daemon.
"$vm" test native-atspi-smoke.sh quick-settings
# The system authentication sheet is the compositor's: a real polkit request
# reaches an external reader as a dialog with password text it can focus,
# type into and cancel.
"$vm" test native-atspi-smoke.sh auth
# The greetd login left the display manager's SELinux domain: the session runs
# as the user, in its logind scope and app scopes, with no home access denied
# in its first minute.
"$vm" ssh /usr/libexec/tilo/session-context-check.sh
# The device admin drives the session through the action model over their own
# ssh login: no test control socket, no synthetic input.
"$vm" test installed-admin-actions.sh

# Typing in a note is a keyboard's job: the sentence reaches Notes as key
# presses, and the document reads back exactly what was typed, saved.
typed_sentence='The quick brown fox jumps over the lazy dog, twice!'
"$vm" test installed-atspi-apps.sh 'type-into Notes "New note" --field "Block text" --state /tmp/notes-typing'
"$vm" type "$typed_sentence"
"$vm" test installed-atspi-apps.sh "typed '$typed_sentence' --state /tmp/notes-typing --status Saved"

# Tab in a note nests the line under the one above instead of moving focus.
"$vm" test installed-atspi-apps.sh 'type-into Notes "New note" --field "Block text" --state /tmp/notes-nesting'
"$vm" type one
"$vm" keys ret
"$vm" type two
"$vm" keys tab
"$vm" test installed-atspi-apps.sh 'described "Paragraph block 2" "Level 2" --state /tmp/notes-nesting'

# A screenshot copied elsewhere pastes into a note as an image with Ctrl+V,
# kept by the document media service.
"$vm" test installed-atspi-apps.sh 'type-into Notes "New note" --field "Block text" --state /tmp/notes-image'
"$vm" test native-atspi-smoke.sh copy-image
# The copy reaches Notes as a selection event; give it a moment to read the
# image before pasting.
sleep 2
"$vm" keys ctrl-v
"$vm" test installed-atspi-apps.sh 'present Notes "Pasted image" --state /tmp/notes-image'

# A note typed for minutes keeps thousands of changes in its history. It still
# reopens and opens its History within budget: the reopened launch's
# APP-STARTUP line and History's HISTORY-OPEN line are judged below.
long_note=$(python3 -c 'import itertools; words = "sidebar flickers when a note is renamed twice then the search field loses focus".split(); print(" ".join(itertools.islice(itertools.cycle(words), 340))[:2000])')
"$vm" test installed-atspi-apps.sh 'type-into Notes "New note" --field "Block text" --state /tmp/notes-history'
"$vm" type "$long_note"
"$vm" test installed-atspi-apps.sh "typed '$long_note' --state /tmp/notes-history --status Saved --min-chars 1800"
"$vm" test installed-atspi-apps.sh "reopen-history Notes so.tilo.notes --expect-text '${long_note:0:12}'"

# Keyboards plugged in after login reach the running compositor: a USB
# keyboard hot-added on QEMU's bus and removed again, listed in the
# compositor's input snapshot as opened and dropped.
usb_kbd='QEMU QEMU USB Keyboard'
usb_kbds=$("$vm" test input-hotplug-check.sh "count '$usb_kbd'")
"$vm" cmd 'device_add usb-kbd,id=hotplug-kbd'
"$vm" test input-hotplug-check.sh "wait-count '$usb_kbd' $((usb_kbds + 1))"
"$vm" cmd 'device_del hotplug-kbd'
"$vm" test input-hotplug-check.sh "wait-count '$usb_kbd' $usb_kbds"

# The windows opened above logged the GPU adapter they render on: on a virgl
# boot none fell back to a CPU rasterizer.
"$vm" ssh "$(python3 "$repo/scripts/vm-acceptance/gpu_check.py" --probe)" \
  | python3 "$repo/scripts/vm-acceptance/gpu_check.py"

# Every app opened above logged its time from process start to first frame,
# held to its cold-start budget; an emulated CPU only reports them.
startup_judgement=()
[ "$kvm" = 1 ] || startup_judgement=(--report-only)
"$vm" ssh "$(python3 "$repo/scripts/vm-acceptance/app_startup_check.py" --probe)" \
  | python3 "$repo/scripts/vm-acceptance/app_startup_check.py" "${startup_judgement[@]}" --require so.tilo.notes

# A touchscreen plugged in after login brings up the on-screen keyboard: a tap
# on the search field shows it, and taps on three of its letter keys type into
# the field, read back over AT-SPI. Root owns the touchscreen; the session
# user's client says where to tap.
touch_log="$TILO_RUN/touch.log"
"$vm" with-tests "printf '%s\\n' '$TILO_PASS' | sudo -S -p '' \"\$TILO_GUEST_TESTS/input-hotplug-check.sh\" touchscreen \"\$XDG_RUNTIME_DIR/tilo/input-state.json\" \"/run/user/\$(id -u)/tilo-touch\" \"\$(id -un)\"" \
  > "$touch_log" 2>&1 &
touch_helper=$!
for _ in $(seq 1 60); do
  grep -q '^ready' "$touch_log" && break
  kill -0 "$touch_helper" 2>/dev/null || break
  sleep 1
done
if ! grep -q '^ready' "$touch_log"; then
  cat "$touch_log"
  die "the compositor never opened a touchscreen plugged in after login"
fi
if ! "$vm" test installed-atspi-apps.sh 'osk-search "/run/user/$(id -u)/tilo-touch"'; then
  cat "$touch_log"
  session_journal
  die "the on-screen keyboard did not type into a tapped field"
fi
wait "$touch_helper"
cat "$touch_log"
"$vm" keys esc

# Lock is the compositor's too: the Super+L route shows its protected password
# field and instructions on the session bus. The chord comes from a virtual
# keyboard plugged in for it, so a hot-plugged device's keys reach the
# compositor as well.
"$vm" with-tests "printf '%s\\n' '$TILO_PASS' | sudo -S -p '' \"\$TILO_GUEST_TESTS/input-hotplug-check.sh\" uinput \"\$XDG_RUNTIME_DIR/tilo/input-state.json\""
"$vm" test native-atspi-smoke.sh lock
"$vm" type "$TILO_PASS"
"$vm" keys ret

# What makes the disk a dev VM: tilo-vm reaches tilo-vmd as root, on this
# architecture, built on the image it names.
"$dev_dir/check-dev-vm.sh" --arch "$arch" ${base_digest:+--base-digest "$base_digest"}
# The image's apps are installed packages: Settings updates from a registry,
# launches as the new version, and rolls back.
RUNNER="$vm ssh" "$repo/os/tests/booted_app_update.sh"
# Last, because it ends this session: signed in again with every frame stalled
# 50-130 ms, as on a slow laptop, apps opened beside each other keep their
# windows, are never configured larger than the display, and never exit.
"$dev_dir/test-slow-frame-launch.sh" --out "$out/slow-frames" --stall 50-130
printf 'PASS\n' > "$out/verdict.txt"
note "the installed desktop passed"
