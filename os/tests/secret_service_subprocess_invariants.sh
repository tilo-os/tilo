#!/usr/bin/env bash
# Secret Service access has two shapes after the platform split:
#   * `tilo-secrets` talks to org.freedesktop.secrets over D-Bus (no
#     subprocess), and secret-vault / runtime-identity go through it;
#   * a few migration paths still call `secret-tool` and must do so only
#     through bounded-command.
#
# The consumers are found in the tree, not listed by path: one that moves to
# another crate stays checked, and one that appears anew is caught here rather
# than running the binary unbounded.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

readonly store_consumers=(
  crates/platform/tilo-secrets/src/linux.rs
  crates/system/secret-vault/src/lib.rs
  crates/runtime/runtime-identity/src/secure_store.rs
)

# Every source that starts the `secret-tool` binary. Tests run it as their
# oracle against the real service, so they are not consumers of it.
mapfile -t secret_tool_consumers < <(
  rg -l --glob '*.rs' --glob '!**/tests/**' --glob '!**/examples/**' \
    --fixed-strings '"secret-tool"' crates apps os || true
)
for consumer in "${secret_tool_consumers[@]}"; do
  rg -q 'bounded_command::run' "$consumer" || {
    echo "$consumer does not use the bounded Secret Service runner" >&2
    exit 1
  }
done

# Neither a consumer nor the store that serves it reaches for the process itself.
if rg -n 'Command::new\("secret-tool"\)|\.output\(\)|wait_with_output\(' \
  "${secret_tool_consumers[@]}" "${store_consumers[@]}"; then
  echo 'Secret Service access must go through bounded-command' >&2
  exit 1
fi

# The platform store must speak D-Bus, not shell out to secret-tool.
rg -q 'org\.freedesktop\.secrets' crates/platform/tilo-secrets/src/linux.rs || {
  echo 'tilo-secrets linux backend does not talk to the Secret Service over D-Bus' >&2
  exit 1
}
if rg -n 'bounded_command::run' "${store_consumers[@]}"; then
  echo 'tilo-secrets and its dependents must not shell out to secret-tool' >&2
  exit 1
fi
