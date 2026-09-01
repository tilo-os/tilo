#!/usr/bin/env bash
# Exercise the generation resolver against an attacker-controlled registry view.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin"
printf 'test public key\n' > "$tmp/cosign.pub"

cat > "$tmp/bin/skopeo" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "${MOCK_TAGS:?}"
EOF
chmod +x "$tmp/bin/skopeo"

cat > "$tmp/bin/cosign" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
image="${*: -1}"
if [ "${MOCK_BAD_SIGNATURE:-0}" = 1 ]; then
  exit 1
fi
digest="sha256:$(printf 'a%.0s' {1..64})"
if [[ "$image" == *:latest ]] && [ "${MOCK_SPLIT_LATEST:-0}" = 1 ]; then
  digest="sha256:$(printf 'b%.0s' {1..64})"
fi
if [ "$1" = verify-attestation ]; then
  generation="${image##*:gen}"
  [ "$generation" != "$image" ] || generation=0
  [ "${MOCK_BAD_PROVENANCE:-0}" != 1 ] || generation=$((generation + 1))
  payload="$(jq -cn --argjson generation "$generation" '{predicate:{buildDefinition:{externalParameters:{generation:$generation}}}}' | base64 -w0)"
  jq -cn --arg payload "$payload" '[{payload:$payload}]'
else
  jq -cn --arg digest "$digest" '[{critical:{image:{"docker-manifest-digest":$digest}}}]'
fi
EOF
chmod +x "$tmp/bin/cosign"

resolve() {
  env PATH="$tmp/bin:$PATH" QOS_GENERATION_VERIFY_KEY="$tmp/cosign.pub" \
    QOS_PUBLISH_REPO=registry.invalid/qos MOCK_TAGS="$1" \
    "$repo/os/dev/next-generation.sh" 2>/dev/null
}

[ "$(resolve '{"Tags":["latest","gen1","gen2"]}')" = 3 ]

if resolve '{"Tags":["latest","gen2"]}' >/dev/null; then
  echo 'generation resolver accepted a deleted/gapped history' >&2
  exit 1
fi

if MOCK_BAD_PROVENANCE=1 resolve '{"Tags":["latest","gen1","gen2"]}' >/dev/null; then
  echo 'generation resolver accepted a tag/provenance mismatch' >&2
  exit 1
fi

if MOCK_SPLIT_LATEST=1 resolve '{"Tags":["latest","gen1","gen2"]}' >/dev/null; then
  echo 'generation resolver accepted latest/gen split view' >&2
  exit 1
fi

if MOCK_BAD_SIGNATURE=1 resolve '{"Tags":["latest","gen1"]}' >/dev/null; then
  echo 'generation resolver accepted an unsigned release' >&2
  exit 1
fi

printf 'ok    signed dense generation series rejects registry tampering\n'
