#!/usr/bin/env bash
# The boot-failure screen (os/usr/libexec/tilo-boot-failure): which step it
# names for a failed unit, what it draws through plymouth, how keys and taps
# choose, what it writes where the next start finds it, and the notice that
# start leaves for the desktop.
#
# The production script runs against a fixture ESP and EFI variables, with
# recording stand-ins for plymouth, the input helper, systemctl, journalctl,
# bootctl, cryptsetup and logger. Nothing here touches the machine.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
screen="$repo/os/usr/libexec/tilo-boot-failure"
work="${KEEP_WORK:-$(mktemp -d)}"
[ -n "${KEEP_WORK:-}" ] || trap 'rm -rf "$work"' EXIT
fails=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() {
  printf 'FAIL  %s\n' "$*"; fails=$((fails + 1))
  [ ! -s "$work/out" ] || sed 's/^/      out: /' "$work/out"
  [ ! -s "$work/calls" ] || sed 's/^/      call: /' "$work/calls"
}
check() {  # DESCRIPTION COMMAND...
  local what="$1"; shift
  if "$@"; then ok "$what"; else bad "$what"; fi
}

guid=4a67b082-0a4c-41cf-b6c7-440b29bb8c4f
current=$(printf 'a%.0s' {1..128})
previous=$(printf 'b%.0s' {1..128})

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

fixture() {
  rm -rf "$work/esp" "$work/run" "$work/state" "$work/calls" "$work/kmsg" "$work/pstore"
  install -d "$work/esp/loader/entries" "$work/esp/EFI/Linux/bootc" "$work/esp/EFI/tilo" \
    "$work/run" "$work/state" "$work/pstore"
  : >"$work/calls"; : >"$work/kmsg"
  printf 'title Tilo\nuki /EFI/Linux/bootc/bootc_composefs-%s.efi\n' "$current" \
    >"$work/esp/loader/entries/bootc_fedora-44-2+1-1.conf"
  printf 'title Tilo\nuki /EFI/Linux/bootc/bootc_composefs-%s.efi\n' "$previous" \
    >"$work/esp/loader/entries/bootc_fedora-44-1+2.conf"
  : >"$work/esp/EFI/Linux/bootc/bootc_composefs-$current.efi"
  : >"$work/esp/EFI/Linux/bootc/bootc_composefs-$previous.efi"
  touch -d '2026-10-09 12:00:00 UTC' "$work/esp/EFI/Linux/bootc/bootc_composefs-$previous.efi"
  efivar LoaderEntrySelected bootc_fedora-44-2.conf
  printf 'ID=fedora\n' >"$work/initrd-release"
  printf '%s\n' "${FAILED_UNITS:-sysroot.mount}" >"$work/failed"
}

# --- stand-ins -----------------------------------------------------------------
bin="$work/bin"
install -d "$bin"
cat >"$bin/plymouth" <<'EOF'
#!/bin/bash
case "$1" in
  --ping) [ -z "${NO_SPLASH:-}" ] ;;
  display-message) printf '%s\n' "${2#--text=tilo-failure:}" >>"$WORK/drawn" ;;
  *) printf 'plymouth %s\n' "$*" >>"$WORK/calls" ;;
esac
EOF
cat >"$bin/input" <<'EOF'
#!/bin/bash
# Each press waits until the screen has drawn once more, as a person would.
# A helper started again (after the recovery console) goes on from the press
# the last one stopped at.
while :; do
  done_n="$(cat "$WORK/pressed" 2>/dev/null || echo 0)"
  line="$(sed -n "$((done_n + 1))p" "$WORK/presses")"
  [ -n "$line" ] || exit 0
  drawn_before="$(cat "$WORK/drawn-before" 2>/dev/null || echo 0)"
  for _ in $(seq 100); do
    drawn_now="$(grep -c '^reset:' "$WORK/drawn" 2>/dev/null || true)"
    [ "${drawn_now:-0}" -gt "$drawn_before" ] && break
    sleep 0.02
  done
  printf '%s\n' "${drawn_now:-0}" >"$WORK/drawn-before"
  printf '%s\n' "$((done_n + 1))" >"$WORK/pressed"
  printf '%s\n' "$line"
done
EOF
cat >"$bin/systemctl" <<'EOF'
#!/bin/bash
case "$1" in
  list-units) sed 's/$/ loaded failed failed X/' "$WORK/failed" ;;
  reboot) printf 'reboot\n' >>"$WORK/calls" ;;
  *) printf 'systemctl %s\n' "$*" >>"$WORK/calls" ;;
esac
EOF
cat >"$bin/journalctl" <<'EOF'
#!/bin/bash
case " $* " in
  *" -b -1 "*) cat "$WORK/previous-journal" 2>/dev/null ;;
  *" -u "*) printf 'mount: /sysroot: wrong fs type, bad option, bad superblock\nFailed to mount sysroot.mount - /sysroot.\n' ;;
esac
EOF
cat >"$bin/bootctl" <<'EOF'
#!/bin/bash
printf 'bootctl %s\n' "$*" >>"$WORK/calls"
EOF
cat >"$bin/cryptsetup" <<'EOF'
#!/bin/bash
case "$1" in
  isLuks) [ "$2" = /dev/fixture-root ] ;;
  open) [ "$(cat)" = secret ] ;;
  *) exit 1 ;;
esac
EOF
cat >"$bin/logger" <<'EOF'
#!/bin/bash
{ printf 'logger %s\n' "$*"; [ "$1" = --journald ] && cat; } >>"$WORK/logged"
EOF
cat >"$bin/pcrextend" <<'EOF'
#!/bin/bash
printf 'pcrextend %s\n' "$*" >>"$WORK/calls"
[ -z "${PCR_FAILS:-}" ]
EOF
printf 'aaaabbbbccccddddeeeeffff00001111\n' >"$work/boot_id"
sed "s#/proc/sys/kernel/random/boot_id#$work/boot_id#" "$screen" >"$work/tilo-boot-failure"
chmod +x "$bin"/* "$work/tilo-boot-failure"

run() {  # ARGS... ; presses in $work/presses, stdin passed through
  rm -f "$work/drawn" "$work/pressed" "$work/drawn-before"
  WORK="$work" PATH="$bin:$PATH" \
    TILO_BOOT_FAILURE_LIB="$repo/os/usr/share/tilo/boot-failure" \
    TILO_BOOT_FAILURE_PLYMOUTH="$bin/plymouth" \
    TILO_BOOT_FAILURE_INPUT="$bin/input" \
    TILO_BOOT_FAILURE_RUN="$work/run" \
    TILO_BOOT_FAILURE_STATE="$work/state" \
    TILO_BOOT_FAILURE_PSTORE="$work/pstore" \
    TILO_BOOT_FAILURE_KMSG="$work/kmsg" \
    TILO_BOOT_FAILURE_SCREEN_HEIGHT=1000 \
    TILO_BOOT_FAILURE_PCREXTEND="$bin/pcrextend" \
    TILO_BOOT_FAILURE_TPM="${TPM:-$work/no-tpm}" \
    TILO_EFIVARS="$work/efivars" \
    TILO_INITRD_RELEASE="${INITRD:-$work/initrd-release}" \
    TILO_ESP="$work/esp" \
    TILO_ROOT_SOURCE_FILE="$work/root-source" \
    timeout 20 "$work/tilo-boot-failure" "$@"
}

# --- the step, in words ------------------------------------------------------------
step_of() {
  ( set +u; source <(sed -n '/^step_of_unit() {/,/^}/p' "$screen"); step_of_unit "$1" )
}
for pair in systemd-cryptsetup@luks\\x2dabc.service:unlock tilo-rollback-unlock.service:unlock \
            sysroot.mount:mount systemd-fsck-root.service:mount initrd-root-fs.target:mount \
            greetd.service:desktop tilo-boot-health.service:desktop \
            initrd-parse-etc.service:prepare systemd-tmpfiles-setup.service:prepare; do
  unit="${pair%:*}" want="${pair##*:}"
  check "$unit is said as \"$want\"" test "$(step_of "$unit")" = "$want"
done

# --- an initrd failure, by keyboard ------------------------------------------------
fixture
printf 'key down\nkey enter\n' >"$work/presses"
run show </dev/null >"$work/out" 2>&1 || true
check "the screen is drawn in the machine's locale (English without a note)" grep -qx 'reset:en-US' "$work/drawn"
check "its title" grep -q '^text:[0-9]*:title$' "$work/drawn"
check "and the step that failed, mounting the system" grep -q '^text:[0-9]*:step-mount$' "$work/drawn"
check "Try again is the first button, focused" grep -q '^button:[0-9]*:1:k=try-again$' "$work/drawn"
check "the previous version is offered with its date, in the catalogue's order" \
  grep -q '^button:[0-9]*:0:k=previous-dated.0|k=month-10|k=date.1|a=9|k=date.2|a=2026$' "$work/drawn"
check "no recovery console in an initrd without an encrypted root" bash -c "! grep -q 'k=console' '$work/drawn'"
check "the boot-menu key is named" grep -q '^text:[0-9]*:menu-hint$' "$work/drawn"
check "down, Enter starts the previous version once" grep -qx 'bootctl set-oneshot bootc_fedora-44-1.conf' "$work/calls"
check "and restarts" grep -qx 'reboot' "$work/calls"
check "the record is on the ESP" grep -qx 'step=mount' "$work/esp/EFI/tilo/boot-failure"
check "with the failed unit" grep -qx 'unit=sysroot.mount' "$work/esp/EFI/tilo/boot-failure"
check "its messages" grep -q '^log=mount: /sysroot: wrong fs type' "$work/esp/EFI/tilo/boot-failure"
check "and the start it failed in" grep -qx 'boot=aaaabbbbcccc' "$work/esp/EFI/tilo/boot-failure"
check "the kernel log has it at a level the pstore console keeps" \
  grep -qx '<2>tilo-boot-failure: step=mount' "$work/kmsg"

# --- the machine's language, from the ESP's note ------------------------------------
fixture
printf 'locale de-DE\n' >"$work/esp/EFI/tilo/boot-info"
printf 'key 1\n' >"$work/presses"
run show </dev/null >"$work/out" 2>&1 || true
check "a German machine's initrd draws in German" grep -qx 'reset:de-DE' "$work/drawn"
check "with the German date order" \
  grep -q '^button:[0-9]*:0:k=previous-dated.0|a=9|k=date.1|k=month-10|k=date.2|a=2026|k=previous-dated.1$' "$work/drawn"
check "1 tries again, the same entry" grep -qx 'bootctl set-oneshot bootc_fedora-44-2.conf' "$work/calls"

# --- by touch, with details --------------------------------------------------------
fixture
# 1000 px high: the title at 333, the first button at 451, one every 60; with
# the five lines of details the first button moves to 573.
printf 'tap 0.5 0.571\ntap 0.5 0.200\ntap 0.5 0.573\n' >"$work/presses"
run show </dev/null >"$work/out" 2>&1 || true
check "a tap on the third row shows the details" grep -q '^detail:[0-9]*:k=detail-unit.0|m=sysroot.mount$' "$work/drawn"
check "with the unit's messages" grep -q '^detail:[0-9]*:m=mount: /sysroot: wrong fs type' "$work/drawn"
check "and the start's number" grep -q '^detail:[0-9]*:k=detail-boot.0|m=aaaabbbbcccc$' "$work/drawn"
check "the button now hides them" grep -q 'k=hide-details$' "$work/drawn"
check "a tap beside every row does nothing; one on the first tries again" \
  test "$(grep -c '^bootctl' "$work/calls")" = 1 -a "$(grep -c 'set-oneshot bootc_fedora-44-2.conf' "$work/calls")" = 1

# --- without a splash ----------------------------------------------------------------
fixture
NO_SPLASH=1 run show <<<'2' >"$work/out" 2>&1 || true
check "without a splash the choices are a numbered menu" grep -q '^  2  Start the previous version from October 9, 2026$' "$work/out"
check "and 2 starts the previous version" grep -qx 'bootctl set-oneshot bootc_fedora-44-1.conf' "$work/calls"

# --- the recovery console ------------------------------------------------------------
# What a person types once the console asks (what was typed before is dropped).
typed_at_prompt() {  # LINE...
  for _ in $(seq 200); do
    grep -q 'Enter the disk passphrase' "$work/out" 2>/dev/null && break
    sleep 0.05
  done
  sleep 0.3
  printf '%s\n' "$@"
}
FAILED_UNITS=systemd-cryptsetup@root.service fixture
printf '/dev/fixture-root\n' >"$work/root-source"
printf 'key 3\nkey 1\n' >"$work/presses"
typed_at_prompt 'wrong' 'secret' 'echo SHELL-OPENED' 'exit' \
  | run show >"$work/out" 2>&1 || true
check "an encrypted root offers the recovery console" grep -q '^button:[0-9]*:0:k=console$' "$work/drawn"
check "and names the disk step" grep -q '^text:[0-9]*:step-unlock$' "$work/drawn"
check "a wrong passphrase is refused" grep -q "That passphrase doesn't unlock this disk." "$work/out"
check "the right one opens a shell" grep -qx 'SHELL-OPENED' "$work/out"

fixture
printf '/dev/fixture-root\n' >"$work/root-source"
: >"$work/tpm"
printf 'key 3\nkey 1\n' >"$work/presses"
typed_at_prompt 'secret' 'echo SHELL-OPENED' 'exit' \
  | TPM="$work/tpm" run show >"$work/out" 2>&1 || true
check "with a TPM, PCR 11 is extended before the shell opens" grep -qx 'pcrextend --pcr=11 tilo-recovery-console' "$work/calls"
check "and then it opens" grep -qx 'SHELL-OPENED' "$work/out"

fixture
printf '/dev/fixture-root\n' >"$work/root-source"
printf 'key 3\nkey 1\n' >"$work/presses"
typed_at_prompt 'secret' 'echo SHELL-OPENED' 'exit' \
  | PCR_FAILS=1 TPM="$work/tpm" run show >"$work/out" 2>&1 || true
check "a TPM that cannot be extended keeps the console shut" bash -c "! grep -qx SHELL-OPENED '$work/out'"

# --- the next start --------------------------------------------------------------------
fixture
printf 'boot=0123456789ab\ntime=1792000000\nstep=mount\nunit=sysroot.mount\nentry=bootc_fedora-44-2.conf\nlog=mount: /sysroot: wrong fs type\n' \
  >"$work/esp/EFI/tilo/boot-failure"
INITRD="$work/not-an-initrd" run import >"$work/out" 2>&1
check "import turns the ESP's record into the desktop's notice" grep -qx 'id=0123456789ab' "$work/state/notice"
check "naming the step" grep -qx 'step=mount' "$work/state/notice"
check "and not as a fallback while this entry has tries left" grep -qx 'fallback=0' "$work/state/notice"
check "it is in this start's journal" grep -q 'failed at mount' "$work/logged"
before="$(stat -c %Y "$work/state/notice")"; rm -f "$work/logged"
INITRD="$work/not-an-initrd" run import >"$work/out" 2>&1
check "the same record again changes nothing" test ! -e "$work/logged" -a "$(stat -c %Y "$work/state/notice")" = "$before"

fixture
mv "$work/esp/loader/entries/bootc_fedora-44-2+1-1.conf" "$work/esp/loader/entries/bootc_fedora-44-2+0-2.conf"
efivar LoaderEntrySelected bootc_fedora-44-1.conf
INITRD="$work/not-an-initrd" run import >"$work/out" 2>&1
check "a newer entry that used its tries up, with no record, is a fallback notice" grep -qx 'fallback=1' "$work/state/notice"
check "named for the deployment that failed" grep -qx "id=fallback-$current" "$work/state/notice"

fixture
printf 'MESSAGE_ID=5b0d1e6f8a2c4e3fa1c7d9b2e4f60a18\nTILO_RECORD=boot=ffff00001111\nTILO_RECORD=time=1792000500\nTILO_RECORD=step=desktop\nTILO_RECORD=unit=greetd.service\nTILO_RECORD=log=greetd: greeter exited\n\n' \
  >"$work/previous-journal"
INITRD="$work/not-an-initrd" run import >"$work/out" 2>&1
check "a failure after switch-root comes from the previous start's journal" grep -qx 'step=desktop' "$work/state/notice"
check "with its messages whole" grep -qx 'log=greetd: greeter exited' "$work/state/notice"
rm -f "$work/previous-journal"

fixture
printf 'boot=aaaabbbbcccc\ntime=1792000000\nstep=mount\n' >"$work/esp/EFI/tilo/boot-failure"
INITRD="$work/not-an-initrd" run import >"$work/out" 2>&1
check "this start's own record is no notice" test ! -e "$work/state/notice"

# --- the catalogues ----------------------------------------------------------------------
en="$(sed -n 's/^\(boot-failure-[a-z0-9-]*\) = .*/\1/p' "$repo/os/usr/share/tilo/boot-failure/locales/en-US.ftl" | sort)"
for ftl in "$repo"/os/usr/share/tilo/boot-failure/locales/*.ftl; do
  check "$(basename "$ftl") has every word the screen uses" \
    test "$(sed -n 's/^\(boot-failure-[a-z0-9-]*\) = .*/\1/p' "$ftl" | sort)" = "$en"
done
for key in $(grep -o 'msg [a-z0-9"-]*boot-failure-[a-z0-9-]*' "$screen" | grep -o 'boot-failure-[a-z0-9-]*' | sort -u); do
  case "$key" in boot-failure-step-|boot-failure-month-) continue ;; esac
  check "the catalogue has $key" grep -q "^$key = " "$repo/os/usr/share/tilo/boot-failure/locales/en-US.ftl"
done

[ "$fails" = 0 ] || { printf '%d failed\n' "$fails"; exit 1; }
printf 'the boot-failure screen holds\n'
