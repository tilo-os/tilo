#!/usr/bin/env bash
# The ESP is reachable at /boot/efi on an installed system, whose /boot is a
# separate ext4 partition, and every unit that needs the ESP waits for it.
#
# Without it an installed system's boot-entry maintenance found no boot loader
# and gave no update a try counter (so a broken update never rolled back), and
# systemd-bless-boot found no ESP to bless a good boot on.
#
# Runs the production generator against fixture command lines and EFI
# variables, the production mount helper against a recording mount(8), and
# reads the production units and the installer's contracts.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
gen="$repo/os/usr/lib/systemd/system-generators/tilo-esp-generator"
helper="$repo/os/usr/libexec/tilo-esp-mount"
units="$repo/os/usr/lib/systemd/system"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
fails=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*"; fails=$((fails + 1)); }

# The installed target's own /boot mount, exactly as the installer image signs
# it into the target UKI, so the two cannot drift apart.
boot_contract="$(grep -o 'systemd\.mount-extra=LABEL=TILO_BOOT:/boot:[^ ]*' \
  "$repo/os/Containerfile.installer" | sort -u)"
[ "$(grep -c . <<<"$boot_contract")" = 1 ] \
  || { echo "FAIL  the installer image signs no single TILO_BOOT mount into the target UKI" >&2; exit 1; }
installed="root=tilo-auto rd.luks.options=tpm2-device=auto,fido2-device=auto tilo.mode=normal $boot_contract composefs=abc"
esp_uuid=0FC63DAF-8483-4772-8E79-3D69D8477DE4
esp_lower="$(tr '[:upper:]' '[:lower:]' <<<"$esp_uuid")"

# A LoaderDevicePartUUID variable as efivarfs shows it: four attribute bytes,
# then the UUID in UTF-16LE with a terminating NUL, upper case as sd-boot
# writes it.
efivar() {
  install -d "$work/efivars"
  {
    printf '\x06\x00\x00\x00'
    printf '%s' "$1" | sed 's/./&\n/g' | while IFS= read -r c; do
      [ -n "$c" ] && printf '%s\x00' "$c"
    done
    printf '\x00\x00'
  } >"$work/efivars/LoaderDevicePartUUID-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f"
}

# The ESP's own file system, as blkid reports it: FAT volume 137A-C31F,
# label ESP. TEST_BLKID_FAIL makes the probe fail.
install -d "$work/stub"
cat >"$work/stub/blkid" <<'STUB'
#!/usr/bin/env bash
[ -z "${TEST_BLKID_FAIL:-}" ] || exit 2
case "$*" in
  "-o value -s UUID "*) echo 137A-C31F ;;
  "-o value -s LABEL "*) echo ESP ;;
  *) exit 2 ;;
esac
STUB
chmod +x "$work/stub/blkid"

# generate NAME CMDLINE [ENV...]: run the generator into $work/NAME.
generate() {
  local name="$1" cmdline="$2"
  shift 2
  printf '%s\n' "$cmdline" >"$work/$name.cmdline"
  rm -rf "${work:?}/$name"
  env TILO_ESP_INITRD_RELEASE="$work/no-initrd-release" \
    TILO_ESP_CMDLINE_FILE="$work/$name.cmdline" \
    TILO_ESP_EFIVARS="$work/efivars" \
    TILO_ESP_FSTAB="$work/fstab" \
    TILO_ESP_BLKID="$work/stub/blkid" \
    "$@" bash "$gen" "$work/$name" 2>"$work/$name.log"
}

# expect NAME CMDLINE FSTAB yes|no WHY [ENV...]: the generator mounts the ESP
# at /boot/efi (yes) or leaves it (no).
expect() {
  local name="$1" cmdline="$2" fstab_line="$3" want="$4" why="$5" got=no
  shift 5
  printf '%s' "$fstab_line" >"$work/fstab"
  generate "$name" "$cmdline" "$@" || bad "the generator failed: $name"
  [ ! -e "$work/$name/tilo-esp-mount.service" ] || got=yes
  if [ "$got" = "$want" ]; then
    ok "$name: $why"
  else
    bad "$name: wanted mount=$want ($why), got mount=$got: $(cat "$work/$name.log")"
  fi
  : >"$work/fstab"
}

: >"$work/fstab"
efivar "$esp_uuid"

# 1. The installed target: the booted ESP, read-only, at /boot/efi.
if generate installed "$installed"; then
  unit="$work/installed/tilo-esp-mount.service"
  if [ -f "$unit" ]; then
    want="ExecStart=/usr/libexec/tilo-esp-mount /dev/disk/by-partuuid/$esp_lower /boot/efi"
    grep -qxF "$want" "$unit" \
      && ok "the installed target mounts the ESP the boot loader names at /boot/efi ($esp_lower)" \
      || bad "tilo-esp-mount.service runs '$(sed -n 's/^ExecStart=//p' "$unit")', not the boot loader's ESP at /boot/efi"
    device="dev-disk-by\\x2dpartuuid-${esp_lower//-/\\x2d}.device"
    grep -qxF "Wants=$device" "$unit" && grep -qxF "After=$device local-fs-pre.target" "$unit" \
      && ok "it waits for that partition's device unit ($device)" \
      || bad "tilo-esp-mount.service does not wait for $device"
    grep -qx 'Before=local-fs.target' "$unit" \
      && ok "it is in place before local-fs.target, so before anything that reads the ESP" \
      || bad "tilo-esp-mount.service is not ordered before local-fs.target"
    grep -qx 'RequiresMountsFor=/boot' "$unit" \
      && ok "it mounts onto the separate /boot, after /boot" || bad "tilo-esp-mount.service does not wait for /boot"
    grep -qx 'ConditionPathIsMountPoint=!/boot/efi' "$unit" \
      || bad "tilo-esp-mount.service would mount over an existing /boot/efi"
    [ "$(readlink "$work/installed/local-fs.target.wants/tilo-esp-mount.service" 2>/dev/null)" = ../tilo-esp-mount.service ] \
      && ok "local-fs.target wants it (an ESP that fails to mount does not fail the boot)" \
      || bad "nothing pulls tilo-esp-mount.service in"
    grep -q '^Requires=' "$unit" && bad "a missing ESP would fail tilo-esp-mount's dependencies outright"
  else
    bad "the installed target's command line produced no ESP mount: $(cat "$work/installed.log")"
  fi
else
  bad "the generator failed on the installed target's command line: $(cat "$work/installed.log")"
fi

# 2. Which /boot is separate. An update's own UKI carries no /boot mount; on an
#    installed system bootc's /etc/fstab still mounts TILO_BOOT there. The
#    sealed disk's fstab and a dev disk's command line mount the ESP itself.
update="root=tilo-auto rd.luks.options=tpm2-device=auto,fido2-device=auto composefs=abc"
boot_uuid=4f6a1c2e-0d1b-4c4e-9a31-2b7f61a0c9d1
expect updated-installed "$update" "UUID=$boot_uuid /boot ext4 ro,nosuid,nodev,noexec 1 2"$'\n' yes \
  "an update booted on an installed system: fstab's ext4 /boot is separate"
expect updated-installed-auto "$update" "UUID=$boot_uuid /boot auto ro 1 2"$'\n' yes \
  "an fstab /boot of type auto that is not the ESP is separate"
expect sealed "$update" $'UUID=137A-C31F /boot vfat umask=0077 0 2\n' no \
  "the sealed disk's fstab mounts the ESP itself at /boot"
expect sealed-auto "$update" $'UUID=137A-C31F /boot auto ro 0 2\n' no \
  "an fstab /boot of type auto that is the ESP is left alone"
expect commented "$update" "# UUID=$boot_uuid /boot ext4 ro 1 2"$'\n' yes "a commented-out fstab line claims no /boot, so the ESP is /boot"
expect no-boot "$update" '' yes "nothing mounts /boot (an update booted on an installed system)"
unit="$work/no-boot/tilo-esp-mount.service"
if [ -f "$unit" ]; then
  grep -qxF "ExecStart=/usr/libexec/tilo-esp-mount /dev/disk/by-partuuid/$esp_lower /boot" "$unit" \
    && grep -qx 'ConditionPathIsMountPoint=!/boot' "$unit" \
    && ! grep -q '^RequiresMountsFor=' "$unit" \
    && ok "no-boot: the ESP becomes /boot itself, as on the sealed disk" \
    || bad "no-boot: the ESP is not mounted at /boot: $(sed -n 's/^ExecStart=//p' "$unit")"
fi
rm -rf "$work/efivars"
expect no-boot-no-loader "$update" '' no "no /boot and no boot loader report: nothing is guessed"
[ ! -s "$work/no-boot-no-loader.log" ] || bad "a board with no boot loader report logs an ESP complaint at every boot"
efivar "$esp_uuid"
expect dev "composefs=abc root=tilo-auto rw systemd.mount-extra=UUID=137A-C31F:/boot:auto:ro,nosuid,nodev,noexec" '' no \
  "a dev disk's /boot is its ESP (by FAT volume id)"
expect dev-label "composefs=abc systemd.mount-extra=LABEL=ESP:/boot:auto:ro" '' no \
  "a /boot named by the ESP's label is the ESP"
expect dev-partuuid "composefs=abc systemd.mount-extra=PARTUUID=$esp_uuid:/boot:auto:ro" '' no \
  "a /boot named by the ESP's partition UUID is the ESP"
expect other-partuuid "composefs=abc systemd.mount-extra=PARTUUID=11111111-2222-3333-4444-555555555555:/boot:auto:ro" '' yes \
  "a /boot on another partition is separate"
expect fat "composefs=abc systemd.mount-extra=UUID=137A-C31F:/boot:vfat:ro" '' no "a FAT /boot is not separate"
expect other-path "composefs=abc systemd.mount-extra=LABEL=DATA:/srv:ext4:ro" '' yes "a mount elsewhere leaves /boot unclaimed, so the ESP is /boot"
expect auto-unprobed "composefs=abc systemd.mount-extra=UUID=$boot_uuid:/boot:auto:ro" '' no \
  "a /boot of type auto that cannot be compared with the ESP is left alone" TEST_BLKID_FAIL=1

expect live-installer "$update tilo.install" '' no \
  "the live installer: it refuses to install to a mounted ESP, and a staged one boots from it"

# 3. The initrd mounts no /boot.
touch "$work/initrd-release"
generate initrd "$installed" TILO_ESP_INITRD_RELEASE="$work/initrd-release" \
  || bad "the generator failed in the initrd"
[ ! -e "$work/initrd/tilo-esp-mount.service" ] && ok "nothing in the initrd" \
  || bad "the generator mounted the ESP in the initrd"

# 4. An administrator's own /boot/efi line wins.
printf 'UUID=ABCD-1234 /boot/efi vfat ro 0 2\n' >"$work/fstab"
generate admin-fstab "$installed" || bad "the generator failed beside an fstab /boot/efi"
[ ! -e "$work/admin-fstab/tilo-esp-mount.service" ] && ok "an fstab /boot/efi is left to fstab" \
  || bad "the generator shadows an fstab /boot/efi"
: >"$work/fstab"

# 5. Without the boot loader's report there is no ESP to name, and no guess.
rm -rf "$work/efivars"
generate no-loader "$installed" || bad "the generator failed without LoaderDevicePartUUID"
if [ ! -e "$work/no-loader/tilo-esp-mount.service" ] && grep -q LoaderDevicePartUUID "$work/no-loader.log"; then
  ok "no LoaderDevicePartUUID: no mount, and the reason is logged"
else
  bad "without LoaderDevicePartUUID the generator guessed an ESP, or said nothing"
fi
efivar "not-a-uuid"
generate bad-loader "$installed" || bad "the generator failed on a malformed LoaderDevicePartUUID"
[ ! -e "$work/bad-loader/tilo-esp-mount.service" ] && ok "a malformed LoaderDevicePartUUID mounts nothing" \
  || bad "a malformed LoaderDevicePartUUID became a mount"

# 6. The helper: a writable FAT superblock, seen read-only at /boot/efi.
#    bootc mounts the same ESP read-write to stage an update; a read-only
#    superblock refuses that with EBUSY. mount(8) and friends are recorded.
bin="$work/bin"
install -d "$bin" "$work/dev" "$work/where" "$work/mounts"
touch "$work/dev/esp"
for tool in mount umount mountpoint findmnt; do
  cat >"$bin/$tool" <<'EOF'
#!/usr/bin/env bash
tool="$(basename "$0")"
printf '%s %s\n' "$tool" "$*" >>"$TEST_LOG"
key() { printf '%s' "$1" | tr / _; }
case "$tool" in
  mount)
    if [ "$1" = --bind ]; then
      opts="$3"; [ -n "${TEST_BIND_IGNORES_RO:-}" ] && opts="rw"
      printf '%s\n' "$opts" >"$TEST_MOUNTS/$(key "$5")"
    else
      printf '%s\n' "$4" >"$TEST_MOUNTS/$(key "$6")"
    fi ;;
  umount) rm -f "$TEST_MOUNTS/$(key "$1")" ;;
  mountpoint) [ -e "$TEST_MOUNTS/$(key "$2")" ] ;;
  findmnt) cat "$TEST_MOUNTS/$(key "${@: -1}")" 2>/dev/null ;;
esac
EOF
  chmod +x "$bin/$tool"
done
run_helper() {
  : >"$work/helper.log"
  env PATH="$bin:$PATH" TEST_LOG="$work/helper.log" TEST_MOUNTS="$work/mounts" \
    TILO_ESP_MOUNT_STAGE="$work/stage" "$@" bash "$helper" "$work/dev/esp" "$work/where" \
    2>"$work/helper.err"
}
key() { printf '%s' "$1" | tr / _; }
if run_helper; then
  first="$(grep '^mount ' "$work/helper.log" | head -1)"
  bind="$(grep '^mount --bind' "$work/helper.log" | head -1)"
  case "$first" in
    "mount -t vfat -o rw,"*" $work/dev/esp $work/stage/esp."*) ok "the superblock is mounted read-write, at a root-only staging point" ;;
    *) bad "the ESP superblock is not mounted read-write first: '$first'" ;;
  esac
  case "$bind" in
    "mount --bind -o ro,"*" $work/where") ok "and bound read-only at /boot/efi ($bind)" ;;
    *) bad "/boot/efi is not a read-only bind: '$bind'" ;;
  esac
  [ "$(cat "$work/mounts/$(key "$work/where")" 2>/dev/null)" = "ro,nosuid,nodev,noexec" ] \
    || bad "the view at /boot/efi is not ro,nosuid,nodev,noexec"
  if ls "$work/mounts" | grep -q stage; then
    bad "the writable staging mount was left behind"
  else
    ok "the writable staging mount is gone: nothing writable remains in the global namespace"
  fi
  [ ! -e "$work/stage" ] || bad "the staging directory was left behind"
else
  bad "tilo-esp-mount failed: $(cat "$work/helper.err")"
fi
# Already mounted: left alone.
if run_helper && ! grep -q '^mount ' "$work/helper.log"; then
  ok "an existing /boot/efi mount is left alone"
else
  bad "tilo-esp-mount mounted over an existing /boot/efi"
fi
rm -f "$work/mounts/"*
# A bind that comes up writable is undone and fails.
if run_helper TEST_BIND_IGNORES_RO=1; then
  bad "a writable /boot/efi was accepted"
elif [ ! -e "$work/mounts/$(key "$work/where")" ] && grep -q writable "$work/helper.err"; then
  ok "a /boot/efi that came up writable is unmounted and reported"
else
  bad "a writable /boot/efi was left mounted, or not reported"
fi
rm -f "$work/mounts/"*
# A missing device fails without mounting anything.
rm -f "$work/dev/esp"
if run_helper; then
  bad "tilo-esp-mount succeeded without its ESP"
elif grep -q '^mount ' "$work/helper.log"; then
  bad "tilo-esp-mount mounted something without its ESP"
else
  ok "a missing ESP fails before any mount"
fi

# 7. The installer leaves a mount point for it on TILO_BOOT: /boot/efi is made
#    after the new /boot is mounted, in both ways it prepares a target.
encrypt="$repo/os/usr/libexec/tilo-disk-encrypt"
made="$(awk '/^[[:space:]]*mount "\$boot(_part)?" "\$mnt\/boot"$/ { b = NR } /install -d -m 0755 "\$mnt\/boot\/efi"/ && b && NR > b { n++ ; b = 0 } END { print n + 0 }' "$encrypt")"
[ "$made" -ge 2 ] && ok "tilo-disk-encrypt makes /boot/efi on TILO_BOOT in both layouts" \
  || bad "tilo-disk-encrypt does not make /boot/efi on the mounted TILO_BOOT ($made of 2 layouts)"

# 8. Everything that reads or writes the ESP waits for it.
for unit in tilo-boot-entries.service tilo-esp-write@.service \
  tilo-deployment-update.service tilo-deployment-rollback.service \
  systemd-bless-boot.service.d/10-tilo-writable-esp.conf; do
  line="$(grep -E '^RequiresMountsFor=' "$units/$unit" | head -1)"
  case " ${line#RequiresMountsFor=} " in
    *" /boot/efi "*) ok "$unit waits for /boot/efi" ;;
    *) bad "$unit does not wait for /boot/efi (${line:-no RequiresMountsFor})" ;;
  esac
done

# 9. The image installs both.
grep -q 'os/usr/lib/systemd/system-generators/tilo-esp-generator' "$repo/os/Containerfile" \
  && ok "the image installs tilo-esp-generator" || bad "os/Containerfile does not install tilo-esp-generator"
grep -q 'os/usr/libexec/tilo-esp-mount' "$repo/os/Containerfile" \
  && ok "the image installs tilo-esp-mount" || bad "os/Containerfile does not install tilo-esp-mount"
[ -x "$gen" ] || bad "tilo-esp-generator is not executable"
[ -x "$helper" ] || bad "tilo-esp-mount is not executable"

[ "$fails" -eq 0 ] || { printf '%s failure(s)\n' "$fails"; exit 1; }
printf 'PASS esp_generator\n'
