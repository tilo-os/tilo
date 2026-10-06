#!/bin/sh
# Every signing step sources this before calling Cosign. Release keys must stay
# inside a KMS/HSM, so refuse any other key reference before anything else.
case ${COSIGN_KEY_REF:-} in
  gcpkms://*|awskms://*|azurekms://*|hashivault://*) ;;
  *) echo '::error::COSIGN_KEY_REF must name a non-exportable KMS/HSM key' >&2; exit 1 ;;
esac

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
    # Native runners also carry an unrelated IAM user's credentials for
    # sccache. The AWS SDK prefers those environment credentials over web
    # identity, so remove every ambient selector before invoking Cosign.
    unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN AWS_SECURITY_TOKEN
    unset AWS_PROFILE AWS_DEFAULT_PROFILE AWS_SHARED_CREDENTIALS_FILE
    AWS_WEB_IDENTITY_TOKEN_FILE=$(mktemp "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/tilo-forgejo-oidc.XXXXXX")
    chmod 0600 "$AWS_WEB_IDENTITY_TOKEN_FILE"
    trap 'rm -f "${AWS_WEB_IDENTITY_TOKEN_FILE:-}"' EXIT HUP INT TERM
    curl --fail --silent --show-error \
      --header "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
      "$ACTIONS_ID_TOKEN_REQUEST_URL&audience=${TILO_SIGNING_AUDIENCE:-tilo-release-signing}" \
      | jq -er .value > "$AWS_WEB_IDENTITY_TOKEN_FILE"
    test -s "$AWS_WEB_IDENTITY_TOKEN_FILE" || {
      echo 'Forgejo returned an empty OIDC token' >&2
      exit 1
    }
    tilo_oidc_payload=$(cut -d. -f2 < "$AWS_WEB_IDENTITY_TOKEN_FILE" | tr '_-' '/+')
    case $((${#tilo_oidc_payload} % 4)) in
      2) tilo_oidc_payload="${tilo_oidc_payload}==" ;;
      3) tilo_oidc_payload="${tilo_oidc_payload}=" ;;
      0) ;;
      *) echo 'Forgejo returned a malformed OIDC token' >&2; exit 1 ;;
    esac
    tilo_oidc_subject=$(printf '%s' "$tilo_oidc_payload" | base64 -d 2>/dev/null | jq -er .sub) || {
      echo 'Forgejo OIDC token has no valid subject' >&2
      exit 1
    }
    printf 'Using Forgejo OIDC subject: %s\n' "$tilo_oidc_subject"
    export AWS_REGION AWS_DEFAULT_REGION AWS_ROLE_ARN AWS_ROLE_SESSION_NAME \
      AWS_WEB_IDENTITY_TOKEN_FILE
    unset tilo_kms_arn tilo_kms_region tilo_kms_account tilo_oidc_payload tilo_oidc_subject
    ;;
esac
