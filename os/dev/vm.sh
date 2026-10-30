#!/bin/bash
# Drive a Tilo qcow2 headlessly: boot, automate the greeter login, take
# screenshots, pull logs, and shut down. Designed for machines where qemu lives
# in a distrobox and the host is immutable (see os/dev/_common.sh for config).
#
#   ./os/dev/vm.sh boot [--gl] [--disk PATH] [--serial ID] [--tpm] [--secboot] [--attach PATH]
#                       [--validation-sysext PATH]
#                                              start the VM (detached); --serial gives
#                                              the boot disk a stable hardware serial
#                                              TILO_VM_LAN_PORT/_PEER/_MAC add a NIC on an
#                                              L2 segment shared with one peer VM
#                                              TILO_VM_HOSTFWD adds forwards to the NIC,
#                                              e.g. hostfwd=tcp:127.0.0.1:3341-:3340
#   ./os/dev/vm.sh boot-installer [--gl]       boot the custom installer + blank /dev/vdb
#   ./os/dev/vm.sh boot-target [--gl] [--serial-socket]
#                                              boot the installed result (target.qcow2);
#                                              --serial-socket also serves the serial
#                                              line on $TILO_RUN/serial.sock to type into
#   TILO_VM_CYCLE=NAME ./os/dev/vm.sh init-cycle
#                                             clear and initialize isolated mutable state
#
# tilo-vm builds and starts QEMU (`tilo-vm qemu`); this script drives the VM it
# started through the monitor, console, serial log and SSH in $TILO_RUN.
#
# --tpm attaches a software TPM 2.0 (swtpm), started, state persisted across boots
# so an enrolled disk can be unlock-tested and a measured boot can be observed.
# It also pins the firmware to the 4 MB OVMF build — the only one with TPM
# support; the boot is refused rather than run against a firmware that cannot
# measure (`tilo-vm qemu firmware --help`). `vm.sh firmware` prints what was selected.
#
# The variable store is PRISTINE by default: no keys, Secure Boot in setup mode,
# re-seeded every boot, which is the state that both measures and accepts an
# owner-signed image. --secboot keeps that store across boots instead, so that
# enrolling keys in the guest (`/usr/libexec/tilo-secureboot enroll`) sticks —
# enrolment is an explicit step, never something the template arrives with.
# TILO_SECBOOT_KEYS=microsoft boots the vendor-key store on purpose.
#   ./os/dev/vm.sh wait                        block until the greeter accepts input
#   ./os/dev/vm.sh firmware [--tpm] [--secboot]  which OVMF/varstore/format, and
#                                              whether it can measure
#   ./os/dev/vm.sh login [USER PASS]           type both greeter fields
#   ./os/dev/vm.sh login --passcode-only [PASS] type the installed one-user form
#   ./os/dev/vm.sh shot NAME                   framebuffer -> $TILO_RUN/NAME.png
#   ./os/dev/vm.sh type STRING                 type a string (sendkey); `type -` reads it from stdin
#   ./os/dev/vm.sh console                     print the installer/greeter console socket
#                                             (tilo-control --socket PATH actions list)
#   ./os/dev/vm.sh tool PROGRAM [ARGS...]      run a host tool where QEMU lives (distrobox)
#   ./os/dev/vm.sh test NAME [ARGS...]         run in-guest test tooling (GUEST_TESTS below,
#                                             e.g. installed-atspi-apps.sh) from this
#                                             checkout over SSH; ARGS are joined like ssh's
#   ./os/dev/vm.sh with-tests COMMAND          run COMMAND over SSH with that tooling's
#                                             directory in $TILO_GUEST_TESTS
#   ./os/dev/vm.sh keys KEY...                 send named keys (ret tab esc f2..)
#   ./os/dev/vm.sh pointer X Y                 move the absolute USB tablet in guest pixels
#   ./os/dev/vm.sh click X Y [left|right|middle]
#                                             move, press and release a pointer button
#   ./os/dev/vm.sh tap X Y                     touch and lift one finger in guest pixels
#                                             (TILO_VM_INPUT=touch)
#   ./os/dev/vm.sh wait-greeter-return         prove UI logout returned to greetd
#   ./os/dev/vm.sh shutdown                    request a clean guest shutdown over SSH
#   ./os/dev/vm.sh cmd "HMP"                   raw QEMU monitor command
#   ./os/dev/vm.sh serial [GREP]               dump the boot serial console log
#   ./os/dev/vm.sh journal [GREP]             offline journal extract (VM stopped)
#   ./os/dev/vm.sh status                       is it running?
#   ./os/dev/vm.sh stop                         power off
#   ./os/dev/vm.sh capture-boot [DIR]          boot->greeter->login->desktop demo
#   ./os/dev/vm.sh greeter-acceptance [DIR]    judge the sign-in screen's scanned-out
#                                             frames: idle, pointer, typing latency,
#                                             a wrong passcode (os/dev/greeter-acceptance.py)
#   ./os/dev/vm.sh check-frame NAME [DIR]       capture the display and require it whole
#
# DISPLAY MODES (important):
#   --2d (default): virtio VGA (x86) or virtio-gpu-pci (ARM), with no host
#       window. screendump works for firmware, the kernel console, and greeter.
#       The compositor falls back to software GL (llvmpipe) and may not start.
#   --gl: the corresponding virtio GL device with egl-headless. Gives
#       the guest hardware GL (virgl) so the compositor can run — BUT screendump
#       only captures GL scanouts, so the 2D greeter/console show "no surface".
#   There is no single headless mode that screenshots both the 2D greeter and the
#   GL compositor; pick per task. Serial logging (below) works in both.
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

mkdir -p "$TILO_RUN"

# Start one installation cycle from provably empty mutable state. The path is
# derived and validated by _common.sh; the same TILO_VM_CYCLE value must be used
# for boot-installer and boot-target so disk, TPM and firmware state persist
# across that reboot and nowhere else.
cmd_init_cycle() {
  local expected_run="${XDG_RUNTIME_DIR:-/tmp}/tilo-vm-$TILO_VM_CYCLE"
  [ -n "$TILO_VM_CYCLE" ] && [ -n "$TILO_CYCLE_DIR" ] \
    || die "set a unique TILO_VM_CYCLE before init-cycle"
  case "$TILO_CYCLE_DIR" in
    "$TILO_INSTALLER_OUTPUT"/vm-cycles/"$TILO_VM_CYCLE") ;;
    *) die "refusing to clear cycle state outside $TILO_INSTALLER_OUTPUT/vm-cycles" ;;
  esac
  [ "$TILO_RUN" = "$expected_run" ] \
    || die "refusing to clear non-cycle runtime $TILO_RUN (expected $expected_run)"
  [ ! -L "$TILO_CYCLE_DIR" ] || die "refusing to clear symlinked cycle state: $TILO_CYCLE_DIR"
  [ ! -L "$TILO_RUN" ] || die "refusing to clear symlinked cycle runtime: $TILO_RUN"
  vm_running && die "VM is still running in cycle $TILO_VM_CYCLE"
  swtpm_stop
  [ ! -e "$TILO_CYCLE_DIR" ] || rm -r "$TILO_CYCLE_DIR"
  [ ! -e "$TILO_RUN" ] || rm -r "$TILO_RUN"
  install -d -m 0700 "$TILO_CYCLE_DIR" "$TILO_RUN"
  msg "fresh VM cycle: $TILO_VM_CYCLE ($TILO_CYCLE_DIR)"
}

# map a single character to a QEMU sendkey keyname
char_key() {
  case "$1" in
    ' ') echo spc;; '-') echo minus;; '.') echo dot;; '/') echo slash;;
    '_') echo shift-minus;; ':') echo shift-semicolon;; ';') echo semicolon;;
    '@') echo shift-2;; '=') echo equal;; '+') echo shift-equal;;
    '|') echo shift-backslash;; '\') echo backslash;;
    '>') echo shift-dot;; '<') echo shift-comma;; ',') echo comma;;
    '~') echo shift-grave_accent;; '`') echo grave_accent;;
    '#') echo shift-3;; '*') echo shift-8;; '&') echo shift-7;;
    '(') echo shift-9;; ')') echo shift-0;; '!') echo shift-1;;
    '"') echo shift-apostrophe;; "'") echo apostrophe;;
    '?') echo shift-slash;;
    # A capital is its letter's key with Shift held, as on a real keyboard.
    [[:upper:]]) echo "shift-$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')";;
    *) echo "$1";;
  esac
}

vm_running() { [ -f "$TILO_PIDFILE" ] && inenv "kill -0 \$(cat '$TILO_PIDFILE') 2>/dev/null"; }

# Start the VM through tilo-vm with this toolkit's settings, the display mode
# $1 and the flags after it (disks, forwards). Every boot has the console
# channel and forwards guest SSH to TILO_SSH_PORT.
start_vm() {
  local mode="$1"; shift
  qemu_flags
  local flags=("${QEMU_FLAGS[@]}" --display "$mode" --ssh-port "$TILO_SSH_PORT" --console
    --start-timeout "$TILO_QEMU_START_SECS")
  [ -z "$TILO_VM_AUDIO_WAV" ] || flags+=(--audio-wav "$TILO_VM_AUDIO_WAV")
  tilo_vm qemu boot "${flags[@]}" "$@" >&2
}

cmd_boot() {
  local mode=2d disk="$TILO_DISK" attach="" disk_serial=""
  local validation="${TILO_VM_VALIDATION_SYSEXT:-}"
  while [ $# -gt 0 ]; do case "$1" in
    --gl) mode=gl;; --2d) mode=2d;; --window) mode=window;; --disk) disk="$2"; shift;;
    # The installer binds a destructive target to WWN or serial; a virtio disk
    # has neither unless QEMU is given one.
    --serial) disk_serial="$2"; shift;;
    --tpm) TILO_TPM=1;; --no-tpm) TILO_TPM=0;;
    --secboot) TILO_SECBOOT=1;;
    # Attach a SECOND virtio disk (/dev/vdb), creating it blank if absent. Lets a
    # booted image install onto a fresh disk (encryption tests) without building
    # the installer medium.
    --attach) attach="$2"; shift;;
    # The release gate's signed validation extension (os/dev/validation-access.sh),
    # read-only under the serial its unit in the disk copy's /etc binds to.
    --validation-sysext) validation="$2"; shift;;
    *) die "boot: unknown arg $1";; esac; shift; done
  [ -f "$disk" ] || die "disk not found: $disk (run os/dev/build-image.sh)"
  [[ "$disk_serial" =~ ^[A-Za-z0-9._-]{0,20}$ ]] || die "boot: --serial must be up to 20 of [A-Za-z0-9._-]"
  local disks=(--disk "$disk,format=qcow2,id=tilo-disk${disk_serial:+,serial=$disk_serial}")
  if [ -n "$attach" ]; then
    disks+=(--disk "$attach,format=qcow2,id=tilo-install-target,serial=tilo-install-target,create=$TILO_TARGET_SIZE")
    msg "attaching $attach as /dev/vdb"
  fi
  if [ -n "$validation" ]; then
    [ -f "$validation" ] || die "validation extension not found: $validation"
    disks+=(--disk "$validation,format=raw,id=tilo-validation,serial=tilo-validation,readonly")
    msg "attaching the validation extension $validation (serial tilo-validation)"
  fi
  # tilo-vm supplies only its registered public key, never a password or
  # private key, as the same systemd OEM credential as the libvirt adapter.
  local extra=()
  [ -z "${TILO_VM_ROOT_KEY_FILE:-}" ] || extra+=(--root-key "$TILO_VM_ROOT_KEY_FILE")
  if [ -n "$TILO_VM_LAN_PORT$TILO_VM_LAN_PEER$TILO_VM_LAN_MAC" ]; then
    extra+=(--lan "$TILO_VM_LAN_PORT:$TILO_VM_LAN_PEER:$TILO_VM_LAN_MAC" --lan-model "$TILO_VM_LAN_MODEL")
  fi
  # TILO_VM_HOSTFWD: more forwards, as QEMU's comma-separated hostfwd=... list.
  local forward
  if [ -n "${TILO_VM_HOSTFWD:-}" ]; then
    local forwards
    IFS=, read -ra forwards <<<"$TILO_VM_HOSTFWD"
    for forward in "${forwards[@]}"; do extra+=(--forward "${forward#hostfwd=}"); done
  fi
  msg "boot ($mode, $TILO_VM_ARCH, ${TILO_XRES}x${TILO_YRES}) disk=$disk"
  start_vm "$mode" --name tilo "${disks[@]}" "${extra[@]}" || die "boot failed"
  msg "up. monitor=$TILO_MON serial=$TILO_SERIAL"
}

# Boot the CUSTOM installer live qcow2 (build-custom-installer.sh) with a blank
# target disk attached as a SECOND virtio disk (/dev/vdb). The installer
# self-installs the booted image onto /dev/vdb. After it finishes, boot the
# result with:  vm.sh boot-target [--gl]
cmd_boot_installer() {
  local mode=2d install_proof=0
  while [ $# -gt 0 ]; do case "$1" in
    --gl) mode=gl;; --2d) mode=2d;; --window) mode=window;;
    --tpm) TILO_TPM=1;; --no-tpm) TILO_TPM=0;;
    --secboot) TILO_SECBOOT=1;;
    --install-proof) install_proof=1;;
    *) die "boot-installer: unknown arg $1";; esac; shift; done
  local disk="$TILO_INSTALLER_DISK"
  [ -f "$disk" ] || die "installer disk not found: $disk (run ./os/dev/build-custom-installer.sh)"
  # The installer runs from a writable overlay of the medium, so a test never
  # mutates the release artifact it validates. First virtio disk = installer
  # (/dev/vda); second = blank target (/dev/vdb).
  local work_disk="$TILO_INSTALLER_WORK_DISK" target="$TILO_TARGET_DISK"
  local flags=(
    --disk "$work_disk,format=qcow2,backing=$disk,id=tilo-installer-medium,serial=tilo-installer-medium"
    --disk "$target,format=qcow2,create=$TILO_TARGET_SIZE,id=tilo-install-target,serial=tilo-install-target")
  if [ "$install_proof" -eq 1 ]; then
    [ ! -L "$TILO_RUN/install-proof.txt" ] || die "install proof path is a symlink"
    rm -f -- "$TILO_RUN/install-proof.txt"
    flags+=(--virtio-port "org.tilo.vm-install-proof=$TILO_RUN/install-proof.txt")
  fi
  msg "boot-installer ($mode, ${TILO_XRES}x${TILO_YRES}) medium=$disk work-overlay=$work_disk target=$target (/dev/vdb)"
  start_vm "$mode" --name tilo-custom-installer "${flags[@]}" || die "boot-installer failed"
  msg "installer up. monitor=$TILO_MON serial=$TILO_SERIAL  (installs onto $target as /dev/vdb)"
}

# Boot the installed system — the target disk the custom installer wrote
# (default $TILO_RUN/target.qcow2). This is the post-install result.
cmd_boot_target() {
  local mode=2d disk="$TILO_TARGET_DISK" serial=()
  while [ $# -gt 0 ]; do case "$1" in
    --gl) mode=gl;; --2d) mode=2d;; --window) mode=window;; --disk) disk="$2"; shift;;
    --tpm) TILO_TPM=1;; --no-tpm) TILO_TPM=0;;
    --secboot) TILO_SECBOOT=1;;
    --serial-socket) serial=(--serial-socket);;
    *) die "boot-target: unknown arg $1";; esac; shift; done
  [ -f "$disk" ] || die "target disk not found: $disk (run vm.sh boot-installer and install first)"
  msg "boot-target ($mode, ${TILO_XRES}x${TILO_YRES}) disk=$disk (installed system)"
  if [ "$mode" = 2d ]; then
    warn "2D proves the boot/unlock path, but the non-GL virtio display exposes no virgl GPU to the Tilo compositor."
    warn "The framebuffer can go black after Plymouth even when greetd and the session are healthy; use --gl to validate the desktop."
  fi
  start_vm "$mode" --name tilo-target "${serial[@]}" \
    --disk "$disk,format=qcow2,id=tilo-install-target,serial=tilo-install-target" \
    || die "boot-target failed"
  msg "target up. monitor=$TILO_MON serial=$TILO_SERIAL"
}

# greetd starting is not the greeter being usable: greetd then starts cage,
# cage the greeter, and only once the greeter's window is mapped and focused do
# typed keys reach it. Keys sent earlier are dropped, so the barrier is the
# greeter's own report over its console that sign-in accepts input.
greeter_accepts_input() {
  python3 "$TILO_OS_DIR/dev/installer-console.py" --socket "$TILO_CONSOLE" \
    check greeter /accepts_input --timeout "${1:-5}" 2>/dev/null
}

# The live installer medium's greetd runs the installer instead, which is
# driven through its console actions.
installer_answers() {
  python3 "$TILO_OS_DIR/dev/installer-console.py" --socket "$TILO_CONSOLE" \
    check installer --timeout "${1:-5}" 2>/dev/null
}

cmd_wait() {
  msg "waiting for greeter (greetd), at most ${TILO_BOOT_WAIT_SECS}s ..."
  local i=0 early_reported=0 greetd=0 start=$SECONDS
  local deadline=$((SECONDS + TILO_BOOT_WAIT_SECS))
  while :; do
    i=$((i + 1))
    # The console is authoritative, including on a quiet production boot with
    # no development SSH service. Neither a serial log nor SSH is a prerequisite
    # for observing the actual application's readiness.
    if greeter_accepts_input 1; then
      msg "greeter accepts input (~$((SECONDS - start))s)"; return 0
    elif installer_answers 1; then
      msg "installer up (~$((SECONDS - start))s)"; return 0
    elif [ "$greetd" = 0 ] && tr -d '\000' < "$TILO_SERIAL" 2>/dev/null | grep -aq 'Started.*greetd.service\|session-c1.scope'; then
      msg "greetd up (~$((SECONDS - start))s); waiting for the greeter to accept input"; greetd=1; continue
    # A guest may reach greetd without writing kernel/userspace status to the
    # selected serial port. The installed validation disk supplies SSH access;
    # use it as an independent readiness signal when available.
    elif [ "$greetd" = 0 ] && (( i % 5 == 0 )) && command -v sshpass >/dev/null 2>&1 \
       && [ -n "${TILO_USER:-}" ] && [ -n "${TILO_PASS:-}" ] \
       && TILO_SSH_BOUND=15 gssh 'systemctl is-active --quiet greetd.service' >/dev/null 2>&1; then
      msg "greetd up via SSH (~$((SECONDS - start))s); waiting for the greeter to accept input"; greetd=1; continue
    fi
    if [ "$early_reported" = 0 ] && grep -aq 'TILO_VALIDATION_DIAGNOSTICS_END early' "$TILO_SERIAL" 2>/dev/null; then
      warn "validation-only guest service diagnostics:"
      tr -d '\000' < "$TILO_SERIAL" \
        | sed -n '/TILO_VALIDATION_DIAGNOSTICS early/,/TILO_VALIDATION_DIAGNOSTICS_END early/p' \
        | tail -n 160 >&2
      early_reported=1
    fi
    vm_running || { boot_diagnose "$TILO_SERIAL" || true
      # A qemu that exits this early usually said why on stderr, which is
      # all that reaches its log; CI keeps nothing else of the run dir.
      if [ -s "$TILO_QEMU_LOG" ]; then
        warn "last lines of $TILO_QEMU_LOG:"; tail -n 40 "$TILO_QEMU_LOG" >&2
      fi
      die "qemu exited during boot; see $TILO_QEMU_LOG / $TILO_SERIAL"; }
    [ "$SECONDS" -lt "$deadline" ] || break
    sleep 2
  done
  warn "no greeter accepted input within ${TILO_BOOT_WAIT_SECS}s of booting"
  if command -v sshpass >/dev/null 2>&1 \
     && [ -n "${TILO_USER:-}" ] && [ -n "${TILO_PASS:-}" ]; then
    local ssh_diagnostic
    ssh_diagnostic="$(TILO_SSH_BOUND=30 gssh 'systemctl status greetd.service tilo-validation-ssh.service --no-pager --lines=20; systemctl --failed --no-pager; systemctl list-jobs --no-pager; journalctl -b --no-pager -o short-monotonic -n 80' 2>&1)" \
      || warn "validation SSH diagnostic probe failed"
    if [ -n "$ssh_diagnostic" ]; then
      warn "validation SSH diagnostic output:"
      printf '%s\n' "$ssh_diagnostic" | tail -n 200 >&2
    fi
  fi
  if [ "$greetd" = 1 ]; then
    warn "greetd is up, but the greeter never reported that sign-in accepts input; its console says:"
    python3 "$TILO_OS_DIR/dev/installer-console.py" --socket "$TILO_CONSOLE" \
      check greeter /accepts_input --timeout 5 >&2 || true
  fi
  boot_diagnose "$TILO_SERIAL" || true
  if [ -s "$TILO_SERIAL" ]; then
    warn "last lines of $TILO_SERIAL:"
    tr -d '\000' < "$TILO_SERIAL" | tail -n 120 >&2
  else
    warn "the serial log is empty: $TILO_SERIAL"
  fi
  if [ -s "$TILO_QEMU_LOG" ]; then
    warn "last lines of $TILO_QEMU_LOG:"
    tail -n 40 "$TILO_QEMU_LOG" >&2
  fi
  die "timed out waiting for greeter (see $TILO_SERIAL)"
}

# Print the firmware this configuration resolves to, and why. The selection
# decides whether anything can be measured at all, so it has to be inspectable
# without booting.
cmd_firmware() {
  while [ $# -gt 0 ]; do case "$1" in
    --tpm) TILO_TPM=1;; --no-tpm) TILO_TPM=0;; --secboot) TILO_SECBOOT=1;;
    *) die "firmware: unknown arg $1";; esac; shift; done
  qemu_flags
  tilo_vm qemu firmware "${QEMU_FLAGS[@]}"
}

cmd_shot() {
  local name="${1:?usage: shot NAME}"
  local out="$TILO_RUN/$name"
  local reply; reply="$(hmp "screendump $out.ppm" 2>&1 || true)"
  if printf '%s' "$reply" | grep -qi 'no surface'; then
    warn "no surface — in GL mode the 2D console isn't capturable; in 2D mode nothing is drawing yet"
    return 1
  fi
  sleep 0.4
  inenv "test -f '$out.ppm'" || { warn "no framebuffer produced"; return 1; }
  python3 "$TILO_OS_DIR/dev/greeter-acceptance.py" --convert "$out.ppm" --png "$out.png"
  echo "$out.png"
}

cmd_type() {
  local s="$*" i c
  # `type -` reads the text from stdin, so a secret typed at a boot prompt
  # never appears in a process list or a caller's logged command line.
  if [ "$s" = "-" ]; then IFS= read -r s || true; fi
  # QEMU holds a sendkey for 100 ms by default. Sending the next character
  # sooner makes adjacent keys overlap and the graphical greeter drops input;
  # use an explicit short hold and leave a small release gap.
  for ((i=0; i<${#s}; i++)); do
    c="${s:$i:1}"
    hmp "sendkey $(char_key "$c") 20" >/dev/null
    # The installer is intentionally software-rendered in the full-cycle gate;
    # under load its event loop can miss the next key if the release gap is only
    # 40 ms.  An 80 ms gap remains quick for credentials while making injected
    # input match a human keyboard reliably.
    sleep 0.08
  done
}

cmd_keys() { local k; for k in "$@"; do hmp "sendkey $k" >/dev/null; sleep 0.12; done; }

# QEMU's USB tablet reports absolute coordinates in the inclusive 0..32767
# range. Accept guest logical pixels so an acceptance script can use the same
# coordinates as screenshots and the compositor's configured mode.
cmd_pointer() {
  local x="${1:?usage: pointer X Y}" y="${2:?usage: pointer X Y}"
  [ "$TILO_XRES" -gt 1 ] && [ "$TILO_YRES" -gt 1 ] \
    || die "pointer injection requires a guest mode larger than 1x1"
  [[ "$x" =~ ^[0-9]+$ && "$y" =~ ^[0-9]+$ ]] \
    || die "pointer coordinates must be non-negative integers"
  [ "$x" -lt "$TILO_XRES" ] && [ "$y" -lt "$TILO_YRES" ] \
    || die "pointer coordinate ($x,$y) is outside ${TILO_XRES}x${TILO_YRES}"
  local qx=$((x * 32767 / (TILO_XRES - 1)))
  local qy=$((y * 32767 / (TILO_YRES - 1)))
  # Through QMP as absolute axes: HMP's mouse_move queues relative motion,
  # which the absolute tablet never sees.
  qmp '{"execute":"input-send-event","arguments":{"events":[
    {"type":"abs","data":{"axis":"x","value":'"$qx"'}},
    {"type":"abs","data":{"axis":"y","value":'"$qy"'}}]}}' \
    || die "pointer: QEMU refused the tablet motion"
}

cmd_click() {
  local x="${1:?usage: click X Y [left|right|middle]}"
  local y="${2:?usage: click X Y [left|right|middle]}"
  local button="${3:-left}"
  case "$button" in
    left|right|middle) ;;
    *) die "click button must be left, right, or middle";;
  esac
  cmd_pointer "$x" "$y"
  qmp '{"execute":"input-send-event","arguments":{"events":[
    {"type":"btn","data":{"down":true,"button":"'"$button"'"}}]}}' \
    || die "click: QEMU refused the button press"
  sleep 0.08
  qmp '{"execute":"input-send-event","arguments":{"events":[
    {"type":"btn","data":{"down":false,"button":"'"$button"'"}}]}}' \
    || die "click: QEMU refused the button release"
}

# One finger on the multitouch screen, which only QMP can drive. Its axes use
# the same 0..32767 range as the tablet.
cmd_tap() {
  local x="${1:?usage: tap X Y}" y="${2:?usage: tap X Y}"
  [ "$TILO_VM_INPUT" = touch ] || die "tap needs the touch screen (TILO_VM_INPUT=touch)"
  [[ "$x" =~ ^[0-9]+$ && "$y" =~ ^[0-9]+$ ]] \
    || die "tap coordinates must be non-negative integers"
  [ "$x" -lt "$TILO_XRES" ] && [ "$y" -lt "$TILO_YRES" ] \
    || die "tap coordinate ($x,$y) is outside ${TILO_XRES}x${TILO_YRES}"
  local qx=$((x * 32767 / (TILO_XRES - 1))) qy=$((y * 32767 / (TILO_YRES - 1)))
  # QEMU passes the tracking id on as given: the finger lifts with -1.
  local finger='"slot":0,"tracking-id":1'
  qmp '{"execute":"input-send-event","arguments":{"events":[
    {"type":"mtt","data":{"type":"begin",'"$finger"',"axis":"x","value":'"$qx"'}},
    {"type":"mtt","data":{"type":"data",'"$finger"',"axis":"x","value":'"$qx"'}},
    {"type":"mtt","data":{"type":"data",'"$finger"',"axis":"y","value":'"$qy"'}},
    {"type":"btn","data":{"down":true,"button":"touch"}}]}}'
  sleep 0.08
  qmp '{"execute":"input-send-event","arguments":{"events":[
    {"type":"mtt","data":{"type":"end","slot":0,"tracking-id":-1,"axis":"x","value":'"$qx"'}},
    {"type":"btn","data":{"down":false,"button":"touch"}}]}}'
}

# The greeter answers its console once its event loop runs; typed keys still
# only reach the form once the compositor maps and focuses the window. Prefer
# the greeter's own accepts_input report over a blank-screen heuristic.
greeter_console() {
  python3 "$(dirname "${BASH_SOURCE[0]}")/installer-console.py" --socket "$TILO_CONSOLE" "$@"
}

wait_greeter_input() {
  greeter_console wait greeter --timeout 180 \
    || die "tilo-greeter never answered on its console ($TILO_CONSOLE)"
  local attempts="${TILO_GREETER_INPUT_ATTEMPTS:-120}" i
  for i in $(seq 1 "$attempts"); do
    greeter_accepts_input && return 0
    vm_running || die "qemu exited before the greeter accepted input"
    sleep 1
  done
  greeter_console check greeter /accepts_input --timeout 5 >&2 || true
  die "the greeter did not accept input; nothing was typed"
}

# After Enter the greeter either leaves sign-in or exits into the session.
check_login_submitted() {
  local i phase
  for i in $(seq 1 30); do
    phase="$(greeter_console state 2>/dev/null \
      | python3 -c 'import json,sys; print(json.load(sys.stdin).get("phase", ""))' 2>/dev/null)" \
      || phase=""
    [ "$phase" = sign-in ] || return 0
    sleep 1
  done
  # A session that dies at once sends greetd back to the greeter. Its own
  # log, in the user's state directory, is the only record of why.
  gssh 'f="$HOME/.local/state/tilo/session-logs/latest.log"; [ -f "$f" ] && { echo "--- $f"; tail -n 80 "$f"; }; journalctl -b --no-pager -o short-monotonic -n 40 _UID="$(id -u)"' >&2 2>/dev/null || true
  die "tilo-greeter is still at sign-in 30 s after the credentials were sent"
}

cmd_login() {
  local passcode_only=0
  if [ "${1:-}" = "--passcode-only" ]; then passcode_only=1; shift; fi
  local u="${1:-$TILO_USER}" p="${2:-$TILO_PASS}"
  wait_greeter_input
  if [ "$passcode_only" -eq 1 ]; then
    p="${1:-$TILO_PASS}"
    msg "logging in with the installed user's passcode"
    local i
    for i in $(seq 1 64); do hmp "sendkey backspace 20" >/dev/null; sleep 0.03; done
    cmd_type "$p"
    cmd_keys ret
    msg "passcode sent"
    check_login_submitted
    return
  fi
  msg "logging in as $u"
  # tilo-greeter is a single form: Enter submits the whole form, it does not
  # advance from username to password. The old tuigreet sequence pressed Enter
  # between fields, which repeatedly submitted an empty password and never
  # started the desktop. Follow the widget's actual focus order instead.
  local i
  for i in $(seq 1 64); do hmp "sendkey backspace 20" >/dev/null; sleep 0.03; done
  cmd_type "$u"
  cmd_keys tab
  for i in $(seq 1 64); do hmp "sendkey backspace 20" >/dev/null; sleep 0.03; done
  cmd_type "$p"
  cmd_keys ret
  msg "credentials sent"
  check_login_submitted
}

# The sign-in screen as a person meets it, judged from what QEMU scans out.
# Leaves the form focused on an empty passcode field, ready for `login`.
cmd_greeter_acceptance() {
  local dir="${1:-$TILO_RUN/greeter}"
  wait_greeter_input
  mkdir -p "$dir"
  python3 "$TILO_OS_DIR/dev/greeter-acceptance.py" --monitor "$TILO_MON" \
    --console "$TILO_CONSOLE" --out "$dir" --width "$TILO_XRES" --height "$TILO_YRES"
}

# In-guest test tooling the image does not ship, as checkout path and the name
# it runs under in the guest.
GUEST_TESTS=(
  "tools/atspi_external_client.py atspi-external-client.py"
  "os/tests/installed_atspi_apps.sh installed-atspi-apps.sh"
  "crates/session/shell/tests/native_atspi_smoke.sh native-atspi-smoke.sh"
  "apps/file-explorer/tests/atspi_smoke.sh files-atspi-smoke.sh"
  "os/tests/collaborative-files-image-contract.sh collaborative-files-image-contract.sh"
  "os/tests/boot_performance_gate.sh boot-performance-gate.sh"
  "os/tests/h264-boot-check.py h264-boot-check.py"
  "os/tests/input-hotplug-check.sh input-hotplug-check.sh"
  "os/tests/installed-admin-actions.sh installed-admin-actions.sh"
  "os/tests/projection-boot-check.sh projection-boot-check.sh"
  "os/tests/live-update-check.sh live-update-check.sh"
)

# Each call copies GUEST_TESTS from this checkout into a private directory in
# the guest, runs COMMAND with TILO_GUEST_TESTS naming it (and
# TILO_ATSPI_CLIENT the client in it), and removes it. The files travel on
# stdin, a name line then a base64 line each, so COMMAND gets no stdin;
# concurrent calls each get their own copy.
cmd_with_tests() {
  [ "$#" -gt 0 ] || die "usage: with-tests COMMAND"
  local entry
  for entry in "${GUEST_TESTS[@]}"; do
    [ -f "$TILO_REPO/${entry% *}" ] || die "guest test tooling is missing: ${entry% *}"
  done
  for entry in "${GUEST_TESTS[@]}"; do
    printf '%s\n' "${entry#* }"
    base64 -w0 "$TILO_REPO/${entry% *}"
    printf '\n'
  done | gssh "d=\$(mktemp -d) || exit 1; trap 'rm -rf \"\$d\"' EXIT;" \
    "while IFS= read -r n && IFS= read -r b; do" \
    "printf %s \"\$b\" | base64 -d > \"\$d/\$n\" || exit 1; done;" \
    "chmod 0700 \"\$d\"/* || exit 1;" \
    "export TILO_GUEST_TESTS=\"\$d\" TILO_ATSPI_CLIENT=\"\$d/atspi-external-client.py\";" \
    "$*"
}

# One capture of whatever is on screen now, saved as NAME.png and required to
# be fully painted.
cmd_check_frame() {
  local name="${1:?usage: check-frame NAME [DIR]}" dir="${2:-$TILO_RUN/greeter}"
  mkdir -p "$dir"
  local ppm="$dir/$name.ppm" reply
  rm -f "$ppm"
  reply="$(hmp "screendump $ppm" 2>&1 || true)"
  if printf '%s' "$reply" | grep -qi 'no surface'; then
    die "QEMU has no 2D surface to capture for $name"
  fi
  local i
  for i in $(seq 1 50); do [ -s "$ppm" ] && break; sleep 0.1; done
  python3 "$TILO_OS_DIR/dev/greeter-acceptance.py" --check-image "$ppm" --png "$dir/$name.png"
}

cmd_serial() {
  [ -f "$TILO_SERIAL" ] || die "no serial log yet ($TILO_SERIAL)"
  if [ -n "${1:-}" ]; then tr -d '\000' < "$TILO_SERIAL" | sed 's/\r//g' | grep -aiE "$1"; else
    tr -d '\000' < "$TILO_SERIAL" | sed 's/\r//g'; fi
}

cmd_status() { if vm_running; then echo "running (pid $(cat "$TILO_PIDFILE"))"; else echo "stopped"; fi; }

# Evidence barrier for a logout driven through Tilo's visible power menu. The
# compositor must be gone and greetd must own a fresh login session; checking
# only one side can mistake a crashed desktop or a still-running shell for a
# successful logout. This intentionally performs no logout action itself.
cmd_wait_greeter_return() {
  local attempts="${TILO_GREETER_RETURN_ATTEMPTS:-120}" i
  msg "waiting for Tilo logout to return to the greeter ..."
  for i in $(seq 1 "$attempts"); do
    # The compositor's portal backend name, not its /proc entry, which turns
    # non-dumpable once it holds identity keys.
    if gssh "! busctl --user status org.freedesktop.impl.portal.desktop.tilo >/dev/null 2>&1 && systemctl is-active --quiet greetd.service" >/dev/null 2>&1; then
      msg "greeter returned after logout (~$i s)"
      return 0
    fi
    vm_running || die "qemu exited while waiting for the greeter to return"
    sleep 1
  done
  cmd_serial 'greetd|shell|session-c[0-9]+.scope' | tail -120 >&2 || true
  die "timed out waiting for logout to stop Tilo and return to greetd"
}

cmd_shutdown() {
  msg "requesting clean guest shutdown"
  gssh "printf '%s\\n' '$TILO_PASS' | sudo -S -p '' systemctl poweroff" >/dev/null 2>&1 || true
  local attempts="${TILO_SHUTDOWN_ATTEMPTS:-60}" i
  for i in $(seq 1 "$attempts"); do
    if ! vm_running; then
      rm -f "$TILO_PIDFILE"
      swtpm_stop
      msg "guest powered off cleanly (~$i s)"
      return 0
    fi
    sleep 1
  done
  die "guest did not power off within ${attempts}s; VM left running for diagnosis"
}

cmd_stop() {
  tilo_vm qemu stop --run "$TILO_RUN" --tpm-dir "$TILO_TPM_DIR" >/dev/null
  msg "stopped"
}

# Offline journal extraction via qemu-nbd. The VM must be stopped. General
# method (no in-image instrumentation), but needs passwordless sudo + the nbd
# kernel module. Works only if the guest has a *persistent* journal
# (/var/log/journal); otherwise use serial logging + StandardError=console.
cmd_journal() {
  vm_running && die "stop the VM first (journal extraction needs the disk offline)"
  local grep_pat="${1:-}" mnt="$TILO_RUN/mnt"
  msg "attaching $TILO_DISK via qemu-nbd"
  sudo modprobe nbd max_part=8
  inenv "sudo qemu-nbd --read-only --connect=/dev/nbd0 '$TILO_DISK'"
  sudo partprobe /dev/nbd0 2>/dev/null || true; sleep 1
  mkdir -p "$mnt"
  local part rootpart=""
  for part in /dev/nbd0p4 /dev/nbd0p3 /dev/nbd0p2; do
    if sudo blkid "$part" 2>/dev/null | grep -q 'TYPE="btrfs"'; then rootpart="$part"; break; fi
  done
  [ -n "$rootpart" ] || { inenv "sudo qemu-nbd -d /dev/nbd0"; die "no btrfs root partition found on disk"; }
  sudo mount -o ro "$rootpart" "$mnt"
  local jdir; jdir="$(sudo find "$mnt" -maxdepth 6 -type d -path '*/log/journal' 2>/dev/null | head -1)"
  if [ -n "$jdir" ]; then
    msg "journal: $jdir"
    if [ -n "$grep_pat" ]; then sudo journalctl -D "$jdir" --no-pager 2>/dev/null | grep -iE "$grep_pat";
    else sudo journalctl -D "$jdir" --no-pager 2>/dev/null; fi
  else
    warn "no persistent journal on disk (journal lives in tmpfs at runtime)."
    warn "Use serial logging instead: add StandardError=journal+console to the unit, or boot and use 'vm.sh serial'."
  fi
  sudo umount "$mnt" 2>/dev/null || true
  inenv "sudo qemu-nbd -d /dev/nbd0" 2>/dev/null || true
}

# One-shot demo: boot 2D, screenshot the greeter, log in, screenshot the result.
cmd_capture_boot() {
  local dir="${1:-$TILO_RUN/capture}"; mkdir -p "$dir"
  TILO_RUN_SAVE="$TILO_RUN"
  cmd_boot --2d
  cmd_wait
  sleep 3
  msg "greeter screenshot"; local g; g="$(cmd_shot greeter || true)"; [ -n "${g:-}" ] && cp -f "$g" "$dir/01-greeter.png"
  cmd_login
  sleep 8
  msg "post-login screenshot"; local d; d="$(cmd_shot after-login || true)"; [ -n "${d:-}" ] && cp -f "$d" "$dir/02-after-login.png"
  cmd_serial 'Reached target Graphical|greetd|PAM:authentication.*res=|shell' > "$dir/boot-summary.txt" 2>/dev/null || true
  msg "artifacts in $dir:"; ls -lh "$dir" >&2
}

sub="${1:-}"; shift || true
case "$sub" in
  init-cycle) cmd_init_cycle "$@";;
  boot) cmd_boot "$@";;
  boot-installer) cmd_boot_installer "$@";;
  boot-target) cmd_boot_target "$@";;
  wait) cmd_wait "$@";;
  firmware) cmd_firmware "$@";;
  login) cmd_login "$@";;
  shot) cmd_shot "$@";;
  type) cmd_type "$@";;
  keys) cmd_keys "$@";;
  pointer) cmd_pointer "$@";;
  click) cmd_click "$@";;
  tap) cmd_tap "$@";;
  wait-greeter-return) cmd_wait_greeter_return "$@";;
  shutdown) cmd_shutdown "$@";;
  cmd) hmp "$*";;
  # The host end of the installer/greeter console channel (tilo-control --socket).
  console) printf '%s\n' "$TILO_CONSOLE";;
  # Run one host tool (qemu-img, sfdisk, mkfs.*) where the VM tools live,
  # stdin passed through: in the distrobox when TILO_DISTROBOX is set.
  tool) [ "$#" -gt 0 ] || die "usage: tool PROGRAM [ARGS...]"; inenv "$(printf '%q ' "$@")";;
  ssh) gssh "$@";;
  test) [ "$#" -gt 0 ] || die "usage: test NAME [ARGS...]"
    name="$1"; shift; cmd_with_tests "\"\$TILO_GUEST_TESTS/$name\"" "$@";;
  with-tests) cmd_with_tests "$@";;
  serial) cmd_serial "$@";;
  journal) cmd_journal "$@";;
  status) cmd_status;;
  stop) cmd_stop;;
  capture-boot) cmd_capture_boot "$@";;
  greeter-acceptance) cmd_greeter_acceptance "$@";;
  check-frame) cmd_check_frame "$@";;
  -h|--help|"") sed -n '2,61p' "$0";;
  *) die "unknown subcommand: $sub (try --help)";;
esac
