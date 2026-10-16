#!/usr/bin/env bash
# Sourced by the Windows signing step: exchange t's identity token (`id_token =
# ["sts.amazonaws.com"]`) for short-lived credentials of the package signing role, and export
# them as AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY and AWS_SESSION_TOKEN, which jsign reads.
# Nothing is stored: the token names this repository and the release tag and lasts minutes, and
# the role's trust policy decides which references may sign (docs/publishing-on-t.md, Windows).
#
# Environment: TILO_SIGNING_ROLE_ARN, TILO_SIGNING_REGION, and t's ACTIONS_ID_TOKEN_REQUEST_URL
# and ACTIONS_ID_TOKEN_REQUEST_TOKEN.
: "${TILO_SIGNING_ROLE_ARN:?the AWS role to sign as}"
: "${TILO_SIGNING_REGION:?the region of the signing key}"
: "${ACTIONS_ID_TOKEN_REQUEST_URL:?no identity token: the check needs id_token = [\"sts.amazonaws.com\"]}"
: "${ACTIONS_ID_TOKEN_REQUEST_TOKEN:?no identity token: the check needs id_token = [\"sts.amazonaws.com\"]}"

tilo_sts_dir=$(mktemp -d "${TMPDIR:-/tmp}/aws-web-identity.XXXXXX")
(umask 077
 curl --fail --silent --show-error --proto '=https' \
   --header "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
   "$ACTIONS_ID_TOKEN_REQUEST_URL&audience=sts.amazonaws.com" \
   | sed -n 's/.*"value"[[:space:]]*:[[:space:]]*"\([A-Za-z0-9._-]*\)".*/\1/p' > "$tilo_sts_dir/token"
 test -s "$tilo_sts_dir/token" || { echo 'the identity token request returned no token' >&2; exit 1; }
 # The token goes in the request body from its file, never on a command line.
 curl --fail-with-body --silent --show-error --proto '=https' \
   "https://sts.$TILO_SIGNING_REGION.amazonaws.com/" \
   --data-urlencode Action=AssumeRoleWithWebIdentity --data-urlencode Version=2011-06-15 \
   --data-urlencode "RoleArn=$TILO_SIGNING_ROLE_ARN" \
   --data-urlencode "RoleSessionName=tilo-t-$(printf '%.12s' "${T_COMMIT:-local}")" \
   --data-urlencode DurationSeconds=900 \
   --data-urlencode "WebIdentityToken@$tilo_sts_dir/token" > "$tilo_sts_dir/reply.xml" \
   || { sed -n 's:.*<Message>\(.*\)</Message>.*:STS refused the token\: \1:p' "$tilo_sts_dir/reply.xml" >&2; exit 1; }
) || { rm -rf -- "$tilo_sts_dir"; exit 1; }
tilo_sts_field() { tr -d '\n' < "$tilo_sts_dir/reply.xml" | sed -n "s:.*<$1>\([^<]*\)</$1>.*:\1:p"; }
unset AWS_PROFILE AWS_DEFAULT_PROFILE AWS_SHARED_CREDENTIALS_FILE AWS_SECURITY_TOKEN
AWS_ACCESS_KEY_ID=$(tilo_sts_field AccessKeyId)
AWS_SECRET_ACCESS_KEY=$(tilo_sts_field SecretAccessKey)
AWS_SESSION_TOKEN=$(tilo_sts_field SessionToken)
rm -rf -- "$tilo_sts_dir"
unset -f tilo_sts_field
[[ -n $AWS_ACCESS_KEY_ID && -n $AWS_SECRET_ACCESS_KEY && -n $AWS_SESSION_TOKEN ]] \
  || { echo 'STS answered without credentials' >&2; exit 1; }
export AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN
export AWS_REGION=$TILO_SIGNING_REGION
