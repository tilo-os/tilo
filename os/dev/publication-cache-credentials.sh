#!/usr/bin/env bash
# Exchange this job's Forgejo Actions OIDC token for short-lived credentials of
# the publication compiler-cache role and print them as STS JSON
# ({AccessKeyId, SecretAccessKey, SessionToken, Expiration}).
#
# AssumeRoleWithWebIdentity is an unsigned STS call, so no ambient AWS
# credential (such as the runner's shared sccache key) is involved. The role
# trusts only main and v* tag tokens of this repository for the
# `tilo-publication-cache` audience; STS refuses anything else.
set -euo pipefail

role=${1:?usage: publication-cache-credentials.sh ROLE_ARN}
: "${ACTIONS_ID_TOKEN_REQUEST_URL:?Forgejo OIDC is not enabled for this job}"
: "${ACTIONS_ID_TOKEN_REQUEST_TOKEN:?Forgejo OIDC is not enabled for this job}"
[[ $role =~ ^arn:aws:iam::[0-9]{12}:role/[A-Za-z0-9+=,.@_-]+$ ]] \
  || { echo "not an IAM role ARN: $role" >&2; exit 1; }

jwt=$(curl --fail --silent --show-error \
  --header "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
  "$ACTIONS_ID_TOKEN_REQUEST_URL&audience=tilo-publication-cache" | jq -er .value)
echo "::add-mask::$jwt" >&2

# Six hours: longer than a publication lane with a cold cache.
response=$(curl --fail-with-body --silent --show-error --retry 3 \
  --header 'Accept: application/json' \
  --data-urlencode 'Action=AssumeRoleWithWebIdentity' \
  --data-urlencode 'Version=2011-06-15' \
  --data-urlencode "RoleArn=$role" \
  --data-urlencode "RoleSessionName=tilo-publication-${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-1}" \
  --data-urlencode 'DurationSeconds=21600' \
  --data-urlencode "WebIdentityToken=$jwt" \
  https://sts.amazonaws.com/)
jq -ec '.AssumeRoleWithWebIdentityResponse.AssumeRoleWithWebIdentityResult.Credentials
        | select(.AccessKeyId and .SecretAccessKey and .SessionToken)' <<<"$response"
