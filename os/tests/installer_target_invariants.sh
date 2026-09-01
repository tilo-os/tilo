#!/usr/bin/env bash
# Static checks for the boundary between the privileged live installer and the
# system it writes. These catch regressions without requiring a destructive VM
# install; the end-to-end image test should assert the same properties at boot.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
backend="$repo_root/os/usr/libexec/qos-install"
containerfile="$repo_root/os/Containerfile.installer"
fail=0

ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

karg_deletes="$(grep -c -- '--karg-delete qos.installer=1' "$backend" || true)"
if [ "$karg_deletes" -eq 2 ]; then
  ok "both encrypted and unencrypted installs remove qos.installer=1"
else
  bad "expected two target karg deletions, found $karg_deletes"
fi

if grep -Fq 'COPY etc/greetd/config.toml /usr/share/qos/greetd-config.toml' "$containerfile" \
   && grep -Fq 'greetd-config.toml "$DEPLOY_ETC/greetd/config.toml"' "$backend"; then
  ok "normal greetd configuration is restored in the target"
else
  bad "normal greetd configuration is not bundled and restored"
fi

if grep -Fq 'rm -f "$DEPLOY_ETC/sudoers.d/qos-installer"' "$backend"; then
  ok "live-installer sudo privilege is removed from the target"
else
  bad "target does not remove the live-installer sudo privilege"
fi

if grep -Fq 'find "$LIVE_CONNECTIONS" -mindepth 1 -maxdepth 1 -type f -print0' "$backend" \
   && grep -Fq 'install -m 0600 "$PROFILE"' "$backend"; then
  ok "live Wi-Fi profiles are copied without following links and remain private"
else
  bad "installer Wi-Fi profiles are not safely preserved for first boot"
fi

exit "$fail"
