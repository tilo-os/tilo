#!/bin/bash
# Drive a Tilo qcow2 headlessly: boot, automate the greeter login, take
# screenshots, pull logs, and shut down. Designed for machines where qemu lives
# in a distrobox and the host is immutable (see os/dev/_common.sh for config).
#
#   ./os/dev/vm.sh boot [--gl] [--disk PATH] [--serial ID] [--tpm] [--secboot] [--attach PATH]
#                       [--validation-sysext PATH]
#                                              start the VM (detached); --serial gives
#                                              the boot disk a stable hardware serial
#   ./os/dev/vm.sh boot-installer [--gl]       boot the custom installer + blank /dev/vdb
#   ./os/dev/vm.sh boot-target [--gl]          boot the installed result (target.qcow2)
#   TILO_VM_CYCLE=NAME ./os/dev/vm.sh init-cycle
#                                             clear and initialize isolated mutable state
#
# --tpm attaches a software TPM 2.0 (swtpm), started, state persisted across boots
# so an enrolled disk can be unlock-tested and a measured boot can be observed.
# It also pins the firmware to the 4 MB OVMF build — the only one with TPM
# support; the boot is refused rather than run against a firmware that cannot
# measure (see os/dev/_common.sh). `vm.sh firmware` prints what was selected.
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
#   ./os/dev/vm.sh keys KEY...                 send named keys (ret tab esc f2..)
#   ./os/dev/vm.sh pointer X Y                 move the absolute USB tablet in guest pixels
#   ./os/dev/vm.sh click X Y [left|right|middle]
#                                             move, press and release a pointer button
#   ./os/dev/vm.sh wait-greeter-return         prove UI logout returned to greetd
#   ./os/dev/vm.sh shutdown                    request a clean guest shutdown over SSH
#   ./os/dev/vm.sh cmd "HMP"                   raw QEMU monitor command
#   ./os/dev/vm.sh serial [GREP]               dump the boot serial console log
#   ./os/dev/vm.sh journal [GREP]             offline journal extract (VM stopped)
#   ./os/dev/vm.sh status                       is it running?
#   ./os/dev/vm.sh stop                         power off
#   ./os/dev/vm.sh capture-boot [DIR]          boot->greeter->login->desktop demo
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
    *) echo "$1";;
  esac
}

vm_running() { [ -f "$TILO_PIDFILE" ] && inenv "kill -0 \$(cat '$TILO_PIDFILE') 2>/dev/null"; }

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
  vm_running && die "VM already running (pid $(cat "$TILO_PIDFILE")); stop it first"
  [[ "$disk_serial" =~ ^[A-Za-z0-9._-]{0,20}$ ]] || die "boot: --serial must be up to 20 of [A-Za-z0-9._-]"
  local serial_arg=""
  [ -z "$disk_serial" ] || serial_arg=",serial=$disk_serial"
  local attach_arg=""
  if [ -n "$attach" ]; then
    inenv "test -f '$attach' || qemu-img create -f qcow2 '$attach' '$TILO_TARGET_SIZE'" >/dev/null
    attach_arg="-drive file=$attach,format=qcow2,if=virtio,serial=tilo-install-target"
    msg "attaching $attach as /dev/vdb"
  fi
  if [ -n "$validation" ]; then
    [ -f "$validation" ] || die "validation extension not found: $validation"
    # QEMU's raw block backend does not accept serial= on -drive. The serial
    # belongs to the virtio block device, which supplies the guest's by-id path.
    attach_arg="$attach_arg -drive file=$validation,format=raw,if=none,id=tilo-validation,readonly=on -device virtio-blk-pci,drive=tilo-validation,serial=tilo-validation"
    msg "attaching the validation extension $validation (serial tilo-validation)"
  fi

  local varstore display
  firmware_setup
  varstore="$(firmware_varstore)"
  rm -f "$TILO_SERIAL" "$TILO_MON" "$TILO_PIDFILE" "$TILO_CONSOLE"
  display="$(display_qemu_args "$mode")"
  msg "boot ($mode, $TILO_VM_ARCH, ${TILO_XRES}x${TILO_YRES}) disk=$disk"

  local args="$(machine_args) $(qemu_accel_args) -m $TILO_MEM -smp $TILO_SMP \
    $(pflash_args "$varstore") \
    -drive file=$disk,format=qcow2,if=virtio$serial_arg $attach_arg \
    $display -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:$TILO_SSH_PORT-:22 $(input_qemu_args) \
    $(console_qemu_args) \
    -monitor unix:$TILO_MON,server,nowait -serial file:$TILO_SERIAL \
    -pidfile $TILO_PIDFILE -name tilo"
  swtpm_start
  start_qemu "$args $(tpm_qemu_args) $(secboot_qemu_args)" \
    || { warn "monitor socket not up; qemu.log:"; cat "$TILO_QEMU_LOG" >&2; die "boot failed"; }
  msg "up. monitor=$TILO_MON serial=$TILO_SERIAL"
}

# Boot the installer ISO (CD-ROM) against a fresh blank target disk, so Anaconda
# can install Tilo onto it. After the install completes + reboots, boot the
# installed system with:  vm.sh boot --gl --disk $TILO_RUN/target.qcow2

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
  vm_running && die "VM already running; stop it first"
  local work_disk="$TILO_INSTALLER_WORK_DISK"
  if [ ! -f "$work_disk" ]; then
    inenv "qemu-img create -f qcow2 -F qcow2 -b '$disk' '$work_disk'" >/dev/null
  fi
  local varstore display
  firmware_setup
  varstore="$(firmware_varstore)"
  rm -f "$TILO_SERIAL" "$TILO_MON" "$TILO_PIDFILE" "$TILO_CONSOLE"
  local target="$TILO_TARGET_DISK"
  inenv "test -f '$target' || qemu-img create -f qcow2 '$target' '$TILO_TARGET_SIZE'" >/dev/null
  display="$(display_qemu_args "$mode")"
  local proof_arg=""
  if [ "$install_proof" -eq 1 ]; then
    [ ! -L "$TILO_RUN/install-proof.txt" ] || die "install proof path is a symlink"
    rm -f -- "$TILO_RUN/install-proof.txt"
    proof_arg="-chardev file,id=tiloproof,path=$TILO_RUN/install-proof.txt -device virtserialport,chardev=tiloproof,name=org.tilo.vm-install-proof"
  fi
  msg "boot-installer ($mode, ${TILO_XRES}x${TILO_YRES}) medium=$disk work-overlay=$work_disk target=$target (/dev/vdb)"
  # First virtio disk = installer (/dev/vda); second = blank target (/dev/vdb).
  local args="$(machine_args) $(qemu_accel_args) -m $TILO_MEM -smp $TILO_SMP \
    $(pflash_args "$varstore") \
    -drive file=$work_disk,format=qcow2,if=virtio,serial=tilo-installer-medium \
    -drive file=$target,format=qcow2,if=virtio,serial=tilo-install-target \
    $display -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:$TILO_SSH_PORT-:22 $(input_qemu_args) \
    $(console_qemu_args) $proof_arg \
    -monitor unix:$TILO_MON,server,nowait -serial file:$TILO_SERIAL \
    -pidfile $TILO_PIDFILE -name tilo-custom-installer"
  swtpm_start
  start_qemu "$args $(tpm_qemu_args) $(secboot_qemu_args)" \
    || { warn "monitor not up; qemu.log:"; cat "$TILO_QEMU_LOG" >&2; die "boot-installer failed"; }
  msg "installer up. monitor=$TILO_MON serial=$TILO_SERIAL  (installs onto $target as /dev/vdb)"
}

# Boot the installed system — the target disk the custom installer wrote
# (default $TILO_RUN/target.qcow2). This is the post-install result.
cmd_boot_target() {
  local mode=2d disk="$TILO_TARGET_DISK"
  while [ $# -gt 0 ]; do case "$1" in
    --gl) mode=gl;; --2d) mode=2d;; --window) mode=window;; --disk) disk="$2"; shift;;
    --tpm) TILO_TPM=1;; --no-tpm) TILO_TPM=0;;
    --secboot) TILO_SECBOOT=1;;
    *) die "boot-target: unknown arg $1";; esac; shift; done
  [ -f "$disk" ] || die "target disk not found: $disk (run vm.sh boot-installer and install first)"
  vm_running && die "VM already running; stop it first"
  local varstore display
  firmware_setup
  varstore="$(firmware_varstore)"
  rm -f "$TILO_SERIAL" "$TILO_MON" "$TILO_PIDFILE" "$TILO_CONSOLE"
  display="$(display_qemu_args "$mode")"
  msg "boot-target ($mode, ${TILO_XRES}x${TILO_YRES}) disk=$disk (installed system)"
  if [ "$mode" = 2d ]; then
    warn "2D proves the boot/unlock path, but the non-GL virtio display exposes no virgl GPU to the Tilo compositor."
    warn "The framebuffer can go black after Plymouth even when greetd and the session are healthy; use --gl to validate the desktop."
  fi
  local args="$(machine_args) $(qemu_accel_args) -m $TILO_MEM -smp $TILO_SMP \
    $(pflash_args "$varstore") \
    -drive file=$disk,format=qcow2,if=virtio,serial=tilo-install-target \
    $display -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:$TILO_SSH_PORT-:22 $(input_qemu_args) \
    $(console_qemu_args) \
    -monitor unix:$TILO_MON,server,nowait -serial file:$TILO_SERIAL \
    -pidfile $TILO_PIDFILE -name tilo-target"
  swtpm_start
  start_qemu "$args $(tpm_qemu_args) $(secboot_qemu_args)" \
    || { warn "monitor not up; qemu.log:"; cat "$TILO_QEMU_LOG" >&2; die "boot-target failed"; }
  msg "target up. monitor=$TILO_MON serial=$TILO_SERIAL"
}

# greetd starting is not the greeter being usable: greetd then starts cage,
# cage the greeter, and only once the greeter's window is mapped and focused do
# typed keys reach it. Keys sent earlier are dropped, so the barrier is the
# greeter's own report over its console that sign-in accepts input.
greeter_accepts_input() {
  python3 "$TILO_OS_DIR/dev/installer-console.py" --socket "$TILO_CONSOLE" \
    check greeter /accepts_input --timeout 5 2>/dev/null
}

# The live installer medium's greetd runs the installer instead, which is
# driven through its console actions.
installer_answers() {
  python3 "$TILO_OS_DIR/dev/installer-console.py" --socket "$TILO_CONSOLE" \
    check installer --timeout 5 2>/dev/null
}

cmd_wait() {
  msg "waiting for greeter (greetd) ..."
  local i early_reported=0 greetd=0 start=$SECONDS
  for i in $(seq 1 "$TILO_BOOT_WAIT_ATTEMPTS"); do
    if [ "$greetd" = 1 ]; then
      if greeter_accepts_input; then
        msg "greeter accepts input (~$((SECONDS - start))s)"; return 0
      elif installer_answers; then
        msg "installer up (~$((SECONDS - start))s)"; return 0
      fi
    elif tr -d '\000' < "$TILO_SERIAL" 2>/dev/null | grep -aq 'Started.*greetd.service\|session-c1.scope'; then
      msg "greetd up (~$((SECONDS - start))s); waiting for the greeter to accept input"; greetd=1; continue
    # A guest may reach greetd without writing kernel/userspace status to the
    # selected serial port. The installed validation disk supplies SSH access;
    # use it as an independent readiness signal when available.
    elif (( i % 5 == 0 )) && command -v sshpass >/dev/null 2>&1 \
       && [ -n "${TILO_USER:-}" ] && [ -n "${TILO_PASS:-}" ] \
       && gssh 'systemctl is-active --quiet greetd.service' >/dev/null 2>&1; then
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
    sleep 2
  done
  if command -v sshpass >/dev/null 2>&1 \
     && [ -n "${TILO_USER:-}" ] && [ -n "${TILO_PASS:-}" ]; then
    local ssh_diagnostic
    ssh_diagnostic="$(gssh 'systemctl status greetd.service tilo-validation-ssh.service --no-pager --lines=20; systemctl --failed --no-pager' 2>&1)" \
      || warn "validation SSH diagnostic probe failed"
    if [ -n "$ssh_diagnostic" ]; then
      warn "validation SSH diagnostic output:"
      printf '%s\n' "$ssh_diagnostic" | tail -n 100 >&2
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
  firmware_setup
  printf 'code       %s\n' "$TILO_FW_CODE"
  printf 'vars       %s\n' "$TILO_FW_VARS"
  printf 'format     %s\n' "$TILO_FW_FMT"
  printf 'keys       %s\n' "$TILO_SECBOOT_KEYS"
  printf 'measures   %s\n' "$TILO_FW_MEASURES"
  printf 'smm        %s\n' "$TILO_FW_SMM"
  printf 'tpm        %s\n' "$TILO_TPM"
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
  inenv "magick '$out.ppm' '$out.png' 2>/dev/null || convert '$out.ppm' '$out.png'; rm -f '$out.ppm'"
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
  hmp "mouse_move $qx $qy 1" >/dev/null
}

cmd_click() {
  local x="${1:?usage: click X Y [left|right|middle]}"
  local y="${2:?usage: click X Y [left|right|middle]}"
  local button="${3:-left}" mask
  case "$button" in
    left) mask=1;; right) mask=2;; middle) mask=4;;
    *) die "click button must be left, right, or middle";;
  esac
  cmd_pointer "$x" "$y"
  hmp "mouse_button $mask" >/dev/null
  sleep 0.08
  hmp "mouse_button 0" >/dev/null
}

# Typing is only meaningful once the greeter has the keyboard.
await_greeter_input() {
  local attempts="${TILO_GREETER_INPUT_ATTEMPTS:-60}" i
  for i in $(seq 1 "$attempts"); do
    greeter_accepts_input && return 0
    vm_running || die "qemu exited before the greeter accepted input"
    sleep 1
  done
  python3 "$TILO_OS_DIR/dev/installer-console.py" --socket "$TILO_CONSOLE" \
    check greeter /accepts_input --timeout 5 >&2 || true
  die "the greeter did not accept input; nothing was typed"
}

cmd_login() {
  local passcode_only=0
  if [ "${1:-}" = "--passcode-only" ]; then passcode_only=1; shift; fi
  local u="${1:-$TILO_USER}" p="${2:-$TILO_PASS}"
  await_greeter_input
  if [ "$passcode_only" -eq 1 ]; then
    p="${1:-$TILO_PASS}"
    msg "logging in with the installed user's passcode"
    local i
    for i in $(seq 1 64); do hmp "sendkey backspace 20" >/dev/null; sleep 0.03; done
    cmd_type "$p"
    cmd_keys ret
    msg "passcode sent"
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
    if gssh "! pgrep -f '^/usr/bin/shell --backend drm( |$)' >/dev/null && systemctl is-active --quiet greetd.service" >/dev/null 2>&1; then
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
  gssh "sudo systemctl poweroff" >/dev/null 2>&1 || true
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
  if vm_running; then hmp "quit" >/dev/null 2>&1 || true; sleep 1; fi
  if vm_running; then inenv "kill \$(cat '$TILO_PIDFILE') 2>/dev/null" || true; fi
  rm -f "$TILO_PIDFILE"
  swtpm_stop
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
  wait-greeter-return) cmd_wait_greeter_return "$@";;
  shutdown) cmd_shutdown "$@";;
  cmd) hmp "$*";;
  # The host end of the installer/greeter console channel (tilo-control --socket).
  console) printf '%s\n' "$TILO_CONSOLE";;
  # Run one host tool (qemu-img, sfdisk, mkfs.*) where the VM tools live,
  # stdin passed through: in the distrobox when TILO_DISTROBOX is set.
  tool) [ "$#" -gt 0 ] || die "usage: tool PROGRAM [ARGS...]"; inenv "$(printf '%q ' "$@")";;
  ssh) gssh "$@";;
  serial) cmd_serial "$@";;
  journal) cmd_journal "$@";;
  status) cmd_status;;
  stop) cmd_stop;;
  capture-boot) cmd_capture_boot "$@";;
  -h|--help|"") sed -n '2,51p' "$0";;
  *) die "unknown subcommand: $sub (try --help)";;
esac
