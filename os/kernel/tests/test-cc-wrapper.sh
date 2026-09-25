#!/bin/sh
set -eu
repo=$(CDPATH='' cd -- "$(dirname -- "$0")/../../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT
cat > "$tmp/compiler" <<'EOF'
#!/bin/sh
printf 'direct %s\n' "$*" >> "$TRACE"
EOF
cat > "$tmp/cache" <<'EOF'
#!/bin/sh
printf 'cached %s\n' "$*" >> "$TRACE"
EOF
chmod +x "$tmp/compiler" "$tmp/cache"
TRACE="$tmp/trace" TILO_KERNEL_CC_LAUNCHER="$tmp/cache" \
    sh "$repo/os/kernel/cc-wrapper.sh" "$tmp/compiler" -c certs/system_certificates.S
TRACE="$tmp/trace" TILO_KERNEL_CC_LAUNCHER="$tmp/cache" \
    sh "$repo/os/kernel/cc-wrapper.sh" "$tmp/compiler" -c kernel/module/main.c
grep -Fx 'direct -c certs/system_certificates.S' "$tmp/trace"
grep -Fx "cached $tmp/compiler -c kernel/module/main.c" "$tmp/trace"
