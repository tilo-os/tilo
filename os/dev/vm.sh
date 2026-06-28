#!/bin/bash
# Drive a qOS qcow2 headlessly: boot, automate the greeter login, take
# screenshots, pull logs, and shut down. Designed for machines where qemu lives
# in a distrobox and the host is immutable (see os/dev/_common.sh for config).
#
#   ./os/dev/vm.sh boot [--gl] [--disk PATH]   start the VM (detached)
#   ./os/dev/vm.sh boot-installer [--gl]       boot the custom installer + blank /dev/vdb
#   ./os/dev/vm.sh boot-target [--gl]          boot the installed result (target.qcow2)
#   ./os/dev/vm.sh wait                        block until the greeter is up
#   ./os/dev/vm.sh login [USER PASS]           type credentials into tuigreet
#   ./os/dev/vm.sh shot NAME                   framebuffer -> $QOS_RUN/NAME.png
#   ./os/dev/vm.sh type STRING                 type a string (sendkey)
#   ./os/dev/vm.sh keys KEY...                 send named keys (ret tab esc f2..)
#   ./os/dev/vm.sh cmd "HMP"                   raw QEMU monitor command
#   ./os/dev/vm.sh serial [GREP]               dump the boot serial console log
#   ./os/dev/vm.sh journal [GREP]             offline journal extract (VM stopped)
#   ./os/dev/vm.sh status                       is it running?
#   ./os/dev/vm.sh stop                         power off
#   ./os/dev/vm.sh capture-boot [DIR]          boot->greeter->login->desktop demo
#
# DISPLAY MODES (important):
#   --2d (default): -device virtio-vga -display none. screendump works for GRUB,
#       the kernel console, and the tuigreet greeter. The compositor falls back
#       to software GL (llvmpipe) and may not start.
#   --gl: -device virtio-vga-gl -display egl-headless,rendernode=<iGPU>. Gives
#       the guest hardware GL (virgl) so the compositor can run — BUT screendump
#       only captures GL scanouts, so the 2D greeter/console show "no surface".
#   There is no single headless mode that screenshots both the 2D greeter and the
#   GL compositor; pick per task. Serial logging (below) works in both.
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

mkdir -p "$QOS_RUN"

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

vm_running() { [ -f "$QOS_PIDFILE" ] && inenv "kill -0 \$(cat '$QOS_PIDFILE') 2>/dev/null"; }

cmd_boot() {
  local mode=2d disk="$QOS_DISK"
  while [ $# -gt 0 ]; do case "$1" in
    --gl) mode=gl;; --2d) mode=2d;; --window) mode=window;; --disk) disk="$2"; shift;;
    *) die "boot: unknown arg $1";; esac; shift; done
  [ -f "$disk" ] || die "disk not found: $disk (run os/dev/build-image.sh)"
  vm_running && die "VM already running (pid $(cat "$QOS_PIDFILE")); stop it first"

  local code vars rn display
  code="$(ovmf_code)"
  vars="$(ovmf_vars)"
  rm -f "$QOS_SERIAL" "$QOS_MON" "$QOS_PIDFILE"
  inenv "cp -f '$vars' '$QOS_RUN/vars.fd'"
  local res="xres=$QOS_XRES,yres=$QOS_YRES"
  case "$mode" in
    gl)
      rn="$(detect_rendernode)"; display="-device virtio-vga-gl,$res -display egl-headless,rendernode=$rn"
      msg "boot (GL headless, rendernode=$rn, ${QOS_XRES}x${QOS_YRES}) disk=$disk" ;;
    window)
      # On-screen GTK window, GL-accelerated: virtio-vga-gl + gtk,gl=on give
      # the guest hardware GL (virgl) so the compositor's wgpu/Vello scene
      # paint runs on the GPU. (screendump can't read a GL surface, so verify
      # resolution via the scanout log over SSH rather than a framebuffer dump.)
      display="-device virtio-vga-gl,$res -display gtk,gl=on"
      msg "boot (gtk window, GL, ${QOS_XRES}x${QOS_YRES}) disk=$disk" ;;
    *)
      display="-device virtio-vga,$res -display none"
      msg "boot (2D headless, ${QOS_XRES}x${QOS_YRES}) disk=$disk" ;;
  esac

  local args="-machine q35,accel=kvm -enable-kvm -m $QOS_MEM -cpu host -smp $QOS_SMP \
    -drive if=pflash,format=raw,readonly=on,file=$code \
    -drive if=pflash,format=raw,file=$QOS_RUN/vars.fd \
    -drive file=$disk,format=qcow2,if=virtio \
    $display -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:$QOS_SSH_PORT-:22 -usb -device usb-tablet \
    -monitor unix:$QOS_MON,server,nowait -serial file:$QOS_SERIAL \
    -pidfile $QOS_PIDFILE -name qos"
  inenv "setsid nohup qemu-system-x86_64 $args >'$QOS_QEMU_LOG' 2>&1 & disown"
  sleep 1
  inenv "test -S '$QOS_MON'" || { warn "monitor socket not up; qemu.log:"; cat "$QOS_QEMU_LOG" >&2; die "boot failed"; }
  msg "up. monitor=$QOS_MON serial=$QOS_SERIAL"
}

# Boot the installer ISO (CD-ROM) against a fresh blank target disk, so Anaconda
# can install qOS onto it. After the install completes + reboots, boot the
# installed system with:  vm.sh boot --gl --disk $QOS_RUN/target.qcow2
cmd_boot_iso() {
  local mode=2d
  while [ $# -gt 0 ]; do case "$1" in
    --gl) mode=gl;; --2d) mode=2d;; --window) mode=window;;
    *) die "boot-iso: unknown arg $1";; esac; shift; done
  local iso="$QOS_ISO"
  [ -z "$iso" ] && iso="$(find "$QOS_OUTPUT" -name '*.iso' -type f 2>/dev/null | head -1)"
  [ -n "$iso" ] && [ -f "$iso" ] || die "no installer ISO (run ./os/dev/build-installer.sh) — or set QOS_ISO=..."
  vm_running && die "VM already running; stop it first"
  local code vars display rn
  code="$(ovmf_code)"; vars="$(ovmf_vars)"
  rm -f "$QOS_SERIAL" "$QOS_MON" "$QOS_PIDFILE"
  inenv "cp -f '$vars' '$QOS_RUN/vars.fd'"
  local target="$QOS_RUN/target.qcow2"
  inenv "test -f '$target' || qemu-img create -f qcow2 '$target' 20G" >/dev/null
  local res="xres=$QOS_XRES,yres=$QOS_YRES"
  case "$mode" in
    gl) rn="$(detect_rendernode)"; display="-device virtio-vga-gl,$res -display egl-headless,rendernode=$rn";;
    window) display="-device virtio-vga-gl,$res -display gtk,gl=on";;
    *) display="-device virtio-vga,$res -display none";;
  esac
  msg "boot-iso ($mode, ${QOS_XRES}x${QOS_YRES}) iso=$iso -> target=$target"
  local args="-machine q35,accel=kvm -enable-kvm -m $QOS_MEM -cpu host -smp $QOS_SMP \
    -drive if=pflash,format=raw,readonly=on,file=$code \
    -drive if=pflash,format=raw,file=$QOS_RUN/vars.fd \
    -drive file=$iso,format=raw,media=cdrom,if=ide \
    -drive file=$target,format=qcow2,if=virtio \
    -boot d $display -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:$QOS_SSH_PORT-:22 \
    -usb -device usb-tablet \
    -monitor unix:$QOS_MON,server,nowait -serial file:$QOS_SERIAL \
    -pidfile $QOS_PIDFILE -name qos-installer"
  inenv "setsid nohup qemu-system-x86_64 $args >'$QOS_QEMU_LOG' 2>&1 & disown"
  sleep 1
  inenv "test -S '$QOS_MON'" || { warn "monitor not up; qemu.log:"; cat "$QOS_QEMU_LOG" >&2; die "boot-iso failed"; }
  msg "installer up. monitor=$QOS_MON serial=$QOS_SERIAL  (installs onto $target)"
}

# Boot the CUSTOM installer live qcow2 (build-custom-installer.sh) with a blank
# target disk attached as a SECOND virtio disk (/dev/vdb). The installer
# self-installs the booted image onto /dev/vdb. After it finishes, boot the
# result with:  vm.sh boot-target [--gl]
cmd_boot_installer() {
  local mode=2d
  while [ $# -gt 0 ]; do case "$1" in
    --gl) mode=gl;; --2d) mode=2d;; --window) mode=window;;
    *) die "boot-installer: unknown arg $1";; esac; shift; done
  local disk="$QOS_INSTALLER_DISK"
  [ -f "$disk" ] || die "installer disk not found: $disk (run ./os/dev/build-custom-installer.sh)"
  vm_running && die "VM already running; stop it first"
  local code vars display rn
  code="$(ovmf_code)"; vars="$(ovmf_vars)"
  rm -f "$QOS_SERIAL" "$QOS_MON" "$QOS_PIDFILE"
  inenv "cp -f '$vars' '$QOS_RUN/vars.fd'"
  local target="$QOS_TARGET_DISK"
  inenv "test -f '$target' || qemu-img create -f qcow2 '$target' '$QOS_TARGET_SIZE'" >/dev/null
  local res="xres=$QOS_XRES,yres=$QOS_YRES"
  case "$mode" in
    gl) rn="$(detect_rendernode)"; display="-device virtio-vga-gl,$res -display egl-headless,rendernode=$rn";;
    window) display="-device virtio-vga-gl,$res -display gtk,gl=on";;
    *) display="-device virtio-vga,$res -display none";;
  esac
  msg "boot-installer ($mode, ${QOS_XRES}x${QOS_YRES}) disk=$disk target=$target (/dev/vdb)"
  # First virtio disk = installer (/dev/vda); second = blank target (/dev/vdb).
  local args="-machine q35,accel=kvm -enable-kvm -m $QOS_MEM -cpu host -smp $QOS_SMP \
    -drive if=pflash,format=raw,readonly=on,file=$code \
    -drive if=pflash,format=raw,file=$QOS_RUN/vars.fd \
    -drive file=$disk,format=qcow2,if=virtio \
    -drive file=$target,format=qcow2,if=virtio \
    $display -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:$QOS_SSH_PORT-:22 -usb -device usb-tablet \
    -monitor unix:$QOS_MON,server,nowait -serial file:$QOS_SERIAL \
    -pidfile $QOS_PIDFILE -name qos-custom-installer"
  inenv "setsid nohup qemu-system-x86_64 $args >'$QOS_QEMU_LOG' 2>&1 & disown"
  sleep 1
  inenv "test -S '$QOS_MON'" || { warn "monitor not up; qemu.log:"; cat "$QOS_QEMU_LOG" >&2; die "boot-installer failed"; }
  msg "installer up. monitor=$QOS_MON serial=$QOS_SERIAL  (installs onto $target as /dev/vdb)"
}

# Boot the installed system — the target disk the custom installer wrote
# (default $QOS_RUN/target.qcow2). This is the post-install result.
cmd_boot_target() {
  local mode=2d disk="$QOS_TARGET_DISK"
  while [ $# -gt 0 ]; do case "$1" in
    --gl) mode=gl;; --2d) mode=2d;; --window) mode=window;; --disk) disk="$2"; shift;;
    *) die "boot-target: unknown arg $1";; esac; shift; done
  [ -f "$disk" ] || die "target disk not found: $disk (run vm.sh boot-installer and install first)"
  vm_running && die "VM already running; stop it first"
  local code vars display rn
  code="$(ovmf_code)"; vars="$(ovmf_vars)"
  rm -f "$QOS_SERIAL" "$QOS_MON" "$QOS_PIDFILE"
  inenv "cp -f '$vars' '$QOS_RUN/vars.fd'"
  local res="xres=$QOS_XRES,yres=$QOS_YRES"
  case "$mode" in
    gl) rn="$(detect_rendernode)"; display="-device virtio-vga-gl,$res -display egl-headless,rendernode=$rn";;
    window) display="-device virtio-vga-gl,$res -display gtk,gl=on";;
    *) display="-device virtio-vga,$res -display none";;
  esac
  msg "boot-target ($mode, ${QOS_XRES}x${QOS_YRES}) disk=$disk (installed system)"
  local args="-machine q35,accel=kvm -enable-kvm -m $QOS_MEM -cpu host -smp $QOS_SMP \
    -drive if=pflash,format=raw,readonly=on,file=$code \
    -drive if=pflash,format=raw,file=$QOS_RUN/vars.fd \
    -drive file=$disk,format=qcow2,if=virtio \
    $display -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:$QOS_SSH_PORT-:22 -usb -device usb-tablet \
    -monitor unix:$QOS_MON,server,nowait -serial file:$QOS_SERIAL \
    -pidfile $QOS_PIDFILE -name qos-target"
  inenv "setsid nohup qemu-system-x86_64 $args >'$QOS_QEMU_LOG' 2>&1 & disown"
  sleep 1
  inenv "test -S '$QOS_MON'" || { warn "monitor not up; qemu.log:"; cat "$QOS_QEMU_LOG" >&2; die "boot-target failed"; }
  msg "target up. monitor=$QOS_MON serial=$QOS_SERIAL"
}

cmd_wait() {
  msg "waiting for greeter (greetd) ..."
  local i
  for i in $(seq 1 90); do
    if tr -d '\000' < "$QOS_SERIAL" 2>/dev/null | grep -aq 'Started.*greetd.service\|session-c1.scope'; then
      msg "greeter up (~$((i*2))s)"; return 0
    fi
    vm_running || die "qemu exited during boot; see $QOS_QEMU_LOG / $QOS_SERIAL"
    sleep 2
  done
  die "timed out waiting for greeter (see $QOS_SERIAL)"
}

cmd_shot() {
  local name="${1:?usage: shot NAME}"
  local out="$QOS_RUN/$name"
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
  for ((i=0; i<${#s}; i++)); do c="${s:$i:1}"; hmp "sendkey $(char_key "$c")" >/dev/null; sleep 0.05; done
}

cmd_keys() { local k; for k in "$@"; do hmp "sendkey $k" >/dev/null; sleep 0.12; done; }

cmd_login() {
  local u="${1:-$QOS_USER}" p="${2:-$QOS_PASS}"
  msg "logging in as $u"
  local i; for i in $(seq 1 24); do hmp "sendkey backspace" >/dev/null; done  # clear remembered user
  cmd_type "$u"; cmd_keys ret; sleep 2
  cmd_type "$p"; cmd_keys ret
  msg "credentials sent"
}

cmd_serial() {
  [ -f "$QOS_SERIAL" ] || die "no serial log yet ($QOS_SERIAL)"
  if [ -n "${1:-}" ]; then tr -d '\000' < "$QOS_SERIAL" | sed 's/\r//g' | grep -aiE "$1"; else
    tr -d '\000' < "$QOS_SERIAL" | sed 's/\r//g'; fi
}

cmd_status() { if vm_running; then echo "running (pid $(cat "$QOS_PIDFILE"))"; else echo "stopped"; fi; }

cmd_stop() {
  if vm_running; then hmp "quit" >/dev/null 2>&1 || true; sleep 1; fi
  if vm_running; then inenv "kill \$(cat '$QOS_PIDFILE') 2>/dev/null" || true; fi
  rm -f "$QOS_PIDFILE"; msg "stopped"
}

# Offline journal extraction via qemu-nbd. The VM must be stopped. General
# method (no in-image instrumentation), but needs passwordless sudo + the nbd
# kernel module. Works only if the guest has a *persistent* journal
# (/var/log/journal); otherwise use serial logging + StandardError=console.
cmd_journal() {
  vm_running && die "stop the VM first (journal extraction needs the disk offline)"
  local grep_pat="${1:-}" mnt="$QOS_RUN/mnt"
  msg "attaching $QOS_DISK via qemu-nbd"
  sudo modprobe nbd max_part=8
  inenv "sudo qemu-nbd --read-only --connect=/dev/nbd0 '$QOS_DISK'"
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
  local dir="${1:-$QOS_RUN/capture}"; mkdir -p "$dir"
  QOS_RUN_SAVE="$QOS_RUN"
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
  boot) cmd_boot "$@";;
  boot-iso) cmd_boot_iso "$@";;
  boot-installer) cmd_boot_installer "$@";;
  boot-target) cmd_boot_target "$@";;
  wait) cmd_wait "$@";;
  login) cmd_login "$@";;
  shot) cmd_shot "$@";;
  type) cmd_type "$@";;
  keys) cmd_keys "$@";;
  cmd) hmp "$*";;
  ssh) gssh "$@";;
  serial) cmd_serial "$@";;
  journal) cmd_journal "$@";;
  status) cmd_status;;
  stop) cmd_stop;;
  capture-boot) cmd_capture_boot "$@";;
  -h|--help|"") sed -n '2,40p' "$0";;
  *) die "unknown subcommand: $sub (try --help)";;
esac
