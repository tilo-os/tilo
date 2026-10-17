#!/usr/bin/env bash
# Deploy what stable released to the deploy target (tools/release/cloud_deploy.py), as t's
# `cloud-deploy` check runs it when stable moves.
#
#   cloud-deploy.sh OUT RECEIPT...
#
# RECEIPT... are the receipts stable's publishing checks wrote (signed `sha-<commit>` images);
# the target, in the GitOps repository TILO_DEPLOY_REPO at TILO_DEPLOY_CONFIG, decides which of
# them run where. Dispatch inputs: `revision=<commit>` deploys that commit's `sha-<commit>`
# images instead of the receipts', `allow_rollback=true` lets it be older than what runs, and
# `dry_run=true` stops before anything is committed. The plan, commit message and diff land in
# OUT (`t outputs cloud-deploy`).
set -euo pipefail

out=${1:?usage: cloud-deploy.sh OUT RECEIPT...}
shift
if [[ ${T_REF:-} != stable ]]; then
  echo "cloud-deploy.sh: deploys run in t's trusted runs of stable; this run is for ${T_REF:-a working tree}" >&2
  exit 2
fi
args=(deploy --out "$out" --require-signed)
if [[ -n ${T_INPUT_REVISION:-} ]]; then
  args+=(--revision "$T_INPUT_REVISION")
else
  for receipt; do args+=(--receipt "$receipt"); done
fi
[[ ${T_INPUT_ALLOW_ROLLBACK:-false} != true ]] || args+=(--allow-rollback)
[[ ${T_INPUT_DRY_RUN:-false} != true ]] || args+=(--dry-run)
exec python3 tools/release/cloud_deploy.py "${args[@]}"
