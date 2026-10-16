#!/usr/bin/env bash
# Plan, and with INFRA_PROMOTION_TOKEN propose, a promotion of the Tilo Cloud images
# (tools/release/cloud_promotion.py), as t's `cloud-promotion` and `cloud-drift` checks run it.
#
#   cloud-promotion.sh promote OUT   `t dispatch cloud-promotion --ref stable [--input
#                                    revision=<commit>] [--input open_pull_request=false]`: the
#                                    plan, pull request body and manifest change land in OUT
#                                    (`t outputs cloud-promotion`)
#   cloud-promotion.sh drift JSON    the weekly report of what production runs
#
# Production runs what stable released: publish-room-service and publish-web-entry set stable's
# `room-service` and `web-entry` pointers to their receipts (signed `sha-<commit>` images), which
# the checks read as T_POINTER_ROOM_SERVICE and T_POINTER_WEB_ENTRY. With both set and no
# revision asked for, those receipts choose what is promoted.
set -euo pipefail

mode=${1:?usage: cloud-promotion.sh promote OUT | drift JSON}
dest=${2:?usage: cloud-promotion.sh promote OUT | drift JSON}
receipts=()
if [[ -n ${T_POINTER_ROOM_SERVICE:-} && -n ${T_POINTER_WEB_ENTRY:-} ]]; then
  dir=$(mktemp -d)
  trap 'rm -rf "$dir"' EXIT
  printf '%s\n' "$T_POINTER_ROOM_SERVICE" > "$dir/room-service.json"
  printf '%s\n' "$T_POINTER_WEB_ENTRY" > "$dir/web-entry.json"
  receipts=(--receipt "$dir/room-service.json" --receipt "$dir/web-entry.json")
else
  echo "Stable's room-service and web-entry pointers are not both set; looking for sha-<commit> tags instead."
fi

case $mode in
  drift)
    mkdir -p "$(dirname -- "$dest")"
    exec python3 tools/release/cloud_promotion.py drift --json "$dest" "${receipts[@]}"
    ;;
  promote) ;;
  *) echo "cloud-promotion.sh: unknown mode $mode" >&2; exit 2 ;;
esac

choice=("${receipts[@]}")
# An explicit revision promotes that commit's sha-<commit> images instead of the receipts.
[[ -z ${T_INPUT_REVISION:-} ]] || choice=(--revision "$T_INPUT_REVISION")
if [[ -n ${INFRA_PROMOTION_TOKEN:-} && ${T_INPUT_OPEN_PULL_REQUEST:-true} == true ]]; then
  python3 tools/release/cloud_promotion.py propose "${choice[@]}" --out "$dest"
else
  echo 'No pull request opened (INFRA_PROMOTION_TOKEN unset or not requested); the change is in the outputs.'
  python3 tools/release/cloud_promotion.py plan "${choice[@]}" --out "$dest"
fi
