#!/usr/bin/env bash
set -euo pipefail

readonly consumers=(
  crates/system/accounts/src/vault.rs
  crates/system/secret-vault/src/lib.rs
  crates/runtime/runtime-identity/src/secure_store.rs
  crates/session/shell/src/shell/collab_mls_authority.rs
)

if rg -n 'Command::new\("secret-tool"\)|\.output\(\)|wait_with_output\(' "${consumers[@]}"; then
  echo 'Secret Service consumers must use bounded-command for every subprocess' >&2
  exit 1
fi

for consumer in "${consumers[@]}"; do
  rg -q 'bounded_command::run' "$consumer" || {
    echo "$consumer does not use the bounded Secret Service runner" >&2
    exit 1
  }
done
