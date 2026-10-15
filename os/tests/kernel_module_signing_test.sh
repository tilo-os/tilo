#!/usr/bin/env bash
# Cross-check tilo-crypto's module envelope with libkmod's independent parser.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT

crypto="${TILO_CRYPTO_BIN:-${CARGO_TARGET_DIR:-$repo/target}/debug/tilo-crypto}"
if [ ! -x "$crypto" ]; then
  cargo build --manifest-path "$repo/Cargo.toml" -p tilo-crypto
fi

cc -c "$repo/os/tests/fixtures/minimal-module.c" -o "$work/unsigned.ko"
"$crypto" generate-secure-boot-cert \
  'Tilo kernel module interoperability test' "$work/key.pem" "$work/cert.pem"
"$crypto" sign-module \
  "$work/key.pem" "$work/cert.pem" "$work/unsigned.ko" "$work/signed.ko"
"$crypto" verify-module "$work/cert.pem" "$work/signed.ko"

[ "$(modinfo -F sig_id "$work/signed.ko")" = 'PKCS#7' ]
[ "$(modinfo -F signer "$work/signed.ko")" = 'Tilo kernel module interoperability test' ]
[ "$(modinfo -F sig_hashalgo "$work/signed.ko")" = 'sha512' ]

cc -Wall -Wextra -Werror -fsyntax-only \
  "$repo/os/tests/kernel_runtime_probe.c"
grep -Fq "module.sig_enforce=1" "$repo/os/tests/kernel_runtime_probe.sh"
grep -Fq "TILO_TAMPERED_MODULE rc=-1 errno=129" \
  "$repo/os/tests/kernel_runtime_probe.sh"
grep -Fq "TILO_SIGNED_MODULE rc=0 errno=0" \
  "$repo/os/tests/kernel_runtime_probe.sh"
grep -Fq "TILO_GAME_SCHEDULING uclamp_min=256 timer_slack_ns=1000 ioprio=be/2" \
  "$repo/os/tests/kernel_runtime_probe.sh"
grep -Fq "PREEMPT(full)" "$repo/os/tests/kernel_runtime_probe.sh"

printf 'ok: libkmod accepts the Rust-native RSA/SHA-512 module signature\n'
printf 'ok: runtime probe requires module trust and game-scheduling behavior\n'
