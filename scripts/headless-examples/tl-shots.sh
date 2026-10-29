#!/bin/bash
# Shots of the tilolang example apps (apps/tl), headless on lavapipe only: each package is
# compiled into a runtime bundle and captured by `tilolang-app --shots` in every state below.
#
#   cargo build -p tilolang-app --bin tilolang-app --example seeded_room
#   scripts/headless-examples/tl-shots.sh OUT [app...]     (default: notes tasks chat board mail today)
#
# Writes OUT/<app>-<state>-<size>[-light].png: `seeded` (the package's first scenario, its
# item open), `empty` (no scenario), `search` (search open over the seeded state), at
# 1280x800 and 420x860, dark, and the seeded state in light too. Logs go to OUT/logs.
set -eu
here=$(cd "$(dirname "$0")/../.." && pwd)
out=$(realpath -m "$1"); shift
tl=${TL_DIR:-$here/apps/tl}
apps=("$@"); [ ${#apps[@]} -gt 0 ] || apps=(notes tasks chat board mail today)
bin=${TILOLANG_APP:-$here/target/debug/tilolang-app}
seed=${SEEDED_ROOM:-$here/target/debug/examples/seeded_room}
# Software Vulkan only: the loader sees lavapipe and nothing else, so no shot reaches the GPU.
export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.$(uname -m).json
export WGPU_BACKEND=vulkan WGPU_ADAPTER_NAME=llvmpipe VK_LOADER_DEBUG=driver
work=$(mktemp -d /tmp/tl-shots.XXXX); trap 'rm -rf "$work"' EXIT
mkdir -p "$out/logs"
for app in "${apps[@]}"; do
  "$seed" "$tl/$app" 0 "$work/$app" >/dev/null
  id=$(grep -o '^app [a-z.]*' "$tl/$app/app.tl" | cut -d' ' -f2)
  scenario=$(grep -o '^scenario "[^"]*"' "$tl/$app/app.tl" | head -1 | cut -d'"' -f2)
  case $app in notes) query=standup ;; tasks) query=the ;; chat) query=onboarding ;; mail) query=launch ;; *) query=a ;; esac
  shoot() { # name args...
    local name=$1; shift
    local log=$out/logs/$name.log
    TILO_PACKAGE_ID=$id TILO_APP_MODULE=$work/$app/bundle timeout 300 "$bin" --shots "$out/$name.png" "$@" >"$log" 2>&1 \
      || echo "FAILED: $name (see $log)"
    grep -q lvp_icd "$log" || echo "NO LAVAPIPE: $name"
    if grep -qiE 'turnip|freedreno_icd|asahi_icd|panfrost_icd|radeon_icd|intel_icd' "$log"; then echo "HARDWARE DRIVER: $name"; fi
  }
  for vp in 1280x800 420x860; do
    shoot "$app-seeded-$vp" --scenario "$scenario" --viewport "$vp" --theme dark
    shoot "$app-seeded-$vp-light" --scenario "$scenario" --viewport "$vp" --theme light
    shoot "$app-empty-$vp" --viewport "$vp" --theme dark
    shoot "$app-search-$vp" --scenario "$scenario" --viewport "$vp" --theme dark --search "$query"
  done
done
