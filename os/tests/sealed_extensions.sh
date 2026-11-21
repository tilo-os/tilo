#!/usr/bin/env bash
# The sealed images, the desktop's and the server's, merge no system or
# configuration extension at boot; only the dev image and the development server
# image re-enable systemd-sysext, for their development layers.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
image="$repo/os/Containerfile"
server="$repo/os/Containerfile.host"
dev="$repo/os/Containerfile.dev"
dev_server="$repo/os/Containerfile.host-dev"
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

server_mask="$(joined "$server" | grep -E '(RUN|&&) systemctl mask systemd-sysext.service systemd-confext.service systemd-sysext.socket' || true)"
if [ "$(printf '%s\n' "$server_mask" | grep -c .)" = 1 ] \
   && ! grep -Fq '|| true' <<<"$server_mask" \
   && grep -Fq 'test "$(readlink "/etc/systemd/system/$u")" = /dev/null' <<<"$server_mask"; then
  ok 'the server image masks boot-time sysext/confext merges and fails if a unit is missing'
else
  bad 'the server image can merge an unsigned or directory extension over /usr or /etc at boot'
fi

# The masks must survive to the final images: nothing in a production recipe
# may unmask or re-enable a merge unit.
reenabled=0
for recipe in "$repo"/os/Containerfile*; do
  case "$recipe" in "$dev" | "$dev_server") continue ;; esac
  if joined "$recipe" | grep -E 'systemctl (unmask|enable|preset)[^&]*(systemd-sysext|systemd-confext)' >/dev/null; then
    bad "${recipe#"$repo"/} re-enables a boot-time extension merge"
    reenabled=1
  fi
done
[ "$reenabled" = 1 ] || ok 'nothing in a production recipe re-enables an extension merge'

dev_run="$(joined "$dev" | grep -F 'systemctl enable sshd.service systemd-sysext.service' || true)"
if grep -Eq 'systemctl unmask systemd-sysext\.service +&& systemctl enable sshd\.service systemd-sysext\.service' <<<"$dev_run" \
   && ! joined "$dev" | grep -Fq 'unmask systemd-confext' \
   && ! joined "$dev" | grep -Fq 'unmask systemd-sysext.socket'; then
  ok 'only the unsealed dev image unmasks systemd-sysext, for its development layers'
else
  bad 'the dev image cannot merge its layers, or unmasks more than systemd-sysext.service'
fi

server_run="$(joined "$dev_server" | grep -F 'systemctl unmask systemd-sysext.service' || true)"
if grep -Eq 'systemctl unmask systemd-sysext\.service +&& systemctl enable systemd-sysext\.service' <<<"$server_run" \
   && ! joined "$dev_server" | grep -Fq 'unmask systemd-confext' \
   && ! joined "$dev_server" | grep -Fq 'unmask systemd-sysext.socket'; then
  ok 'the development server image unmasks systemd-sysext.service alone, for its development layers'
else
  bad 'the development server image cannot merge its layers, or unmasks more than systemd-sysext.service'
fi

if grep -Fq '### No boot-time system or configuration extensions' "$doc" \
   && for u in "${units[@]}"; do grep -Fq "\`$u\`" "$doc" || exit 1; done; then
  ok 'the boot-chain security notes document the extension boundary'
else
  bad 'docs/security/boot-chain.md does not describe the extension boundary'
fi

exit "$fail"
