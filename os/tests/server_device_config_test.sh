#!/usr/bin/env bash
# tilo-server-device-config: applies the hostname, drives and network a
# server's entry asks for, against a fake root — NetworkManager keyfiles in a
# temporary directory, drives from a fake lsblk, and nothing of this machine's
# network or disks.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
helper="$repo/os/usr/libexec/tilo-server-device-config"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

nm_dir="$work/networkmanager"
state="$work/state"
mkdir -p "$nm_dir" "$state"
request="$state/device-config.request"
report="$state/device-config.report"
applied_at="$state/device-config.applied-at"
confirmed="$state/device-config.confirmed"
nmcli_log="$work/nmcli.log"
hostnamectl_log="$work/hostnamectl.log"
wipefs_log="$work/wipefs.log"
devices="$work/devices"

cat >"$work/nmcli" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$TILO_TEST_NMCLI_LOG"
EOF
cat >"$work/hostnamectl" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$TILO_TEST_HOSTNAMECTL_LOG"
EOF
cat >"$work/wipefs" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$TILO_TEST_WIPEFS_LOG"
EOF
cat >"$work/findmnt" <<'EOF'
#!/bin/sh
printf '/dev/sda2\n'
EOF
cat >"$work/lsblk" <<'EOF'
#!/bin/sh
case "$*" in
    -P*|*" -P "*) cat "$TILO_TEST_LSBLK_DEVICES" ;;
    -s*|*" -s "*) printf 'sda2\nsda\n' ;;
esac
EOF
chmod +x "$work/nmcli" "$work/hostnamectl" "$work/wipefs" "$work/findmnt" "$work/lsblk"

cat >"$devices" <<'EOF'
NAME="sda" TYPE="disk" SERIAL="ROOT1" WWN="0xroot" FSTYPE="" MOUNTPOINT=""
NAME="sda1" TYPE="part" SERIAL="" WWN="" FSTYPE="vfat" MOUNTPOINT="/boot/efi"
NAME="sda2" TYPE="part" SERIAL="" WWN="" FSTYPE="crypto_LUKS" MOUNTPOINT=""
NAME="sdb" TYPE="disk" SERIAL="DATA1" WWN="0xdata" FSTYPE="" MOUNTPOINT=""
NAME="sdc" TYPE="disk" SERIAL="FULL1" WWN="0xfull" FSTYPE="ext4" MOUNTPOINT=""
EOF

export TILO_DEVICE_CONFIG_REQUEST="$request"
export TILO_DEVICE_CONFIG_REPORT="$report"
export TILO_DEVICE_CONFIG_APPLIED_AT="$applied_at"
export TILO_DEVICE_CONFIG_CONFIRMED="$confirmed"
export TILO_DEVICE_CONFIG_STATE="$state"
export TILO_DEVICE_CONFIG_REVERT_AFTER=0
export TILO_NETWORKMANAGER_DIR="$nm_dir"
export TILO_NMCLI="$work/nmcli"
export TILO_HOSTNAMECTL="$work/hostnamectl"
export TILO_LSBLK="$work/lsblk"
export TILO_FINDMNT="$work/findmnt"
export TILO_WIPEFS="$work/wipefs"
export TILO_ETC="$work/etc"
export TILO_TEST_NMCLI_LOG="$nmcli_log"
export TILO_TEST_HOSTNAMECTL_LOG="$hostnamectl_log"
export TILO_TEST_WIPEFS_LOG="$wipefs_log"
export TILO_TEST_LSBLK_DEVICES="$devices"

revision=0123456789abcdef0123456789abcdef

reset() {
    : >"$nmcli_log"
    : >"$hostnamectl_log"
    : >"$wipefs_log"
    rm -f "$confirmed" "$report" "$applied_at"
    rm -f "$nm_dir"/*.nmconnection
    rm -rf "$state/network-previous"
}

report_field() { sed -n "s/^$1	//p" "$report"; }

# The daemon's confirmation that the machine is still reached.
keep() { printf 'revision\t%s\n' "$revision" >"$confirmed"; }

fail() { printf 'tilo-server-device-config test: %s\n' "$*" >&2; exit 1; }

# A configuration is applied whole: the hostname through hostnamectl, the
# drives checked by the identity the entry names, and the network written as
# NetworkManager keyfiles and brought up.
reset
cat >"$request" <<EOF
revision|$revision
hostname|nas
disk|system|ROOT1|0
disk|data|DATA1|0
interface|eno1|dhcp|||||0
interface|enp2|static|10.0.0.5/24|10.0.0.1|1.1.1.1,9.9.9.9||0
EOF
keep
"$helper"
[ "$(report_field state)" = applied ] || fail "a configuration was not applied: $(cat "$report")"
grep -qx 'set-hostname nas' "$hostnamectl_log" || fail 'the hostname did not reach hostnamectl'
grep -qx 'connection reload' "$nmcli_log" || fail 'the keyfiles were not reloaded'
grep -qx 'connection up tilo-eno1' "$nmcli_log" || fail 'the dhcp interface was not brought up'
grep -qx 'connection up tilo-enp2' "$nmcli_log" || fail 'the static interface was not brought up'
grep -q '^method=manual$' "$nm_dir/tilo-enp2.nmconnection" || fail 'a static interface is not manual'
grep -q '^address1=10.0.0.5/24$' "$nm_dir/tilo-enp2.nmconnection" || fail 'the address is not written'
grep -q '^gateway=10.0.0.1$' "$nm_dir/tilo-enp2.nmconnection" || fail 'the gateway is not written'
grep -q '^dns=1.1.1.1;9.9.9.9;$' "$nm_dir/tilo-enp2.nmconnection" || fail 'the dns servers are not written'
grep -q '^method=auto$' "$nm_dir/tilo-eno1.nmconnection" || fail 'a dhcp interface is not automatic'
[ "$(stat -c '%a' "$nm_dir/tilo-enp2.nmconnection")" = 600 ] || fail 'a keyfile is readable by others'
[ "$(report_field revision)" = "$revision" ] || fail 'the report names another revision'
[ -r "$applied_at" ] || fail 'the helper did not say it applied'

# A drive that holds something is refused unless the entry confirmed erasing
# it by naming the drive; nothing else is applied then either.
reset
cat >"$request" <<EOF
revision|$revision
hostname|nas
disk|data|FULL1|0
EOF
if "$helper" 2>/dev/null; then fail 'a drive holding data was taken without confirmation'; fi
[ "$(report_field state)" = refused ] || fail "a drive holding data was not refused: $(cat "$report")"
grep -q 'holds data' <<<"$(report_field detail)" || fail 'the refusal does not say why'
[ ! -s "$hostnamectl_log" ] || fail 'a refused configuration was half applied'
[ ! -s "$nmcli_log" ] || fail 'a refused configuration touched the network'

# With the confirmation the drive is erased by identity.
reset
cat >"$request" <<EOF
revision|$revision
disk|data|FULL1|1
EOF
"$helper"
[ "$(report_field state)" = applied ] || fail "a confirmed drive was not applied: $(cat "$report")"
grep -qx -- '-a /dev/sdc' "$wipefs_log" || fail 'the confirmed drive was not erased'

# The disk boots from is never repurposed, and a data drive is never
# the system disk.
for request_body in 'disk|data|ROOT1|1' 'disk|system|DATA1|0' 'disk|mirror|ROOT1|1'; do
    reset
    {
        printf 'revision|%s\n' "$revision"
        printf '%s\n' "$request_body"
    } >"$request"
    if "$helper" 2>/dev/null; then fail "'$request_body' was applied"; fi
    [ "$(report_field state)" = refused ] || fail "'$request_body' was not refused"
done

# A static interface without an address is refused whole.
reset
cat >"$request" <<EOF
revision|$revision
interface|enp2|static|||||0
EOF
if "$helper" 2>/dev/null; then fail 'a static interface without an address was applied'; fi
[ "$(report_field state)" = refused ] || fail 'a static interface without an address was not refused'

# A request naming a setting this helper does not set is refused whole.
reset
cat >"$request" <<EOF
revision|$revision
schedule|daily
EOF
if "$helper" 2>/dev/null; then fail 'a request naming an unknown setting was applied'; fi
[ "$(report_field state)" = refused ] || fail 'a request naming an unknown setting was not refused'

# A network change nobody confirms is put back: the keyfiles it replaced come
# back, the ones it wrote go, and the report says it was reverted.
reset
printf 'previous\n' >"$nm_dir/tilo-eno9.nmconnection"
chmod 600 "$nm_dir/tilo-eno9.nmconnection"
cat >"$request" <<EOF
revision|$revision
interface|eno1|dhcp|||||0
EOF
"$helper"
[ "$(report_field state)" = reverted ] || fail "an unconfirmed network was not reverted: $(cat "$report")"
[ "$(cat "$nm_dir/tilo-eno9.nmconnection")" = previous ] || fail 'the previous keyfile was not restored'
[ ! -e "$nm_dir/tilo-eno1.nmconnection" ] || fail 'the new keyfile stayed after a revert'
grep -qx 'connection up tilo-eno9' "$nmcli_log" || fail 'the previous interface was not brought back'

# A confirmation of the revision it applied keeps it.
reset
printf 'revision\t%s\n' "$revision" >"$confirmed"
cat >"$request" <<EOF
revision|$revision
interface|eno1|dhcp|||||0
EOF
keep
"$helper"
[ "$(report_field state)" = applied ] || fail "a confirmed network was reverted: $(cat "$report")"
[ -r "$nm_dir/tilo-eno1.nmconnection" ] || fail 'a confirmed keyfile was removed'
# A confirmation of another revision keeps nothing.
reset
printf 'revision\t0123456789abcdef0123456789abcdee\n' >"$confirmed"
"$helper"
[ "$(report_field state)" = reverted ] || fail 'a confirmation of another revision kept the change'

# A machine with no request applies nothing.
reset
rm -f "$request"
"$helper"
[ ! -e "$report" ] || fail 'a machine with no request wrote a report'

echo 'tilo-server-device-config: ok'
