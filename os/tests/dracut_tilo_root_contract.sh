#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
module="$repo/os/usr/lib/dracut/modules.d/91tilo-rollback"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Exercise the hook as dracut sources it. Only Tilo's exact custom root token
# may claim ownership; ordinary and malformed roots remain available to the
# normal dracut validators.
root=tilo-auto
unset rootok || true
# shellcheck source=/dev/null
. "$module/parse-tilo-root.sh"
test "${rootok:-}" = 1

# shellcheck disable=SC2034 # the sourced hook consumes root
for root in /dev/vda2 tilo-auto-extra ''; do
    unset rootok || true
    # shellcheck source=/dev/null
    . "$module/parse-tilo-root.sh"
    test -z "${rootok:-}"
done

cat >"$work/expected-hook" <<'EOF'
#!/usr/bin/sh

# Tilo's initrd generator resolves this target-independent token after dracut's
# command-line phase. Tell dracut that the exact token has a registered owner.
# shellcheck disable=SC2154 # dracut defines root before sourcing cmdline hooks
case "$root" in
    tilo-auto)
        # shellcheck disable=SC2034
        rootok=1
        ;;
esac
EOF
cmp "$work/expected-hook" "$module/parse-tilo-root.sh"

# Run module installation against recording dracut primitives. This verifies
# the installed hook and the complete executable closure used directly by the
# root resolver, its generated mount unit, and the recovery-unlock helper.
: >"$work/events"
inst_multiple() { printf 'multiple:%s\n' "$*" >>"$work/events"; }
inst_hook() { printf 'hook:%s:%s:%s\n' "$1" "$2" "$3" >>"$work/events"; }
inst_simple() { printf 'simple:%s:%s\n' "$1" "${2:-}" >>"$work/events"; }
# shellcheck disable=SC2034 # the sourced module consumes moddir
moddir="$module"
# shellcheck source=/dev/null
. "$module/module-setup.sh"
install

cat >"$work/expected-events" <<EOF
multiple:awk bash cat chmod cryptsetup dd find grep install ln lsblk readlink rm seq sleep systemd-ask-password systemd-cryptsetup systemd-escape tr udevadm tpm2_createprimary tpm2_flushcontext tpm2_getcap tpm2_getpolicydigest tpm2_load tpm2_loadexternal tpm2_nvread tpm2_nvreadpublic tpm2_policyauthorize tpm2_policyauthvalue tpm2_policynv tpm2_policypcr tpm2_policyor tpm2_readpublic tpm2_startauthsession tpm2_unseal tpm2_verifysignature
hook:cmdline:90:$module/parse-tilo-root.sh
simple:/usr/libexec/tilo/tilo-rollback:/usr/bin/tilo-rollback
simple:/usr/libexec/tilo/tilo-boot-signin:
simple:/usr/libexec/tilo/tilo-auth:/usr/bin/tilo-auth
simple:/usr/libexec/tilo-rollback-initrd-unlock:
simple:/usr/libexec/tilo-root-discover:
simple:/usr/lib/systemd/system-generators/tilo-root-generator:
simple:/usr/share/tilo/tpm2-pcr-signing.public.pem:
EOF
cmp "$work/expected-events" "$work/events"

# The module is chosen on what every encrypted root needs. The boot sign-in
# rides along: check() never makes it, or tilo-auth, a reason to skip.
required=""
require_binaries() { required="$*"; }
check && status=0 || status=$?
test "$status" = 255
test "$required" = "/usr/libexec/tilo/tilo-rollback cryptsetup systemd-ask-password systemd-escape"

printf 'ok: dracut accepts only root=tilo-auto and installs its complete command closure\n'
