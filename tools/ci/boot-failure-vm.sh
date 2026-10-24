#!/usr/bin/env bash
# Starts that fail on purpose, on the OS image os-image-x86_64 built, installed
# onto a disk as a device gets it and booted under KVM in a micro-VM: the
# production boot loader, the sealed UKI and the boot-failure screen.
#
#   boot-failure-vm.sh IN OUT   the image `os-image.sh build` left in IN; the
#                               evidence (screens, serial logs, the ESP and the
#                               notices after each start) goes to OUT
#
# The faults are made with the VM off, on the disk itself, and taken away again:
#
#   initrd unit   the root partition's GPT type no longer names a Tilo root, so
#                 tilo-root-discover.service fails in the initrd
#   sysroot       the root file system's superblock is gone, so sysroot.mount
#                 fails
#   greeter       greetd cannot start (a drop-in in the deployment's /etc), so
#                 tilo-boot-health finds no session after switch-root
#
# What is proved, start by start:
#
#   * every start is on trial: a good start is blessed and its entry put back
#     on trial with 2 tries (tilo-boot-rearm), not left without a counter;
#   * each fault shows the boot-failure screen, drawn by plymouth, naming the
#     step; keys move the focus and open the details, a tap presses a button,
#     and Try again restarts into the same entry;
#   * the record of an initrd failure is on the ESP, and the next good start
#     turns it (and a failure after switch-root, from the journal) into the
#     desktop's notice;
#   * after 2 failed starts in a row the boot loader stops choosing the entry
#     by itself: on this one-deployment disk it starts Repair Mode, the next in
#     its fallback order;
#   * the screen speaks the machine's language in the initrd too, from the note
#     tilo-deployment keeps on the ESP.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/publish/lib.sh"
cd "$repo_root"

in=${1:?usage: boot-failure-vm.sh IN OUT}
out=${2:?usage: boot-failure-vm.sh IN OUT}
[[ $(uname -m) == x86_64 ]] || die "boot-failure-vm runs on x86_64"
[[ -f $in/oci/index.json ]] || die "$in holds no OS image layout (oci/)"
[[ -c /dev/kvm ]] || die 'no /dev/kvm: the check needs requires = ["/dev/kvm"]'

missing=()
for p in dosfstools btrfs-progs systemd-container binutils socat qemu-img ipxe-roms-qemu \
         edk2-ovmf qemu-system-x86-core qemu-device-display-virtio-vga qemu-device-usb-host \
         python3-pillow ImageMagick util-linux jq; do
  rpm -q "$p" >/dev/null 2>&1 || missing+=("$p")
done
command -v podman >/dev/null || missing+=(podman skopeo fuse-overlayfs e2fsprogs)
((${#missing[@]} == 0)) || dnf install -y --setopt=install_weak_deps=False "${missing[@]}" >/dev/null
modprobe fuse 2>/dev/null || true
modprobe loop 2>/dev/null || true
use_cosign
id=$(podman pull -q "oci:$in/oci")
[[ $(podman image inspect --format '{{.Digest}}' "$id") == "$(cat "$in/digest")" ]] \
  || die "$in/oci is not the image $in/digest names"
podman tag "$id" localhost/tilo:ci

rm -rf "$out"
mkdir -p "$out"
export TILO_IMAGE=localhost/tilo:ci TILO_SKIP_BUILD=1 TILO_DISTROBOX= TILO_VM_ARCH=x86_64 \
  TILO_OUTPUT=$scratch/boot-output TILO_RUN=$scratch/boot-run TILO_MEM=6144 TILO_SMP=4 \
  TILO_BOOT_WAIT_ATTEMPTS=150 TMPDIR=/var/tmp
mkdir -p "$TILO_OUTPUT" "$TILO_RUN"
bash os/dev/build-image.sh > "$out/install.log" 2>&1 \
  || { tail -n 60 "$out/install.log"; die "the image does not install onto a disk"; }
base=$scratch/base.raw
overlay=$scratch/start.qcow2
qemu-img convert -O raw "$TILO_OUTPUT/qcow2/disk.qcow2" "$base"
rm -f "$TILO_OUTPUT/qcow2/disk.qcow2"
export TILO_DISK=$overlay

fails=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*"; fails=$((fails + 1)); }
# A failed run publishes nothing, so what it saw goes to the log: the ESP and
# notice of every start, and what the screen said on the console.
evidence() {
  local rc=$?
  ((fails || rc)) || return 0
  local f
  for f in "$out"/*-esp.txt; do [[ -f $f ]] && { echo "== $(basename "$f")"; cat "$f"; }; done
  for f in "$out"/*-serial.log; do
    [[ -f $f ]] && { echo "== $(basename "$f")"; grep -a -E 'tilo-boot-failure|systemd-boot|bless|Command line' "$f" | tail -20; }
  done
}
trap evidence EXIT
check() { local what=$1; shift; if "$@"; then ok "$what"; else bad "$what"; fi; }
vm() { bash os/dev/vm.sh "$@"; }

# --- the disk, with the VM off ---------------------------------------------------
ESP_TYPE=c12a7328-f81f-11d2-ba4b-00a0c93ec93b
ROOT_TYPE=7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab
mnt=$scratch/mnt
loop=""
attach() {
  loop=$(losetup -fP --show "$base")
  for _ in $(seq 50); do [[ -e ${loop}p1 ]] && break; sleep 0.1; done
}
detach() {
  umount "$mnt/esp" "$mnt/root" 2>/dev/null || true
  [[ -z $loop ]] || losetup -d "$loop"
  loop=""
}
part_number() {  # TYPE-GUID
  sfdisk -J "$loop" | jq -r --arg t "${1^^}" \
    '.partitiontable.partitions[] | select(.type == $t) | .node' | sed 's/.*p\([0-9]*\)$/\1/' | head -1
}
esp_part() { printf '%sp%s\n' "$loop" "$(part_number "$ESP_TYPE")"; }
root_part() { printf '%sp%s\n' "$loop" "${ROOT_PART:?}"; }
mount_esp() { mkdir -p "$mnt/esp"; mount "$(esp_part)" "$mnt/esp"; }
mount_root() { mkdir -p "$mnt/root"; mount "$(root_part)" "$mnt/root"; }
deploy_etc() { find "$mnt/root/state/deploy" -mindepth 2 -maxdepth 2 -name etc -type d | head -1; }
notice_file() { find "$mnt/root/state" -path '*/lib/tilo/boot-failure/notice' 2>/dev/null | head -1; }

# The ESP and the notice as they are now, kept as evidence.
inspect() {  # NAME
  attach; mount_esp
  { ls -l "$mnt/esp/loader/entries"; echo
    for f in "$mnt/esp/EFI/tilo/boot-info" "$mnt/esp/EFI/tilo/boot-failure"; do
      echo "== $f"; cat "$f" 2>/dev/null || echo "(none)"; done
  } > "$out/$1-esp.txt"
  mount_root
  { echo "== notice"; cat "$(notice_file)" 2>/dev/null || echo "(none)"; } >> "$out/$1-esp.txt"
  detach
}
entries() { sed -n '1,/^$/p' "$out/$1-esp.txt"; }

# --- starting it -------------------------------------------------------------------
start() {  # NAME
  rm -f "$overlay"
  qemu-img create -q -f qcow2 -F raw -b "$base" "$overlay"
  START=$1
  vm boot --2d > "$out/$1-boot.log" 2>&1 || { cat "$out/$1-boot.log"; die "the VM does not start"; }
}
stop() {
  if [[ ${1:-} == clean ]]; then
    vm cmd system_powerdown >/dev/null 2>&1 || true
    for _ in $(seq 60); do vm status >/dev/null 2>&1 || break; sleep 2; done
  fi
  vm stop >/dev/null 2>&1 || true
  tr -d '\000\r' < "$TILO_RUN/serial.log" | sed 's/\x1b\[[0-9;]*[A-Za-z]//g' > "$out/$START-serial.log" || true
  qemu-img commit -q "$overlay"
}
serial() { tr -d '\000\r' < "$TILO_RUN/serial.log" 2>/dev/null | sed 's/\x1b\[[0-9;]*[A-Za-z]//g'; }
# The screen's record reaches the console at a critical level, more than one
# line of it per start: starts are told apart by the boot id they name.
screens() { serial | grep -ao 'tilo-boot-failure: .*boot=[0-9a-f]*' | sed 's/.*boot=//' | sort -u | wc -l; }
await_screen() {  # COUNT [SECONDS]
  local n=$1 deadline=$((SECONDS + ${2:-420}))
  while (( SECONDS < deadline )); do
    (( $(screens) >= n )) && { sleep 4; return 0; }
    vm status >/dev/null 2>&1 || return 1
    sleep 3
  done
  return 1
}
shot() {  # NAME
  local png
  png=$(vm shot "boot-failure-$1") && cp "$png" "$out/$1.png" && printf '      shot %s\n' "$out/$1.png"
}
# Where tilo-boot-failure puts button N (from 0) with the details closed, in
# the pointer's 1280x800 space: the title a third down the screen, the first
# button 118 px below it, one every 60 px (its paint()). The screen's own
# height is the shot's.
button_y() {  # SHOT N
  local h
  h=$(python3 -c 'import sys; from PIL import Image; print(Image.open(sys.argv[1]).size[1])' "$1")
  echo $(( (h / 3 + 118 + 60 * $2) * 800 / h ))
}
# Two shots differ: a key or a tap changed the screen.
differs() { [[ -f $1 && -f $2 ]] && ! cmp -s "$1" "$2"; }
greeter_up() { vm wait > "$out/$START-wait.log" 2>&1; }

# --- 0. the first start: every entry goes on trial -------------------------------------
attach
ROOT_PART=$(part_number "$ROOT_TYPE")
[[ -n $ROOT_PART ]] || die "no Tilo root partition on the installed disk"
detach
start 0-first
check "the installed image starts to its greeter" greeter_up
sleep 60   # tilo-boot-entries.timer, 30 s after the start
stop clean
inspect 0-first
check "the first start's boot-entry pass puts the running entry on trial, with 2 tries" \
  grep -q 'bootc.*+2\.conf$' <(entries 0-first)
check "it notes the machine's locale on the ESP for the initrd's screen" \
  grep -qx 'locale en-US' "$out/0-first-esp.txt"

# --- 1. a good start is judged, blessed and put back on trial ----------------------------
start 1-good
check "a counted start reaches its greeter" greeter_up
sleep 90   # the health check's 20 s hold, the blessing, the re-arming
stop clean
inspect 1-good
check "after a good start the entry is on trial again with 2 tries, not left uncounted" \
  grep -q 'bootc.*+2\.conf$' <(entries 1-good)
check "and nothing failed, so there is no notice" grep -qx '(none)' <(sed -n '/== notice/,$p' "$out/1-good-esp.txt" | tail -1)

# --- 2. an initrd unit fails, twice ------------------------------------------------------
attach; sfdisk -q --part-type "$loop" "$ROOT_PART" 0fc63daf-8483-4772-8e79-3d69d8477de4; detach
start 2-initrd
if await_screen 1; then
  ok "a failing initrd unit (tilo-root-discover) shows the boot-failure screen"
  shot initrd-unit
  vm keys down ret; sleep 3; shot initrd-unit-details
  check "Down and Enter on Show details open the details" differs "$out/initrd-unit.png" "$out/initrd-unit-details.png"
  check "the screen says which unit failed" grep -q 'tilo-boot-failure: unit=tilo-root-discover.service' <(serial)
  # Enter again closes them; then Try again, by touch, anywhere along its row.
  vm keys ret; sleep 3
  vm click 200 "$(button_y "$out/initrd-unit.png" 0)"
  check "a tap on Try again restarts into the same entry, which fails again" await_screen 2
  shot initrd-unit-again
else
  bad "a failing initrd unit (tilo-root-discover) shows the boot-failure screen"
fi
stop
inspect 2-initrd
check "the initrd's record is on the ESP" grep -qx 'unit=tilo-root-discover.service' "$out/2-initrd-esp.txt"
check "naming the step" grep -qx 'step=mount' "$out/2-initrd-esp.txt"
check "two failed starts used up the entry's tries" grep -q 'bootc.*+0-2\.conf$' <(entries 2-initrd)
attach; sfdisk -q --part-type "$loop" "$ROOT_PART" "$ROOT_TYPE"; detach

# --- 3. the boot loader falls back by itself --------------------------------------------
start 3-fallback
sleep 150; shot fallback-repair-mode
stop
attach; mount_root
journal=$(find "$mnt/root/state" -path '*/var/log/journal' -type d 2>/dev/null | head -1)
journalctl -D "$journal" -b 0 -k --no-pager -o cat -g 'Command line' > "$out/3-fallback-cmdline.txt" 2>&1 || true
detach
check "with its tries used up the entry is passed over: the boot loader starts Repair Mode by itself" \
  grep -q 'tilo.mode=repair' "$out/3-fallback-cmdline.txt"
# Back to a start a person would choose from the boot menu after the repair.
attach; mount_esp
for f in "$mnt/esp/loader/entries"/bootc*+*.conf; do [[ $f == "${f%%+*}+2.conf" ]] || mv "$f" "${f%%+*}+2.conf"; done
mount_root; sed -i 's/^LANG=.*/LANG=de_DE.UTF-8/' "$(deploy_etc)/locale.conf"
detach

# --- 4. the next good start brings the record in -------------------------------------------
start 4-notice
check "the repaired machine starts to its greeter" greeter_up
sleep 90
stop clean
inspect 4-notice
check "the initrd failure became the desktop's notice" grep -q '^step=mount$' <(sed -n '/== notice/,$p' "$out/4-notice-esp.txt")
check "the ESP now notes the German locale for the initrd" grep -qx 'locale de-DE' "$out/4-notice-esp.txt"

# --- 5. sysroot does not mount: the screen in German, with a previous version -----------------
attach; mount_esp
cur=$(ls "$mnt/esp/loader/entries"/bootc*.conf | head -1)
# A second entry, so the screen has a previous version to offer (the same UKI;
# nothing but this start's screen reads it, and it is taken away after).
cp "$cur" "$mnt/esp/loader/entries/bootc_fedora-44-0-fake.conf"
touch -d '2026-10-09 12:00 UTC' "$mnt/esp/$(sed -n 's/^uki[[:space:]]*//p' "$cur" | sed 's#^/##')" 2>/dev/null || true
umount "$mnt/esp"
dd if="$(root_part)" of="$scratch/root-head" bs=64K count=2 status=none
dd if=/dev/zero of="$(root_part)" bs=64K count=2 conv=notrunc status=none
detach
start 5-sysroot
if await_screen 1; then
  ok "a root file system that does not mount shows the boot-failure screen"
  shot sysroot-de
  vm keys down; sleep 2; shot sysroot-de-focus
  check "Down moves the focus" differs "$out/sysroot-de.png" "$out/sysroot-de-focus.png"
  check "the failed unit is sysroot.mount" grep -q 'tilo-boot-failure: unit=sysroot.mount' <(serial)
  # Show details is the third button, after Try again and the previous version.
  vm click 640 "$(button_y "$out/sysroot-de.png" 2)"; sleep 3; shot sysroot-de-details
  check "a tap on Show details opens them" differs "$out/sysroot-de-focus.png" "$out/sysroot-de-details.png"
else
  bad "a root file system that does not mount shows the boot-failure screen"
fi
stop
inspect 5-sysroot
check "its record names sysroot.mount" grep -qx 'unit=sysroot.mount' "$out/5-sysroot-esp.txt"
attach
dd if="$scratch/root-head" of="$(root_part)" bs=64K count=2 conv=notrunc status=none
mount_esp
rm -f "$mnt/esp/loader/entries/bootc_fedora-44-0-fake.conf"
f=$(ls "$mnt/esp/loader/entries"/bootc*.conf | head -1)
mv "$f" "$mnt/esp/loader/entries/$(basename "$f" | sed -E 's/\+[0-9]+(-[0-9]+)?\.conf$/.conf/; s/\.conf$/+2.conf/')"
mount_root
sed -i 's/^LANG=.*/LANG=en_US.UTF-8/' "$(deploy_etc)/locale.conf"
install -d "$(deploy_etc)/systemd/system/greetd.service.d"
printf '[Service]\nExecStartPre=/usr/bin/false\n' > "$(deploy_etc)/systemd/system/greetd.service.d/90-boot-failure-vm.conf"
detach

# --- 6. the desktop does not start -------------------------------------------------------------
start 6-greeter
if await_screen 1 600; then
  ok "a desktop that never comes up shows the boot-failure screen after switch-root"
  check "naming the desktop step" grep -q 'tilo-boot-failure: step=desktop' <(serial)
  shot greeter
else
  bad "a desktop that never comes up shows the boot-failure screen after switch-root"
fi
stop
attach; mount_root
rm -f "$(deploy_etc)/systemd/system/greetd.service.d/90-boot-failure-vm.conf"
detach

# --- 7. and the next start says so ---------------------------------------------------------------
start 7-after
check "without the fault the machine starts to its greeter again" greeter_up
sleep 90
stop clean
inspect 7-after
check "the failure after switch-root, from the journal, became the desktop's notice" \
  grep -q '^step=desktop$' <(sed -n '/== notice/,$p' "$out/7-after-esp.txt")
check "and the entry is on trial again" grep -q 'bootc.*+2\.conf$' <(entries 7-after)

ls "$out"/*.png 2>/dev/null | sed 's/^/screen: /'
((fails == 0)) || die "$fails checks failed"
echo 'every start that failed on purpose showed the screen, left its record and was counted'
