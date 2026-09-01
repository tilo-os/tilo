#!/usr/bin/env bash
# Static and executable checks for the OS release boundary. These are cheap
# enough for ordinary CI; the workflow still performs the expensive independent
# rebuild, VM boot and signature verification on a real release.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$repo/.forgejo/workflows/build-image.yml"
provenance="$repo/os/dev/release-provenance.sh"
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

grep -Fq 'group: qos-os-image-series' "$workflow" \
  && ok 'OS generation publication is globally serialized' \
  || bad 'release jobs can race and reuse one generation'

if grep -Fq "github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/main'" "$workflow" \
   && ! grep -Eq "if:.*github.event_name == 'workflow_dispatch'[[:space:]]*}}" "$workflow"; then
  ok 'manual publication is restricted to main'
else
  bad 'a manual feature-branch dispatch can reach a publication step'
fi

grep -Fq 'Independently rebuild the release candidate' "$workflow" \
  && grep -Fq 'no-cache: true' "$workflow" \
  && grep -Fq 'independent OS builds differ' "$workflow" \
  && grep -Fq 'final sealed OS images differ' "$workflow" \
  && grep -Fq 'os/dev/reseal-image.sh' "$workflow" \
  && grep -Fq -- '--timestamp "$SOURCE_DATE_EPOCH"' "$repo/os/dev/reseal-image.sh" \
  && ok 'release compares uncached base and final sealed OS rebuilds' \
  || bad 'reproducibility is claimed without a release-blocking comparison'

sign_line="$(grep -n 'cosign sign ' "$workflow" | head -1 | cut -d: -f1)"
promote_line="$(grep -n 'promote release channels' "$workflow" | head -1 | cut -d: -f1)"
[ -n "$sign_line" ] && [ -n "$promote_line" ] && [ "$sign_line" -lt "$promote_line" ] \
  && ok 'channel tags are promoted only after signing and attestation' \
  || bad 'release channels can expose an unsigned candidate'

grep -Fq 'COSIGN_KEY_REF must name a non-exportable KMS/HSM key' "$workflow" \
  && ! grep -Fq 'COSIGN_PRIVATE_KEY' "$workflow" \
  && ok 'release signing requires a non-exportable KMS/HSM key' \
  || bad 'release signing still accepts an exportable private key secret'

if grep -Eq -- '--tlog-upload=false|--insecure-ignore-tlog' "$workflow"; then
  bad 'OS release disables transparency-log publication or verification'
else
  ok 'release signatures and attestations require transparency-log evidence'
fi

grep -Fq 'verify_signed_generation "$current"' "$repo/os/dev/next-generation.sh" \
  && grep -Fq 'latest does not point at signed gen$current' "$repo/os/dev/next-generation.sh" \
  && ok 'generation allocation authenticates provenance and the latest channel' \
  || bad 'generation allocation still trusts registry tags alone'

if grep -E 'uses: [^#[:space:]]+@(v[0-9]|main|master)([[:space:]#]|$)' "$workflow"; then
  bad 'OS release workflow executes a mutable third-party action reference'
else
  ok 'OS release workflow pins every third-party action by commit'
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
hex="$(printf 'a%.0s' {1..64})"
if RELEASE_DIGEST="sha256:$hex" QOS_GENERATION=42 SOURCE_DATE_EPOCH=1 \
   BASE_REF="base@sha256:$hex" RPMS_REF="rpms@sha256:$hex" \
   KERNEL_RPMS_REF="kernel@sha256:$hex" GITHUB_SHA="$hex" GITHUB_RUN_ID=test \
   "$provenance" "$tmp/provenance.json" \
   && jq -e '.buildDefinition.externalParameters.generation == 42' "$tmp/provenance.json" >/dev/null \
   && jq -e '.subject[0].digest.sha256 | length == 64' "$tmp/provenance.json" >/dev/null; then
  ok 'SLSA provenance binds generation, source inputs and image digest'
else
  bad 'release provenance generator did not produce a bound predicate'
fi

if RELEASE_DIGEST="sha256:$hex" QOS_GENERATION=42 SOURCE_DATE_EPOCH=1 \
   BASE_REF='base:latest' RPMS_REF="rpms@sha256:$hex" \
   KERNEL_RPMS_REF="kernel@sha256:$hex" GITHUB_SHA="$hex" \
   "$provenance" "$tmp/bad.json" >/dev/null 2>&1; then
  bad 'provenance accepted an unpinned build dependency'
else
  ok 'provenance refuses unpinned build dependencies'
fi

exit "$fail"
