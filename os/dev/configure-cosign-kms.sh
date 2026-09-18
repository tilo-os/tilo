#!/bin/sh
# Cosign's AWS KMS provider does not infer the SDK region from every supported
# awskms URI form. Derive it from the already trusted key ARN so workflows do
# not need a second, independently configurable region secret.
case ${COSIGN_KEY_REF:-} in
  awskms://*)
    : "${AWS_SIGNING_ROLE_ARN:?AWS signing role ARN is required}"
    : "${ACTIONS_ID_TOKEN_REQUEST_URL:?Forgejo OIDC request URL is required}"
    : "${ACTIONS_ID_TOKEN_REQUEST_TOKEN:?Forgejo OIDC request token is required}"
    tilo_kms_arn=${COSIGN_KEY_REF#awskms://}
    while [ "${tilo_kms_arn#/}" != "$tilo_kms_arn" ]; do
      tilo_kms_arn=${tilo_kms_arn#/}
    done
    case $tilo_kms_arn in
      arn:aws:kms:*:*:*)
        tilo_kms_region=${tilo_kms_arn#arn:aws:kms:}
        tilo_kms_region=${tilo_kms_region%%:*}
        ;;
      *) echo 'COSIGN_KEY_REF contains no valid AWS KMS ARN' >&2; exit 1 ;;
    esac
    case $tilo_kms_region in
      ''|*[!a-z0-9-]*) echo 'COSIGN_KEY_REF contains an invalid AWS KMS region' >&2; exit 1 ;;
    esac
    tilo_kms_account=${tilo_kms_arn#arn:aws:kms:*:}
    tilo_kms_account=${tilo_kms_account%%:*}
    case $AWS_SIGNING_ROLE_ARN in
      "arn:aws:iam::$tilo_kms_account:role/"?*) ;;
      *) echo 'AWS_SIGNING_ROLE_ARN must name a role in the KMS key account' >&2; exit 1 ;;
    esac
    if [ -n "${AWS_REGION:-}" ] && [ "$AWS_REGION" != "$tilo_kms_region" ]; then
      echo 'AWS_REGION conflicts with the region in COSIGN_KEY_REF' >&2
      exit 1
    fi
    if [ -n "${AWS_DEFAULT_REGION:-}" ] && [ "$AWS_DEFAULT_REGION" != "$tilo_kms_region" ]; then
      echo 'AWS_DEFAULT_REGION conflicts with the region in COSIGN_KEY_REF' >&2
      exit 1
    fi
    AWS_REGION=$tilo_kms_region
    AWS_DEFAULT_REGION=$tilo_kms_region
    AWS_ROLE_ARN=$AWS_SIGNING_ROLE_ARN
    AWS_ROLE_SESSION_NAME="tilo-forgejo-${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-1}"
    AWS_WEB_IDENTITY_TOKEN_FILE=$(mktemp "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/tilo-forgejo-oidc.XXXXXX")
    chmod 0600 "$AWS_WEB_IDENTITY_TOKEN_FILE"
    trap 'rm -f "${AWS_WEB_IDENTITY_TOKEN_FILE:-}"' EXIT HUP INT TERM
    curl --fail --silent --show-error \
      --header "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
      "$ACTIONS_ID_TOKEN_REQUEST_URL&audience=tilo-release-signing" \
      | jq -er .value > "$AWS_WEB_IDENTITY_TOKEN_FILE"
    test -s "$AWS_WEB_IDENTITY_TOKEN_FILE" || {
      echo 'Forgejo returned an empty OIDC token' >&2
      exit 1
    }
    export AWS_REGION AWS_DEFAULT_REGION AWS_ROLE_ARN AWS_ROLE_SESSION_NAME \
      AWS_WEB_IDENTITY_TOKEN_FILE
    unset tilo_kms_arn tilo_kms_region tilo_kms_account
    ;;
esac
