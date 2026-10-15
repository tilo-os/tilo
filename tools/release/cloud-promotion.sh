#!/usr/bin/env bash
# Plan, and with INFRA_PROMOTION_TOKEN propose, a promotion of the Tilo Cloud images
# (tools/release/cloud_promotion.py), as t's `cloud-promotion` check runs it on a dispatch of
# stable: `t dispatch cloud-promotion --ref stable [--input revision=<commit>]
# [--input open_pull_request=false]`. The plan, pull request body and manifest change land in
# OUT (`t outputs cloud-promotion`).
set -euo pipefail

out=${1:?usage: cloud-promotion.sh OUT}
revision=()
[[ -z ${T_INPUT_REVISION:-} ]] || revision=(--revision "$T_INPUT_REVISION")
if [[ -n ${INFRA_PROMOTION_TOKEN:-} && ${T_INPUT_OPEN_PULL_REQUEST:-true} == true ]]; then
  python3 tools/release/cloud_promotion.py propose "${revision[@]}" --out "$out"
else
  echo 'No pull request opened (INFRA_PROMOTION_TOKEN unset or not requested); the change is in the outputs.'
  python3 tools/release/cloud_promotion.py plan "${revision[@]}" --out "$out"
fi
