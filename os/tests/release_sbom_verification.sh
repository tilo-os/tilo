#!/usr/bin/env bash
set -euo pipefail
repo="$(cd "$(dirname "$0")/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
export MOCK_PROVENANCE="$work/provenance.json" MOCK_SBOM="$work/sbom.json"
mkdir "$work/bin"
cat > "$work/bin/cosign" <<'EOF'
#!/usr/bin/env bash
case "$1" in
  verify-attestation) cat "$MOCK_PROVENANCE" ;;
  download) [ "$2" = sbom ] && cat "$MOCK_SBOM" ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$work/bin/cosign"
export PATH="$work/bin:$PATH"
image="registry.tilo.so/tilo/tilo@sha256:$(printf 'a%.0s' {1..64})"
digest="${image##*@}"
printf '{"spdxVersion":"SPDX-2.3","name":"fixture"}\n' > "$MOCK_SBOM"
sbom_hash="$(sha256sum "$MOCK_SBOM" | cut -d' ' -f1)"

provenance() {
  local image_digest="$1" byproducts="$2" payload
  payload="$(jq -nc --arg digest "$image_digest" --argjson byproducts "$byproducts" '
    {predicate:{buildDefinition:{internalParameters:{expectedReleaseDigest:$digest}},
      runDetails:{byproducts:$byproducts}}}' | base64 -w0)"
  jq -nc --arg payload "$payload" '[{payload:$payload}]' > "$MOCK_PROVENANCE"
}
accept='[{"name":"spdx-sbom","uri":"sha256:'"$sbom_hash"'"}]'
provenance "$digest" "$accept"
"$repo/os/dev/verify-release-sbom.sh" "$image" > "$work/out"
grep -Fq "verified SPDX SBOM sha256:$sbom_hash" "$work/out"

printf '{"spdxVersion":"SPDX-2.3","name":"tampered"}\n' > "$MOCK_SBOM"
if "$repo/os/dev/verify-release-sbom.sh" "$image" > "$work/out" 2>&1; then
  echo 'tampered SBOM was accepted' >&2; exit 1
fi
printf '{"spdxVersion":"SPDX-2.3","name":"fixture"}\n' > "$MOCK_SBOM"

provenance "sha256:$(printf 'b%.0s' {1..64})" "$accept"
if "$repo/os/dev/verify-release-sbom.sh" "$image" > "$work/out" 2>&1; then
  echo 'provenance for another image was accepted' >&2; exit 1
fi

conflict='[{"name":"spdx-sbom","uri":"sha256:'"$sbom_hash"'"},
  {"name":"spdx-sbom","uri":"sha256:'"$(printf 'c%.0s' {1..64})"'"}]'
provenance "$digest" "$conflict"
if "$repo/os/dev/verify-release-sbom.sh" "$image" > "$work/out" 2>&1; then
  echo 'conflicting SBOM digests were accepted' >&2; exit 1
fi

provenance "$digest" '[]'
if "$repo/os/dev/verify-release-sbom.sh" "$image" > "$work/out" 2>&1; then
  echo 'missing SBOM digest was accepted' >&2; exit 1
fi
echo 'release SBOM provenance binding rejects missing, mismatched and conflicting digests'
