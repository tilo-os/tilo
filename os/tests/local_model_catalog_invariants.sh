#!/usr/bin/env bash
# Release fixtures must exercise immutable, public-only local-model catalogs.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source_json="$repo/crates/agent/local-model-service/tests/fixtures/catalog-source.example.json"
runtime="$repo/crates/agent/local-model-service/tests/fixtures/example-runtime.bin"
model="$repo/crates/agent/local-model-service/tests/fixtures/example-model.bin"
tool="$repo/crates/agent/local-model-service/src/bin/qos-model-catalog.rs"
fail=0

ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

jq -e '.schema == 1 and .production == false' "$source_json" >/dev/null \
    || bad 'checked-in example is not explicitly non-production schema 1'
jq -e '
  (.models | length > 0) and (.runtimes | length > 0) and
  (all(.models[]; . as $model |
    ($model.revision | test("^[0-9a-f]{40}$|^[0-9a-f]{64}$")) and
    ($model.artifact.url | contains($model.revision)) and
    ($model.license_id | length > 0) and
    ($model.license_url | startswith("https://")) and
    ($model.installed_size_bytes >= $model.artifact.size_bytes))) and
  (all(.runtimes[];
    (.oci_image | test("@sha256:[0-9a-f]{64}$")) and
    (.base_image_digest | test("^sha256:[0-9a-f]{64}$"))))
' "$source_json" >/dev/null || bad 'fixture contains a mutable or incomplete release entry'

runtime_hash="$(sha256sum "$runtime" | awk '{print $1}')"
model_hash="$(sha256sum "$model" | awk '{print $1}')"
runtime_size="$(wc -c < "$runtime" | tr -d ' ')"
model_size="$(wc -c < "$model" | tr -d ' ')"
jq -e --arg hash "$runtime_hash" --argjson size "$runtime_size" \
    '.runtimes[0].artifact.sha256 == $hash and .runtimes[0].artifact.size_bytes == $size' \
    "$source_json" >/dev/null || bad 'runtime fixture receipt drifted'
jq -e --arg hash "$model_hash" --argjson size "$model_size" \
    '.models[0].artifact.sha256 == $hash and .models[0].artifact.size_bytes == $size' \
    "$source_json" >/dev/null || bad 'model fixture receipt drifted'

grep -Fq 'qos.local-model.catalog.v1' "$tool" \
    || bad 'catalog signature domain is not pinned in release tooling'
grep -Fq 'signing_key.verifying_key()' "$tool" \
    || bad 'release tooling does not derive public image trust from the signer'
if grep -R -Eiq 'private[_ -]?key|signing[_ -]?key|secret[_ -]?key' \
    "$repo/crates/agent/local-model-service/tests/fixtures"; then
    bad 'checked-in catalog fixtures contain private-key material'
else
    ok 'catalog fixtures contain no release private key'
fi

if grep -Eiq '/(main|master|latest)/|:latest(["/]|$)|/tags/' "$source_json"; then
    bad 'catalog fixture contains a mutable artifact reference'
else
    ok 'catalog fixture uses immutable revisions and digest-pinned OCI identity'
fi

[ "$fail" -eq 0 ] || exit "$fail"
ok 'model and runtime fixture receipts match declarative catalog source'
