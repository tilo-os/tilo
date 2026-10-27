#!/usr/bin/env bash
# Hold a built OS image's /usr to its flavour's budget (os/image-size-budget.toml).
#
#   image-size.sh FLAVOUR IMAGE OUT
#
# IMAGE is an OCI layout directory (os-image-<arch>'s OUT/oci), an OCI archive
# (os-board-<board>'s image.oci.tar) or docker://REFERENCE, a published image read
# from its registry (ARCH picks the platform of an index). tools/perf/image_size.py replays its layers
# without a container engine and writes OUT/image-size.json and OUT/image-size.md
# (the breakdown by layer, directory, file and package); os/tests/image_size_budget.py
# then fails the check when /usr is over the flavour's ceiling or carries what the
# Windows compatibility extension owns. The report is written before the verdict,
# so a failing run still says where the bytes went.
set -euo pipefail

flavour=${1:?usage: image-size.sh FLAVOUR IMAGE OUT}
image=${2:?usage: image-size.sh FLAVOUR IMAGE OUT}
out=${3:?usage: image-size.sh FLAVOUR IMAGE OUT}

rm -rf "$out"
mkdir -p "$out"
source=(--oci-layout "$image")
if [[ $image == docker://* ]]; then
  source=(--image "${image#docker://}" --arch "${ARCH:-$(uname -m)}")
elif [[ -f $image ]]; then
  mkdir -p "$out/layout"
  tar -xf "$image" -C "$out/layout"
  source=(--oci-layout "$out/layout")
fi
python3 -I tools/perf/image_size.py "${source[@]}" \
  --json "$out/image-size.json" --markdown "$out/image-size.md" --quiet
rm -rf "$out/layout"
cat "$out/image-size.md"
python3 -I os/tests/image_size_budget.py check --flavour "$flavour" --report "$out/image-size.json"
