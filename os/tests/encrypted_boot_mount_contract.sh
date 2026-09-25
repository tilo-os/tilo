#!/usr/bin/env bash
# Exercise the privileged encrypted /boot contract without creating a real
# filesystem.  The helper must bind the deterministic UKI label to exactly the
# freshly-created /boot partition and fail closed on absent, wrong, or duplicate
# labels.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
contract="$repo_root/os/usr/libexec/tilo-boot-mount-contract"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/bin" "$tmp/root/boot"
disk="/dev/loop0"
part="/dev/loop1"
[ -b "$disk" ] && [ -b "$part" ] || {
  printf 'SKIP: loop block-device fixtures are unavailable\n'
  exit 0
}

cat > "$tmp/bin/readlink" <<EOF
#!/bin/bash
last="\${@: -1}"
case "\$last" in
  /dev/tilo-contract-disk) printf '%s\\n' '$disk' ;;
  *) printf '%s\\n' "\$last" ;;
esac
EOF
cat > "$tmp/bin/lsblk" <<EOF
#!/bin/bash
args="\$*"
if [[ "\$args" == *"TYPE $disk" ]]; then
  printf 'disk\\n'
elif [[ "\$args" == *"TYPE $part" ]]; then
  printf 'part\\n'
elif [[ "\$args" == *"PKNAME $part" ]]; then
  printf 'tilo-contract-disk\\n'
elif [[ "\$args" == *'NAME,LABEL'* ]]; then
  printf '%s TILO_BOOT\\n' '$part'
  printf '%s\\n' "\${TILO_TEST_EXTRA_LABEL:-}"
else
  exit 1
fi
EOF
cat > "$tmp/bin/blkid" <<'EOF'
#!/bin/sh
case "$*" in
  *'TYPE -o value'*) printf 'ext4\n' ;;
  *'LABEL -o value'*) printf '%s\n' "${TILO_TEST_LABEL:-TILO_BOOT}" ;;
  *) exit 1 ;;
esac
EOF
cat > "$tmp/bin/findmnt" <<EOF
#!/bin/sh
printf '%s\\n' "\${TILO_TEST_MOUNT_SOURCE:-$part}"
EOF
chmod 0755 "$tmp/bin/"*

run_contract() {
  PATH="$tmp/bin:$PATH" "$contract" --disk "$disk" --boot-part "$part" --target-root "$tmp/root"
}

[ "$(run_contract)" = LABEL=TILO_BOOT ]
! PATH="$tmp/bin:$PATH" "$contract" --disk "$disk" --target-root "$tmp/root" >/dev/null 2>&1
! TILO_TEST_LABEL=OTHER PATH="$tmp/bin:$PATH" "$contract" --disk "$disk" --boot-part "$part" --target-root "$tmp/root" >/dev/null 2>&1
! TILO_TEST_MOUNT_SOURCE=/dev/hostile PATH="$tmp/bin:$PATH" "$contract" --disk "$disk" --boot-part "$part" --target-root "$tmp/root" >/dev/null 2>&1
! TILO_TEST_EXTRA_LABEL="/dev/hostile TILO_BOOT" PATH="$tmp/bin:$PATH" "$contract" --disk "$disk" --boot-part "$part" --target-root "$tmp/root" >/dev/null 2>&1

printf 'ok: encrypted boot mount contract rejects missing, substituted, wrong, and duplicate labels\n'
