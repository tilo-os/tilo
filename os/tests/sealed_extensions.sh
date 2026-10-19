#!/usr/bin/env bash
# Neither the sealed image nor the dev image merges a system or configuration
# extension at boot by itself; tilo-extension-activate.service merges what it
# verified.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
image="$repo/os/Containerfile"
dev="$repo/os/Containerfile.dev"
doc="$repo/docs/security/boot-chain.md"
units=(systemd-sysext.service systemd-confext.service systemd-sysext.socket)
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

# Join continuation lines so each RUN instruction is one line.
joined() { sed -e ':a' -e '/\\$/N; s/\\\n/ /; ta' "$1"; }

# The strict masks share the prune-mask RUN (one rootfs layer); from them on,
# nothing may tolerate a missing unit.
mask="$(joined "$image" | grep -F '&& systemctl mask systemd-sysext.service systemd-confext.service systemd-sysext.socket' || true)"
strict="${mask#*&& systemctl mask systemd-sysext.service}"
if [ "$(printf '%s\n' "$mask" | grep -c .)" = 1 ] \
   && ! grep -Fq '|| true' <<<"$strict" \
   && grep -Fq 'test "$(readlink "/etc/systemd/system/$u")" = /dev/null' <<<"$mask"; then
  ok 'the sealed image masks boot-time sysext/confext merges and fails if a unit is missing'
else
  bad 'the sealed image can merge an unsigned or directory extension over /usr or /etc at boot'
fi

# The masks must survive to the final image: nothing later may unmask or
# re-enable a merge unit in the production recipe.
if joined "$image" | grep -E 'systemctl (unmask|enable|preset)[^&]*(systemd-sysext|systemd-confext)' >/dev/null; then
  bad 'os/Containerfile re-enables a boot-time extension merge after masking it'
else
  ok 'nothing in the production recipe re-enables an extension merge'
fi

# The dev image is the production mechanism with a development key, never a
# bypass: it keeps every mask, and its layers merge only as signed layers
# through tilo-extension-activate.service.
if ! joined "$dev" | grep -E 'systemctl (unmask|enable|preset)[^&]*(systemd-sysext|systemd-confext)' >/dev/null \
   && joined "$dev" | grep -Fq 'test "$(readlink /etc/systemd/system/systemd-sysext.service)" = /dev/null' \
   && joined "$dev" | grep -Fq 'systemctl is-enabled tilo-extension-activate.service'; then
  ok 'the dev image keeps systemd-sysext masked; its layers merge only through the activation'
else
  bad 'the dev image re-enables a boot-time extension merge'
fi

if grep -Fq '### No boot-time system or configuration extensions' "$doc" \
   && for u in "${units[@]}"; do grep -Fq "\`$u\`" "$doc" || exit 1; done; then
  ok 'the boot-chain security notes document the extension boundary'
else
  bad 'docs/security/boot-chain.md does not describe the extension boundary'
fi

exit "$fail"
