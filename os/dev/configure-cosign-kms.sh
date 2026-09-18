#!/bin/sh
# Cosign's AWS KMS provider does not infer the SDK region from every supported
# awskms URI form. Derive it from the already trusted key ARN so workflows do
# not need a second, independently configurable region secret.
case ${COSIGN_KEY_REF:-} in
  awskms://*)
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
    export AWS_REGION AWS_DEFAULT_REGION
    unset tilo_kms_arn tilo_kms_region
    ;;
esac
