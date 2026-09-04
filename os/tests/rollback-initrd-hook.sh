#!/usr/bin/env bash
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
generator="$repo/os/usr/lib/systemd/system-generators/qos-rollback-unlock-generator"
helper="$repo/os/usr/libexec/qos-rollback-initrd-unlock"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

uuid=01234567-89ab-cdef-0123-456789abcdef
printf 'quiet rd.luks.uuid=%s qos.generation=7\n' "$uuid" >"$work/cmdline"
QOS_ROLLBACK_CMDLINE_FILE="$work/cmdline" "$generator" "$work/generated"
escaped='luks\x2d01234567\x2d89ab\x2dcdef\x2d0123\x2d456789abcdef'
service="$work/generated/qos-rollback-unlock@$escaped.service"
dropin="$work/generated/systemd-cryptsetup@$escaped.service.d/10-qos-rollback.conf"
if grep -Fq "Requires=dev-disk-by\\x2duuid-01234567\\x2d89ab\\x2dcdef\\x2d0123\\x2d456789abcdef.device" "$service" \
   && grep -Fq "Before=systemd-cryptsetup@$escaped.service" "$service" \
   && grep -Fq "After=qos-rollback-unlock@$escaped.service" "$dropin"; then
  ok "generator orders TPM unlock after the LUKS device and before cryptsetup"
else
  bad "generator did not create the expected per-volume ordering"
fi

printf 'rd.luks.uuid=../../unsafe rd.luks.uuid=not-a-uuid\n' >"$work/bad-cmdline"
QOS_ROLLBACK_CMDLINE_FILE="$work/bad-cmdline" "$generator" "$work/bad-generated"
if [ ! -e "$work/bad-generated" ]; then
  ok "generator ignores malformed command-line device identities"
else
  bad "generator emitted a unit for a malformed device identity"
fi

mkdir "$work/bin"
cat >"$work/bin/cryptsetup" <<'EOF'
#!/bin/bash
case "${QOS_TEST_TOKEN:-1}:$*" in 1:*'--token-id 0'*) printf '{"type":"qos-rollback"}\n'; exit 0 ;; *) exit 1 ;; esac
EOF
cat >"$work/bin/systemd-ask-password" <<'EOF'
#!/bin/bash
printf called >"$QOS_TEST_ASKED"
printf 246810
EOF
cat >"$work/bin/qos-rollback" <<'EOF'
#!/bin/bash
printf '%s\n' "$*" >"$QOS_TEST_ARGS"
while [ "$#" -gt 0 ]; do
  if [ "$1" = --output ]; then printf 'sealed-key' >"$2"; fi
  shift
done
[ "${QOS_TEST_FAIL:-0}" -eq 0 ]
EOF
chmod 0755 "$work/bin/"*
mkdir -p "/dev/disk/by-uuid" 2>/dev/null || true
if PATH="$work/bin:$PATH" QOS_TEST_ARGS="$work/args" QOS_TEST_ASKED="$work/asked" QOS_ROLLBACK_RUNTIME_ROOT="$work/root" \
     "$helper" "$uuid" \
   && grep -Fq "unlock --device /dev/disk/by-uuid/$uuid --pin-file $work/root/run/qos-rollback/unlock.pin --output $work/root/run/cryptsetup-keys.d/luks-$uuid.key" "$work/args" \
   && [ ! -e "$work/root/run/qos-rollback/unlock.pin" ]; then
  ok "helper keeps the PIN off argv to qos-rollback and removes its tmpfs file"
else
  bad "helper did not perform the expected secret-safe unlock handoff"
fi

rm -f "$work/asked" "$work/args"
if PATH="$work/bin:$PATH" QOS_TEST_TOKEN=0 QOS_TEST_ARGS="$work/args" \
     QOS_TEST_ASKED="$work/asked" QOS_ROLLBACK_RUNTIME_ROOT="$work/no-token-root" \
     "$helper" "$uuid" \
   && [ ! -e "$work/asked" ] && [ ! -e "$work/args" ]; then
  ok "passphrase-only volumes do not receive a redundant TPM PIN prompt"
else
  bad "helper prompted or attempted TPM unlock without a qOS token"
fi

rm -f "$work/asked" "$work/args"
failed_key="$work/failure-root/run/cryptsetup-keys.d/luks-$uuid.key"
if PATH="$work/bin:$PATH" QOS_TEST_FAIL=1 QOS_TEST_ARGS="$work/args" \
     QOS_TEST_ASKED="$work/asked" QOS_ROLLBACK_RUNTIME_ROOT="$work/failure-root" \
     "$helper" "$uuid" \
   && [ ! -e "$failed_key" ]; then
  ok "failed TPM unseal removes partial key material and yields to recovery"
else
  bad "failed TPM unseal blocked fallback or left partial key material"
fi

if [ "$fail" -eq 0 ]; then
  printf 'all rollback initramfs-hook invariants hold\n'
else
  printf 'rollback initramfs-hook invariants FAILED\n' >&2
fi
exit "$fail"
