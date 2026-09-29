#!/usr/bin/env bash
# Verify the Nix tilo-runtime result: its layout with store paths in place of
# FHS ones, then the same smoke test the distro packages pass.
set -euo pipefail

[[ $# == 1 ]] || { echo "usage: $0 RESULT" >&2; exit 2; }
result=$(realpath "$1")
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
private=$result/libexec/tilo-runtime

for file in "$private/tilo-runtime" "$private/tilo-launcher" "$private/store"; do
    [[ -x $file ]] || { echo "missing executable: $file" >&2; exit 1; }
done
[[ $(readlink "$result/bin/tilo-runtime") == ../libexec/tilo-runtime/tilo-runtime ]]
grep -Fxq "ExecStart=$result/bin/tilo-runtime --daemon" "$result/share/systemd/user/tilo-runtime.service"
grep -Fxq "Exec=$private/store" "$result/share/applications/so.tilo.store.desktop"
test -f "$result/share/icons/hicolor/scalable/apps/so.tilo.store.svg"
test -f "$result/share/licenses/tilo-runtime/LICENSE"
# The sandbox tools reach the launcher through its wrapper, not a host PATH.
grep -Eq '/nix/store/[^/]+-bubblewrap-[^/]+/bin' "$private/tilo-launcher"
grep -Eq '/nix/store/[^/]+-xdg-dbus-proxy-[^/]+/bin' "$private/tilo-launcher"

# A Nix userspace has no GPU driver on its library path; lavapipe from the
# flake's own nixpkgs renders the app headless. TILO_NIX_FLAKE names the flake
# when the checkout is not a readable Git repository.
mesa=$(nix build --no-link --print-out-paths --inputs-from "${TILO_NIX_FLAKE:-$repo_root}" nixpkgs#mesa)
icd=$(find "$mesa/share/vulkan/icd.d" -name 'lvp_icd.*.json' -print -quit)
[[ -n $icd ]] || { echo "nixpkgs mesa carries no lavapipe ICD" >&2; exit 1; }
TILO_RUNTIME_SMOKE_USER=current TILO_RUNTIME_DIR="$private" \
    TILO_RUNTIME_BIN="$result/bin/tilo-runtime" VK_ICD_FILENAMES="$icd" \
    "$repo_root/dist/runtime/smoke-test.sh"
