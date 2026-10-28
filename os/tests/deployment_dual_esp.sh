#!/usr/bin/env bash
# tilo status, tilo boot and tilo rollback find the ESP the machine started
# from on a disk shared with Windows, whose two ESPs are both FAT, both carry
# an EFI directory, and only one of which boots Tilo.
#
# The layout is the one such a machine has: the Windows ESP mounted at
# /boot/efi, the boot loader's own ESP not mounted at all, sd-boot naming it in
# LoaderDevicePartUUID and its UKI entries counted in their file names. The
# production tilo-deployment runs against it, with recording stand-ins for the
# commands that would touch the machine (mount, bootc, bootctl, systemctl).
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
deployment="$repo/os/usr/libexec/tilo-deployment"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
fails=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*"; fails=$((fails + 1)); }

guid=4a67b082-0a4c-41cf-b6c7-440b29bb8c4f
tilo_uuid=2dbc5c51-a046-4266-a3ef-4256b25cdd1d
windows_uuid=6e3c8d29-eefd-452f-a856-9bc30f84aace
booted=$(printf 'a%.0s' {1..128})
rollback=$(printf 'b%.0s' {1..128})
staged=$(printf 'c%.0s' {1..128})

# A Boot Loader Interface string variable as efivarfs shows it: four attribute
# bytes, then UTF-16LE with a terminating NUL.
efivar() {  # NAME VALUE
  install -d "$work/efivars"
  {
    printf '\x06\x00\x00\x00'
    printf '%s' "$2" | sed 's/./&\n/g' | while IFS= read -r c; do
      [ -n "$c" ] && printf '%s\x00' "$c"
    done
    printf '\x00\x00'
  } >"$work/efivars/$1-$guid"
}

# The Tilo ESP's contents, kept off any mount until something mounts it.
make_tilo_esp() {
  local esp="$work/disk/p5" digest gen
  rm -rf "$esp"
  install -d "$esp/loader/entries" "$esp/loader/entries.staged" "$esp/EFI/Linux/bootc" "$esp/EFI/BOOT"
  : >"$esp/EFI/BOOT/BOOTAA64.EFI"
  : >"$esp/EFI/Linux/tilo-repair.efi"
  for digest in "$booted:7" "$rollback:6" "$staged:8"; do
    gen="${digest#*:}" digest="${digest%:*}"
    # A UKI stand-in whose sealed command line is the whole file (see the
    # tilo-uki-cmdline stand-in); the PE parser has its own tests.
    printf 'root=tilo-auto composefs=%s tilo.generation=%s tilo.mode=normal\n' "$digest" "$gen" \
      >"$esp/EFI/Linux/bootc/bootc_composefs-$digest.efi"
  done
  printf 'title Fedora Linux 44\nuki /EFI/Linux/bootc/bootc_composefs-%s.efi\nsort-key bootc-fedora-1\n' "$booted" \
    >"$esp/loader/entries/bootc_fedora-44-2.conf"
  printf 'title Fedora Linux 44\nuki /EFI/Linux/bootc/bootc_composefs-%s.efi\nsort-key bootc-fedora-0\n' "$rollback" \
    >"$esp/loader/entries/bootc_fedora-44-1+1-2.conf"
  printf 'title Fedora Linux 44\nuki /EFI/Linux/bootc/bootc_composefs-%s.efi\nsort-key bootc-fedora-2\n' "$staged" \
    >"$esp/loader/entries.staged/bootc_fedora-44-3+3.conf"
}

make_windows_esp() {
  install -d "$work/root/boot/efi/EFI/Microsoft/Boot" "$work/root/boot/efi/EFI/Boot"
  : >"$work/root/boot/efi/EFI/Microsoft/Boot/bootmgfw.efi"
  : >"$work/root/boot/efi/EFI/Boot/BOOTAA64.EFI"
}

cat >"$work/bootc.json" <<EOF
{"status": {
  "booted": {"image": {"image": {"image": "registry.tilo.so/tilo/tilo-vm:main-d7d1575-lenovo-83ed"},
             "timestamp": "2026-10-09T13:20:59Z"}, "composefs": {"verity": "$booted"}},
  "staged": {"image": {"image": {"image": "registry.tilo.so/tilo/tilo-vm:main-e1f2a3b-lenovo-83ed"},
             "timestamp": "2026-10-21T09:00:00Z"}, "composefs": {"verity": "$staged"}},
  "rollback": {"image": {"image": {"image": "registry.tilo.so/tilo/tilo-vm:main-6656acd-lenovo-83ed"},
               "timestamp": "2026-10-02T07:15:00Z"}, "composefs": {"verity": "$rollback"}}
}}
EOF

install -d "$work/bin" "$work/udev"
printf 'E:ID_FS_TYPE=vfat\nE:ID_PART_ENTRY_UUID=%s\n' "$tilo_uuid" >"$work/udev/b259:5"
printf 'E:ID_FS_TYPE=vfat\nE:ID_PART_ENTRY_UUID=%s\n' "$windows_uuid" >"$work/udev/b259:1"

# The mount table: "TARGET MAJ:MIN SOURCE" per line.
cat >"$work/bin/findmnt" <<'STUB'
#!/usr/bin/env bash
case "$*" in
  "-rn -o TARGET,MAJ:MIN") awk '{ print $1, $2 }' "$FIXTURE/mounts" ;;
  "-n -o SOURCE --mountpoint "*) awk -v t="${!#}" '$1 == t { print $3 }' "$FIXTURE/mounts" ;;
  *) echo "findmnt stand-in: unexpected $*" >&2; exit 2 ;;
esac
STUB
cat >"$work/bin/mountpoint" <<'STUB'
#!/usr/bin/env bash
[ "$1" = -q ] || exit 2
awk -v t="$2" '$1 == t { found = 1 } END { exit !found }' "$FIXTURE/mounts"
STUB
# Mounting the Tilo ESP: its contents appear at the target, and the mount
# table says so. Both mount routes record themselves.
cat >"$work/bin/fixture-mount-esp" <<'STUB'
#!/usr/bin/env bash
dev="$1" where="$2"
echo "mount $dev $where" >>"$FIXTURE/log"
case "$dev" in */"$TILO_UUID") ;; *) echo "mounted the wrong ESP: $dev" >&2; exit 1 ;; esac
cp -a "$FIXTURE/disk/p5/." "$where/"
echo "$where 259:5 /dev/nvme0n1p5" >>"$FIXTURE/mounts"
STUB
cat >"$work/bin/systemctl" <<'STUB'
#!/usr/bin/env bash
echo "systemctl $*" >>"$FIXTURE/log"
case "$*" in
  "cat tilo-esp-mount.service") [ -e "$FIXTURE/esp-mount-unit" ] ;;
  "start tilo-esp-mount.service")
    fixture-mount-esp "/dev/disk/by-partuuid/$TILO_UUID" "$(cat "$FIXTURE/esp-mount-unit")" ;;
  *) exit 0 ;;
esac
STUB
cat >"$work/bin/id" <<'STUB'
#!/usr/bin/env bash
[ "$1" = -u ] && { echo "$FIXTURE_UID"; exit 0; }
exec /usr/bin/id "$@"
STUB
cat >"$work/bin/bootc" <<'STUB'
#!/usr/bin/env bash
echo "bootc $*" >>"$FIXTURE/log"
case "$*" in
  "status --format json") cat "$FIXTURE/bootc.json" ;;
  rollback) [ -z "${FIXTURE_ROLLBACK_DEFAULT:-}" ] || echo "$FIXTURE_ROLLBACK_DEFAULT" >"$FIXTURE/default" ;;
  *) exit 2 ;;
esac
STUB
# sd-boot's menu as bootctl lists it, read from the ESP it is pointed at:
# counters stripped from the ids, the Type #2 image at the top of EFI/Linux.
cat >"$work/bin/bootctl" <<'STUB'
#!/usr/bin/env bash
echo "bootctl $*" >>"$FIXTURE/log"
[ -z "${FIXTURE_BOOTCTL_FAILS:-}" ] || exit 1
case "$1" in
  --esp-path=*) esp="${1#--esp-path=}" ;;
  set-oneshot|set-default) exit 0 ;;
  *) exit 1 ;;
esac
[ "$2 $3" = "--no-pager list" ] || exit 2
[ -d "$esp/loader/entries" ] || { echo "Couldn't find EFI system partition." >&2; exit 1; }
default="$(cat "$FIXTURE/default" 2>/dev/null)"
for f in "$esp"/loader/entries/*.conf; do
  id="${f##*/}"; id="${id%.conf}"; id="${id%%+*}.conf"
  title="$(sed -n 's/^title //p' "$f")"
  [ "$id" = "$default" ] && title="$title (default)"
  printf '         type: Boot Loader Specification Type #1 (.conf)\n        title: %s\n           id: %s\n\n' "$title" "$id"
done
for f in "$esp"/EFI/Linux/*.efi; do
  printf '         type: Boot Loader Specification Type #2 (.efi)\n        title: Tilo Repair Mode\n           id: %s\n\n' "${f##*/}"
done
STUB
cat >"$work/bin/tilo-image-trust" <<'STUB'
#!/usr/bin/env python3
import json, sys
role, field = sys.argv[2].split("-", 1)
value = json.load(sys.stdin)["status"].get(role) or {}
path = {"image": ["image", "image", "image"], "version": ["image", "version"],
        "timestamp": ["image", "timestamp"], "composefs": ["composefs", "verity"]}[field]
for key in path:
    value = value.get(key) if isinstance(value, dict) else None
print(value or "")
STUB
cat >"$work/bin/tilo-uki-cmdline" <<'STUB'
#!/usr/bin/env bash
[ "$1" = show ] && exec cat "$2"
exit 2
STUB
chmod +x "$work/bin"/*

# A fresh machine: Windows ESP at /boot/efi, the Tilo ESP unmounted.
reset() {
  rm -rf "$work/root" "$work/log" "$work/esp-mount-unit" "$work/default"
  install -d "$work/root/boot" "$work/root/mnt"
  make_tilo_esp
  make_windows_esp
  printf '/ 253:0 /dev/mapper/root\n%s 259:1 /dev/nvme0n1p1\n' "$work/root/boot/efi" >"$work/mounts"
  : >"$work/log"
  rm -rf "$work/efivars"
  efivar LoaderDevicePartUUID "${tilo_uuid^^}"
  efivar LoaderEntrySelected bootc_fedora-44-2.conf
  echo bootc_fedora-44-2.conf >"$work/default"
}

run() {  # UID ARGS...
  local uid="$1"; shift
  env -i PATH="$work/bin:/usr/bin:/bin" FIXTURE="$work" FIXTURE_UID="$uid" \
    TILO_UUID="$tilo_uuid" TILO_EFIVARS="$work/efivars" TILO_UDEV_DATA="$work/udev" \
    TILO_ESP_CANDIDATES="$work/root/boot $work/root/boot/efi" \
    TILO_ESP_MOUNT="$work/bin/fixture-mount-esp" TILO_UKI_CMDLINE="$work/bin/tilo-uki-cmdline" \
    TILO_ESP_PLAN_DIR="$work/plan" TILO_ESP_WRITE_CMD=true \
    FIXTURE_BOOTCTL_FAILS="${FIXTURE_BOOTCTL_FAILS:-}" \
    FIXTURE_ROLLBACK_DEFAULT="${FIXTURE_ROLLBACK_DEFAULT:-}" \
    bash "$deployment" "$@"
}

has() {  # TEXT NEEDLE WHAT
  if grep -qF -- "$2" <<<"$1"; then ok "$3"; else bad "$3"; printf '      wanted: %s\n%s\n' "$2" "$1" | sed 's/^/      /'; fi
}
lacks() {  # TEXT NEEDLE WHAT
  if grep -qF -- "$2" <<<"$1"; then bad "$3"; printf '%s\n' "$1" | sed 's/^/      /'; else ok "$3"; fi
}

# --- tilo status, as root, through the generator's own mount unit -------------
reset
echo "$work/root/boot" >"$work/esp-mount-unit"
out="$(run 0 list 2>&1)" || bad "list failed: $out"
has "$(cat "$work/log")" "systemctl start tilo-esp-mount.service" \
  "the unmounted ESP is mounted on demand through tilo-esp-mount.service"
has "$out" "Started from the ESP on /dev/nvme0n1p5 at $work/root/boot" \
  "it is the ESP the boot loader named, not the Windows one at /boot/efi"
lacks "$out" "nvme0n1p1" "the Windows ESP is never reported"
has "$out" "booted    registry.tilo.so/tilo/tilo-vm:main-d7d1575-lenovo-83ed" "the booted image"
has "$out" "built 2026-10-09, generation 7" "the booted image's date and generation"
has "$out" "entry bootc_fedora-44-2.conf, booted now: good" "the booted entry and its blessed state"
has "$out" "staged    registry.tilo.so/tilo/tilo-vm:main-e1f2a3b-lenovo-83ed" "the staged image"
has "$out" "built 2026-10-21" "the staged image's date"
has "$out" "entry bootc_fedora-44-3.conf, in the menu after the next shutdown: on trial, 3 of 3 tries left" \
  "the staged entry, still under entries.staged, with its full counter"
has "$out" "rollback  registry.tilo.so/tilo/tilo-vm:main-6656acd-lenovo-83ed" "the rollback image"
has "$out" "built 2026-10-02" "the rollback image's date"
has "$out" "entry bootc_fedora-44-1.conf: on trial, 1 of 3 tries left" "the rollback entry's boot counter"
has "$out" "bootc_fedora-44-2.conf       Fedora Linux 44 (booted now, starts next)" "the boot menu marks the booted and next entry"
has "$out" "tilo-repair.efi              Tilo Repair Mode" "the boot menu lists the Type #2 repair entry"
has "$(cat "$work/log")" "bootctl --esp-path=$work/root/boot --no-pager list" "bootctl is pointed at the ESP that was found"

# --- the same without the generator's unit: tilo-esp-mount where it would go ----
reset
out="$(run 0 list 2>&1)" || bad "list without a mount unit failed: $out"
has "$(cat "$work/log")" "mount /dev/disk/by-partuuid/$tilo_uuid $work/root/boot" \
  "with no tilo-esp-mount.service, tilo-esp-mount mounts the named ESP at /boot"
has "$out" "entry bootc_fedora-44-2.conf, booted now: good" "and the listing is the same"

# --- not root: nothing is mounted, and the message says what is needed ---------
reset
if out="$(run 1000 list 2>&1)"; then bad "an unprivileged list of an unmounted ESP succeeded"; fi
has "$out" "is not mounted, and mounting it needs root: sudo tilo status" "an unprivileged caller is told it needs root"
lacks "$(cat "$work/log")" "mount " "and nothing is mounted for it"

# --- already mounted somewhere else ---------------------------------------------
reset
install -d "$work/root/mnt/esp"
cp -a "$work/disk/p5/." "$work/root/mnt/esp/"
echo "$work/root/mnt/esp 259:5 /dev/nvme0n1p5" >>"$work/mounts"
out="$(run 0 list 2>&1)" || bad "list of an ESP mounted elsewhere failed: $out"
has "$out" "Started from the ESP on /dev/nvme0n1p5 at $work/root/mnt/esp" "an ESP mounted elsewhere is found where it is"
lacks "$(cat "$work/log")" "mount " "and is not mounted a second time"

# --- tilo boot -------------------------------------------------------------------
reset
echo "$work/root/boot" >"$work/esp-mount-unit"
run 0 boot rollback >/dev/null 2>&1 || bad "boot rollback failed"
has "$(cat "$work/log")" "bootctl set-oneshot bootc_fedora-44-1.conf" \
  "tilo boot rollback starts the rollback deployment's entry once, found by its composefs identity"
run 0 boot bootc_fedora-44-2 >/dev/null 2>&1 || bad "boot by id without .conf failed"
has "$(cat "$work/log")" "bootctl set-oneshot bootc_fedora-44-2.conf" "an id without .conf is accepted"
run 0 boot tilo-repair.efi >/dev/null 2>&1 || bad "boot of the repair entry failed"
has "$(cat "$work/log")" "bootctl set-oneshot tilo-repair.efi" "the Type #2 repair entry can be booted once"
if out="$(run 0 boot staged 2>&1)"; then bad "boot staged succeeded before the staged entry is in the menu"; fi
has "$out" "no entry in the boot menu starts the staged deployment" "a staged entry still under entries.staged is refused"
if out="$(run 0 boot bootmgfw 2>&1)"; then bad "an unknown entry was accepted"; fi
has "$out" "no boot entry called bootmgfw" "an unknown entry is refused, with the menu"
if out="$(run 1000 boot rollback 2>&1)"; then bad "boot succeeded without root"; fi
has "$out" "boot needs root" "booting once needs root"

# bootctl cannot be asked: the ids come off the ESP.
reset
echo "$work/root/boot" >"$work/esp-mount-unit"
FIXTURE_BOOTCTL_FAILS=1 run 0 boot rollback >/dev/null 2>&1 || true
out="$(FIXTURE_BOOTCTL_FAILS=1 run 0 list 2>&1)" || bad "list without bootctl failed: $out"
has "$out" "(the boot loader could not be asked; from the ESP:)" "the menu falls back to the ESP's own files"
has "$out" "tilo-repair.efi" "including the repair image"

# --- tilo rollback ----------------------------------------------------------------
reset
echo "$work/root/boot" >"$work/esp-mount-unit"
out="$(FIXTURE_ROLLBACK_DEFAULT=bootc_fedora-44-1.conf run 0 rollback --yes 2>&1)" \
  || bad "rollback failed: $out"
has "$(cat "$work/log")" "bootc rollback" "tilo rollback asks bootc first"
has "$out" "Rolling back to: registry.tilo.so/tilo/tilo-vm:main-6656acd-lenovo-83ed" "and names the image it returns to"
has "$out" "next boot: bootc_fedora-44-1.conf" "the next boot is checked against the boot loader on the found ESP"

# --- the status socket's one-word protocol ----------------------------------------
reset
echo "$work/root/boot" >"$work/esp-mount-unit"
out="$(printf 'text\n' | run 0 serve-status 2>&1)"
[ "$(head -1 <<<"$out")" = 0 ] && ok "serve-status answers text with its status first" \
  || bad "serve-status text: $out"
has "$out" "entry bootc_fedora-44-1.conf: on trial, 1 of 3 tries left" "and the listing after it"
out="$(printf 'json\n' | run 0 serve-status 2>&1)"
has "$out" "\"verity\": \"$booted\"" "serve-status answers json with bootc's status"
out="$(printf 'rollback --yes\n' | run 0 serve-status 2>&1)"
[ "$(head -1 <<<"$out")" = 2 ] && ok "any other request is refused" || bad "serve-status accepted: $out"
lacks "$(cat "$work/log")" "bootc rollback" "and nothing it sends runs"

# --- a board without the Boot Loader Interface ----------------------------------------
reset
rm -f "$work/efivars/LoaderDevicePartUUID-$guid"
cp -a "$work/disk/p5/." "$work/root/boot/"
out="$(run 0 list 2>&1)" || bad "list without LoaderDevicePartUUID failed: $out"
has "$out" "entry bootc_fedora-44-2.conf, booted now: good" "without LoaderDevicePartUUID, /boot's loader entries are used"
lacks "$(cat "$work/log")" "mount " "and nothing is mounted"

[ "$fails" = 0 ] || { echo "$fails failed" >&2; exit 1; }
echo "all passed"
