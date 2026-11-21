#!/usr/bin/env bash
# One component of this tree as the incoming layer a machine's tilo-vm-incoming.service applies:
# DIR/<component>.tar and DIR/<component>.json, packed by tilo-vm's own packer with the
# component's manifest profile (os/components.toml), as `tilo-vm swap` builds it.
#
#   server-plan-layer.sh COMPONENT DIR      (TILO_VM_BIN: the tilo-vm to pack with)
set -euo pipefail
repo=$(cd "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
component=${1:?usage: server-plan-layer.sh COMPONENT DIR}
dir=${2:?usage: server-plan-layer.sh COMPONENT DIR}
packer=${TILO_VM_BIN:?TILO_VM_BIN names the tilo-vm that packs the layer}
profile=$(python3 "$repo/os/scripts/install-components.py" --list --format json \
  | jq -r --arg c "$component" '.[] | select(.name == $c) | .profile')
[[ -n $profile ]] || { echo "server-plan-layer: no component $component in os/components.toml" >&2; exit 1; }
started=$SECONDS
(cd "$repo" && "$packer" layer-pack --component "$component" --profile "$profile" --incoming "$dir")
[[ -s $dir/$component.tar && -s $dir/$component.json ]] \
  || { echo "server-plan-layer: the packer left no layer for $component" >&2; exit 1; }
printf 'layer %s: %s bytes, sha256 %s, %ss\n' "$component" "$(stat -c %s "$dir/$component.tar")" \
  "$(sha256sum "$dir/$component.tar" | cut -c1-16)" "$((SECONDS - started))"
