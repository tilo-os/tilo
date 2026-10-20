#!/usr/bin/env bash
# Capture the first screens of every built-in app at three window sizes.
#
#   shots/run.sh [app...]      (default: every app)
#
# Rendering goes through the toolkit's offscreen path on Mesa's software Vulkan (lavapipe), so it
# never touches the GPU. Run from the repository root.
set -euo pipefail
cd "$(dirname "$0")/.."
export VK_ICD_FILENAMES=${VK_ICD_FILENAMES:-/usr/share/vulkan/icd.d/lvp_icd.aarch64.json}
apps=("$@")
[ ${#apps[@]} -gt 0 ] || apps=(counter todo board notes notes-detail letters letters-detail chat chat-detail)
tdev cargo build -p tilolang-app
for app in "${apps[@]}"; do
  for size in 420x860 1280x800 2560x1200; do
    tdev cargo run -q -p tilolang-app -- --shots "shots/${app}-${size}.png" --viewport "$size" --scenario "$app"
  done
done
