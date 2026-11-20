#!/usr/bin/env bash
# A start uses the tilo-base SELinux module its image ships, not whatever its
# policy store kept: the refresh unit compares the store's module with the
# image's copy and installs it again when they differ, and costs one
# comparison when they do not.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="$repo/os/usr/libexec/tilo-selinux-policy"
unit="$repo/os/usr/lib/systemd/system/tilo-selinux-policy.service"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail() { printf 'selinux-policy-refresh: %s\n' "$*" >&2; exit 1; }

command -v bzip2 >/dev/null || fail "bzip2 is needed for the store's form of a module"
[ -x "$script" ] || fail "the refresh script is not executable"
bash -n "$script"

shipped="$work/shipped.cil"
store="$work/store"
active="$store/active/modules/400/tilo-base/cil"
calls="$work/calls"
mkdir -p "$(dirname "$active")"
printf '(allow a b (file (read)))\n' >"$shipped"
: >"$calls"

# Stands in for semodule: notes each invocation, and installs the file the
# script asked for the way a store keeps it (bzip2, semanage.conf's
# bzip-blocksize) when STUB_FAIL is not set.
cat >"$work/semodule" <<'STUB'
#!/usr/bin/bash
printf '%s\n' "$*" >>"$SEMODULE_CALLS"
[ "${STUB_FAIL:-0}" = 0 ] || exit 1
[ "$1" = -i ] || exit 2
bzip2 -c "$2" >"$STUB_ACTIVE"
STUB
chmod +x "$work/semodule"

refresh() {
  TILO_SELINUX_SHIPPED="$shipped" TILO_SELINUX_STORES="$1" \
    TILO_SEMODULE="$work/semodule" SEMODULE_CALLS="$calls" STUB_ACTIVE="$active" \
    bash "$script"
}

# A store module as the image ships it: one comparison, no policy work.
# libsemanage compresses it, and a store configured without compression keeps
# the text; neither is a mismatch.
bzip2 -c "$shipped" >"$active"
refresh "$store" || fail "an equal compressed module failed the check"
[ ! -s "$calls" ] || fail "an equal compressed module was installed again: $(cat "$calls")"
cp "$shipped" "$active"
refresh "$store" || fail "an equal plain module failed the check"
[ ! -s "$calls" ] || fail "an equal plain module was installed again: $(cat "$calls")"

# The store kept an older module (a system update carried /etc or /var over):
# the image's copy replaces it, and the store ends up with what the image ships.
printf '(allow a b (file (read write)))\n' | bzip2 -c >"$active"
refresh "$store" || fail "a stale module failed the check"
[ "$(cat "$calls")" = "-i $shipped" ] || fail "a stale module was not installed again: $(cat "$calls")"
bzip2 -dc "$active" | cmp -s - "$shipped" || fail "the store did not end up with the image's module"

# A store without the module at all is the same mismatch.
: >"$calls"
rm -f "$active"
refresh "$store" || fail "a missing module failed the check"
[ "$(cat "$calls")" = "-i $shipped" ] || fail "a missing module was not installed: $(cat "$calls")"

# A system whose policy tools moved the store is recognised there, and one
# that still has both locations is done as soon as either holds the image's
# module: installing again could not make it any more current.
: >"$calls"
cp "$shipped" "$active"
other="$work/other"
mkdir -p "$(dirname "$other/active/modules/400/tilo-base/cil")"
printf 'older\n' >"$other/active/modules/400/tilo-base/cil"
refresh "$other $store" || fail "a moved store failed the check"
[ ! -s "$calls" ] || fail "a moved store installed again: $(cat "$calls")"

# A policy store that refuses the module does not fail the start: the unit
# turns the exit status into success and the journal says why.
: >"$calls"
printf 'older\n' >"$active"
if STUB_FAIL=1 refresh "$store" 2>"$work/stderr"; then
  fail "a refused install reported success"
fi
grep -Fq 'keeps the older policy' "$work/stderr" || fail "a refused install said nothing"

# The image installs the unit with the refresh script, turns it on at
# sysinit, and keeps it ahead of the units the module confines.
grep -qx 'ExecStart=/usr/libexec/tilo-selinux-policy' "$unit" || fail "the unit starts the wrong file"
grep -qx 'WantedBy=sysinit.target' "$unit" || fail "the unit is not part of sysinit"
grep -qx 'ConditionPathExists=/usr/share/tilo/selinux/tilo-base.cil' "$unit" \
  || fail "the unit does not wait for the image's module"
grep -qx 'SuccessExitStatus=1' "$unit" || fail "a refused install would fail the start"
grep -Fq 'Before=sysinit.target fs.service systemd-sysext.service' "$unit" \
  || fail "the module is not in place before fs.service and the extension merge"
grep -q '^ProtectKernelTunables=yes' "$unit" \
  && fail "the unit hides the selinuxfs its policy load writes"
for image in os/image/system.Containerfile os/Containerfile; do
  grep -Fq 'systemctl enable tilo-selinux-policy.service' "$repo/$image" \
    || fail "$image does not turn the unit on"
  grep -Fq 'os/usr/lib/systemd/system/tilo-selinux-policy.service' "$repo/$image" \
    || fail "$image does not install the unit"
  sed -n '/^COPY --chmod=0755 \\/,/\/usr\/libexec\/$/p' "$repo/$image" \
    | grep -Fq 'os/usr/libexec/tilo-selinux-policy' \
    || fail "$image does not install the refresh script as a program"
done

# The store entries the script compares are the ones the image's install of
# the module writes: default priority 400, under the name the file gives it,
# in either store layout the image can boot with.
for store in /etc/selinux/targeted /var/lib/selinux/targeted; do
  grep -Fq "$store" "$script" || fail "the script does not check $store"
done
grep -Fq 'active/modules/400/tilo-base/cil' "$script" \
  || fail "the script compares another store entry than the image installs"
grep -Fq '/usr/share/tilo/selinux/tilo-base.cil' "$script" \
  || fail "the script does not default to the image's module"
printf 'selinux-policy-refresh: ok\n'
