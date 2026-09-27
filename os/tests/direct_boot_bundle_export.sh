#!/usr/bin/env bash
# os/dev/export-direct-boot-bundle.sh --bls: an unsealed dev disk's one boot
# entry becomes the same kernel/initrd/cmdline bundle a sealed UKI yields.
set -euo pipefail
repo="$(cd "$(dirname "$0")/../.." && pwd)"
export_bundle="$repo/os/dev/export-direct-boot-bundle.sh"
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
fail=0
bad() { printf 'FAIL  %s\n' "$1" >&2; fail=1; }
ok() { printf 'ok    %s\n' "$1"; }

esp="$work/esp"
mkdir -p "$esp/loader/entries" "$esp/EFI/Linux/abc"
printf 'kernel' > "$esp/EFI/Linux/abc/vmlinuz"
printf 'initrd' > "$esp/EFI/Linux/abc/initrd"
cat > "$esp/loader/entries/tilo-1.conf" <<'EOF'
title Tilo
version 1
linux /EFI/Linux/abc/vmlinuz
initrd /EFI/Linux/abc/initrd
options composefs=deadbeef root=tilo-auto rw systemd.mount-extra=UUID=137A-C31F:/boot:auto:ro,nosuid,nodev,noexec systemd.mount-extra=UUID=other:/var:auto:ro
EOF
partuuid=0fc63daf-8483-4772-8e79-3d69d8477de4

if bash "$export_bundle" --bls "$esp/loader/entries/tilo-1.conf" "$esp" "$partuuid" "$work/out" \
   && [[ $(cat "$work/out/Image") == kernel && $(cat "$work/out/initrd") == initrd ]] \
   && jq -e --arg p "$partuuid" '.kernel == "Image" and .initrd == "initrd"
        and .cmdline == "composefs=deadbeef root=tilo-auto rw systemd.mount-extra=UUID=137A-C31F:/boot:auto:rw,nosuid,nodev,noexec systemd.mount-extra=UUID=other:/var:auto:ro"
        and .loader_partuuid == $p' "$work/out/boot.json" >/dev/null \
   && grep -Fq 'systemd.mount-extra=UUID=137A-C31F:/boot:auto:ro,nosuid,nodev,noexec' "$esp/loader/entries/tilo-1.conf"; then
  ok 'a dev BLS bundle makes only its overlay ESP writable without changing the installed entry'
else
  bad 'a BLS entry does not export as a direct boot bundle'
fi

sed -i 's|^linux .*|linux /../outside|' "$esp/loader/entries/tilo-1.conf"
if bash "$export_bundle" --bls "$esp/loader/entries/tilo-1.conf" "$esp" "$partuuid" "$work/escape" 2>/dev/null; then
  bad 'a BLS path outside the ESP was exported'
else
  ok 'BLS paths must stay on the ESP'
fi

sed -i -e 's|^linux .*|linux /EFI/Linux/abc/vmlinuz|' -e 's|root=tilo-auto ||' "$esp/loader/entries/tilo-1.conf"
if bash "$export_bundle" --bls "$esp/loader/entries/tilo-1.conf" "$esp" "$partuuid" "$work/noroot" 2>/dev/null; then
  bad 'a command line without the Tilo root resolver was exported'
else
  ok 'the command line must name root=tilo-auto'
fi
exit "$fail"
