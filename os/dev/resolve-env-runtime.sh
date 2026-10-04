#!/usr/bin/env bash
# Resolve this checkout's env-runtime environment image by fingerprint.
#
# The fingerprint is the SHA-256 of os/Containerfile.env-runtime: its package
# list and version are the whole definition of a runtime, and every tool
# image is bound to one exact runtime, so a runtime changes only when that
# file does. Looks up registry.tilo.so/tilo/env-runtime:set-<fingerprint>,
# checks its Cosign signature against keys/cosign.pub, and prints the
# digest-pinned reference.
#
#   resolve-env-runtime.sh           print registry.../env-runtime@sha256:...
#   resolve-env-runtime.sh --exists  exit 0 if the tag resolves, 1 if it does
#                                    not exist, 3 if the registry failed
#   resolve-env-runtime.sh --tag     print set-<fingerprint> only
#   resolve-env-runtime.sh --version print the runtime's package version
#
# Needs: cosign, skopeo. Registry read access for the tag.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
definition="$repo/os/Containerfile.env-runtime"

mode=resolve
case "${1:-}" in
  "") ;;
  --exists) mode=exists ;;
  --tag) mode=tag ;;
  --version) mode=version ;;
  -h|--help)
    sed -n '2,18p' "$0"
    exit 0
    ;;
  *)
    printf 'resolve-env-runtime: unknown option %s\n' "$1" >&2
    exit 2
    ;;
esac

if [ "$mode" = version ]; then
  version="$(sed -n 's/^# env-runtime-version: \([0-9][0-9.]*\)$/\1/p' "$definition")"
  [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || { printf 'resolve-env-runtime: %s names no env-runtime-version\n' "$definition" >&2; exit 1; }
  printf '%s\n' "$version"
  exit 0
fi

fingerprint="$(sha256sum "$definition" | cut -d' ' -f1)"
tag="set-$fingerprint"
tagged="registry.tilo.so/tilo/env-runtime:$tag"

if [ "$mode" = tag ]; then
  printf '%s\n' "$tag"
  exit 0
fi

command -v skopeo >/dev/null \
  || { printf 'resolve-env-runtime: skopeo is required\n' >&2; exit 3; }

err="$(mktemp)"
trap 'rm -f -- "$err"' EXIT
# Exit 1: the tag does not exist. Exit 3: the registry could not be asked,
# which says nothing about the tag.
if ! digest="$(skopeo inspect --format '{{.Digest}}' "docker://$tagged" 2>"$err")"; then
  if grep -qE 'manifest unknown|name unknown|not found' "$err"; then
    printf 'resolve-env-runtime: no published env-runtime for %s\n' "$tagged" >&2
    exit 1
  fi
  printf 'resolve-env-runtime: could not look up %s: %s\n' "$tagged" "$(tail -n 1 "$err")" >&2
  exit 3
fi
[[ "$digest" =~ ^sha256:[0-9a-f]{64}$ ]] \
  || { printf 'resolve-env-runtime: tag %s has no digest\n' "$tagged" >&2; exit 1; }
ref="registry.tilo.so/tilo/env-runtime@$digest"

if [ "$mode" = exists ]; then
  exit 0
fi

command -v cosign >/dev/null \
  || { printf 'resolve-env-runtime: cosign is required\n' >&2; exit 1; }
cosign verify --key "$repo/keys/cosign.pub" "$ref" >/dev/null \
  || { printf 'resolve-env-runtime: signature does not verify for %s\n' "$ref" >&2; exit 1; }

printf '%s\n' "$ref"
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  {
    printf 'env_runtime_ref=%s\n' "$ref"
    printf 'tag=%s\n' "$tag"
  } >> "$GITHUB_OUTPUT"
fi
