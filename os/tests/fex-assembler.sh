#!/usr/bin/env bash
# FEX's SMSW tests depend on operand-size encodings, including redundant ones.
# NASM 3.02 drops required prefixes; test bytes before compiling the test suite.
set -euo pipefail
assembler=${1:-nasm}
out=$(mktemp -d)
trap 'rm -rf -- "$out"' EXIT
cat > "$out/probe.asm" <<'EOF'
BITS 64
smsw rax
smsw ebx
smsw cx
BITS 32
smsw eax
smsw bx
EOF
"$assembler" -f bin "$out/probe.asm" -o "$out/probe.bin"
actual=$(od -An -v -tx1 "$out/probe.bin" | tr -d ' \n')
expected=480f01e00f01e3660f01e10f01e0660f01e3
if [ "$actual" != "$expected" ]; then
    "$assembler" -v >&2
    printf 'FEX SMSW assembler contract failed: expected %s, got %s\n' "$expected" "$actual" >&2
    exit 1
fi
"$assembler" -v
printf 'FEX SMSW operand-size encoding contract passed\n'
