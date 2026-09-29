#!/usr/bin/env bash
# Secret Service access has two shapes after the platform split:
#   * `tilo-secrets` talks to org.freedesktop.secrets over D-Bus (no
#     subprocess), and secret-vault / runtime-identity go through it;
#   * a few migration paths still call `secret-tool` and must do so only
#     through bounded-command.
set -euo pipefail

readonly secret_tool_consumers=(
  crates/system/accounts/src/vault.rs
  crates/session/shell/src/shell/collab_mls_authority.rs
)

readonly store_consumers=(
  crates/platform/tilo-secrets/src/linux.rs
  crates/system/secret-vault/src/lib.rs
  crates/runtime/runtime-identity/src/secure_store.rs
)

if rg -n 'Command::new\("secret-tool"\)|\.output\(\)|wait_with_output\(' \
  "${secret_tool_consumers[@]}" "${store_consumers[@]}"; then
  echo 'Secret Service consumers must use bounded-command for every subprocess' >&2
  exit 1
fi

for consumer in "${secret_tool_consumers[@]}"; do
  rg -q 'bounded_command::run' "$consumer" || {
    echo "$consumer does not use the bounded Secret Service runner" >&2
    exit 1
  }
done

# The platform store must speak D-Bus, not shell out to secret-tool.
rg -q 'org\.freedesktop\.secrets' crates/platform/tilo-secrets/src/linux.rs || {
  echo 'tilo-secrets linux backend does not talk to the Secret Service over D-Bus' >&2
  exit 1
}
if rg -n 'bounded_command::run' "${store_consumers[@]}"; then
  echo 'tilo-secrets and its dependents must not shell out to secret-tool' >&2
  exit 1
fi
