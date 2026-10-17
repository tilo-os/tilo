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
# AWS credentials come from an OpenID Connect identity token, never a stored
# key: t (`id_token = ["sts.amazonaws.com"]`, docs/publishing-on-t.md) and
# Forgejo Actions (`enable-openid-connect`) both hand the job
# ACTIONS_ID_TOKEN_REQUEST_URL and ACTIONS_ID_TOKEN_REQUEST_TOKEN. The token
# names the repository, reference and commit and is valid for minutes; STS
# exchanges it for AWS_SIGNING_ROLE_ARN, whose trust policy decides which
# references may sign. The keys never leave KMS.
# The token and its claims are parsed with sed: t's publishing checks run in
# the skopeo image, which has curl but no jq.
case ${COSIGN_KEY_REF:-} in
  awskms://*)
    : "${AWS_SIGNING_ROLE_ARN:?AWS signing role ARN is required}"
    : "${ACTIONS_ID_TOKEN_REQUEST_URL:?OIDC token request URL is required}"
    : "${ACTIONS_ID_TOKEN_REQUEST_TOKEN:?OIDC token request token is required}"
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
    # CloudTrail names each signature's session: on t after the commit signed,
    # on Forgejo after the run.
    if [ -n "${T_COMMIT:-}" ]; then
      AWS_ROLE_SESSION_NAME="tilo-t-$(printf '%.12s' "$T_COMMIT")"
    else
      AWS_ROLE_SESSION_NAME="tilo-forgejo-${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-1}"
    fi
    # Native runners also carry an unrelated IAM user's credentials for
    # sccache. The AWS SDK prefers those environment credentials over web
    # identity, so remove every ambient selector before invoking Cosign.
    unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN AWS_SECURITY_TOKEN
    unset AWS_PROFILE AWS_DEFAULT_PROFILE AWS_SHARED_CREDENTIALS_FILE
    AWS_WEB_IDENTITY_TOKEN_FILE=$(mktemp "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/tilo-oidc.XXXXXX")
    chmod 0600 "$AWS_WEB_IDENTITY_TOKEN_FILE"
    trap 'rm -f "${AWS_WEB_IDENTITY_TOKEN_FILE:-}"' EXIT HUP INT TERM
    curl --fail --silent --show-error \
      --header "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
      "$ACTIONS_ID_TOKEN_REQUEST_URL&audience=${TILO_SIGNING_AUDIENCE:-tilo-release-signing}" \
      | sed -n 's/.*"value"[[:space:]]*:[[:space:]]*"\([A-Za-z0-9._-]*\)".*/\1/p' \
      > "$AWS_WEB_IDENTITY_TOKEN_FILE"
    test -s "$AWS_WEB_IDENTITY_TOKEN_FILE" || {
      echo 'the OIDC token request returned no token' >&2
      exit 1
    }
    tilo_oidc_payload=$(cut -d. -f2 < "$AWS_WEB_IDENTITY_TOKEN_FILE" | tr '_-' '/+')
    case $((${#tilo_oidc_payload} % 4)) in
      2) tilo_oidc_payload="${tilo_oidc_payload}==" ;;
      3) tilo_oidc_payload="${tilo_oidc_payload}=" ;;
      0) ;;
      *) echo 'the OIDC token is malformed' >&2; exit 1 ;;
    esac
    tilo_oidc_claims=$(printf '%s' "$tilo_oidc_payload" | base64 -d 2>/dev/null) || {
      echo 'the OIDC token is malformed' >&2
      exit 1
    }
    # The value of the string claim $1, or nothing. Claims are one line of JSON
    # whose values here (URLs, references, names) carry no quotes.
    tilo_oidc_claim() {
      printf '%s' "$tilo_oidc_claims" | tr -d '\n' \
        | sed -n "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" | sed 's,\\/,/,g'
    }
    tilo_oidc_subject=$(tilo_oidc_claim sub)
    [ -n "$tilo_oidc_subject" ] || {
      echo 'the OIDC token has no valid subject' >&2
      exit 1
    }
    printf 'Using OIDC subject: %s\n' "$tilo_oidc_subject"
    # STS validates the issuer and audience before any role trust policy: an
    # issuer with no IAM OIDC provider, or an audience missing from that
    # provider's client IDs, fails as InvalidIdentityToken; a trust policy that
    # rejects the subject or audience fails as AccessDenied. Print both so a
    # failed exchange names what IAM saw.
    tilo_oidc_issuer=$(tilo_oidc_claim iss)
    [ -n "$tilo_oidc_issuer" ] || {
      echo 'the OIDC token has no valid issuer' >&2
      exit 1
    }
    # aud is a string or a list of strings.
    tilo_oidc_audience=$(tilo_oidc_claim aud)
    [ -n "$tilo_oidc_audience" ] || tilo_oidc_audience=$(printf '%s' "$tilo_oidc_claims" \
      | tr -d '\n' | sed -n 's/.*"aud"[[:space:]]*:[[:space:]]*\[\([^]]*\)\].*/\1/p' \
      | tr -d '" ')
    [ -n "$tilo_oidc_audience" ] || {
      echo 'the OIDC token has no valid audience' >&2
      exit 1
    }
    printf 'OIDC issuer: %s audience: %s\n' "$tilo_oidc_issuer" "$tilo_oidc_audience"
    export AWS_REGION AWS_DEFAULT_REGION AWS_ROLE_ARN AWS_ROLE_SESSION_NAME \
      AWS_WEB_IDENTITY_TOKEN_FILE
    unset tilo_kms_arn tilo_kms_region tilo_kms_account tilo_oidc_payload tilo_oidc_claims \
      tilo_oidc_subject tilo_oidc_issuer tilo_oidc_audience
    unset -f tilo_oidc_claim
    ;;
esac
