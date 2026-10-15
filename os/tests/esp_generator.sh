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
    "$@" bash "$gen" "$work/$name" 2>"$work/$name.log"
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

# 2. Layouts where /boot is the ESP, or no /boot is named, are left alone.
for case in \
  "sealed|root=tilo-auto rd.luks.options=tpm2-device=auto,fido2-device=auto composefs=abc" \
  "dev|composefs=abc root=tilo-auto rw systemd.mount-extra=UUID=137A-C31F:/boot:auto:ro,nosuid,nodev,noexec" \
  "fat|composefs=abc systemd.mount-extra=UUID=137A-C31F:/boot:vfat:ro" \
  "other-path|composefs=abc systemd.mount-extra=LABEL=DATA:/srv:ext4:ro"; do
  name="${case%%|*}"
  generate "$name" "${case#*|}" || bad "the generator failed on the $name command line"
  if [ -e "$work/$name/tilo-esp-mount.service" ]; then
    bad "$name: /boot is not a separate Linux partition, yet the generator mounted the ESP at /boot/efi"
  else
    ok "$name: no extra ESP mount"
  fi
done

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
