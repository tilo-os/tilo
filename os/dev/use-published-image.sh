#!/usr/bin/env bash
# Verify and load one immutable, already-published Tilo release as TILO_IMAGE.
# The release Imager uses this instead of allocating a generation for an OS
# image it will never publish.  stdout is the verified embedded generation.
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

[ "$#" -eq 1 ] || die "usage: $0 code.sandbox.example/qos/tilo@sha256:<digest>"
image="$1"
case "$image" in
  code.sandbox.example/qos/tilo@sha256:*) ;;
  *) die "published Tilo image must be an immutable code.sandbox.example/qos/tilo digest" ;;
esac
digest="${image#code.sandbox.example/qos/tilo@sha256:}"
[[ "$digest" =~ ^[0-9a-f]{64}$ ]] \
  || die "published Tilo image digest must be 64 lowercase hex characters"

for tool in cosign jq podman; do
  command -v "$tool" >/dev/null || die "$tool is required to consume a published Tilo image"
done
verify_key="$TILO_REPO/keys/cosign.pub"
[ -s "$verify_key" ] || die "Tilo release verification key is missing: $verify_key"

# Podman accepts an explicit containers-auth file, while cosign reads Docker's
# config directory.  Give both clients the same short-lived registry identity;
# otherwise a private image can be pulled but its signature attachments cannot
# be read.  Never copy credentials unless the caller explicitly supplied them.
auth_dir=""
cleanup_auth() {
  [ -z "$auth_dir" ] || rm -r -- "$auth_dir"
}
trap cleanup_auth EXIT
manifest_args=()
pull_args=()
if [ -n "${REGISTRY_AUTH_FILE:-}" ]; then
  [ -s "$REGISTRY_AUTH_FILE" ] || die "REGISTRY_AUTH_FILE does not name a populated credential file"
  auth_dir="$(mktemp -d)"
  chmod 0700 "$auth_dir"
  cp -- "$REGISTRY_AUTH_FILE" "$auth_dir/config.json"
  chmod 0600 "$auth_dir/config.json"
  export DOCKER_CONFIG="$auth_dir"
  manifest_args+=(--authfile "$REGISTRY_AUTH_FILE")
  pull_args+=(--authfile "$REGISTRY_AUTH_FILE")
fi

signatures="$(cosign verify --key "$verify_key" "$image")" \
  || die "published Tilo image has no valid transparency-logged release signature"
signed_digest="$(printf '%s' "$signatures" | jq -er \
  'if type == "array" then .[] else . end
   | .critical.image["docker-manifest-digest"]
   | select(type == "string")' | sort -u)" \
  || die "published Tilo signature does not bind an image digest"
[ "$signed_digest" = "sha256:$digest" ] \
  || die "published Tilo signature binds $signed_digest, not sha256:$digest"

provenance="$(cosign verify-attestation --key "$verify_key" \
  --type slsaprovenance1 "$image")" \
  || die "published Tilo image has no valid transparency-logged SLSA provenance"

# A release input is the signed OCI index, not either independently signed
# candidate child.  The index itself cryptographically binds these descriptors;
# provenance must name the same pair so release metadata cannot describe a
# different architecture set from the object clients actually pull.
raw_index="$(podman manifest inspect "${manifest_args[@]}" "$image")" \
  || die "published Tilo image is not a readable multi-architecture index"
children="$(printf '%s' "$raw_index" | jq -ce '
  if (.manifests | type) != "array" or (.manifests | length) != 2 then
    error("expected exactly two manifests")
  else
    [.manifests[]
      | select(.platform.os == "linux")
      | {architecture:
          (if .platform.architecture == "amd64" then "x86_64"
           elif .platform.architecture == "arm64"
                and ((.platform.variant // "") == "" or .platform.variant == "v8")
             then "aarch64"
           else error("unsupported platform") end),
         digest: .digest}]
    | sort_by(.architecture)
    | if map(.architecture) == ["aarch64", "x86_64"]
         and (map(.digest) | unique | length) == 2
         and all(.[]; .digest | test("^sha256:[0-9a-f]{64}$"))
      then . else error("invalid native manifest set") end
  end
')" || die "published Tilo image is not the exact Linux x86-64/ARM64 release index"

expected_source="${TILO_EXPECTED_OS_SOURCE_REVISION:-}"
expected_source_uri=""
if [ -n "$expected_source" ]; then
  [[ "$expected_source" =~ ^[0-9a-f]{40}$ ]] \
    || die "TILO_EXPECTED_OS_SOURCE_REVISION must be a full lowercase Git commit"
  expected_source_uri="git+ssh://code.sandbox.example/qos/qos@$expected_source"
fi
generation="$(printf '%s' "$provenance" | jq -er \
  --arg digest "sha256:$digest" --argjson children "$children" \
  --arg source "$expected_source_uri" '
  [(if type == "array" then .[] else . end)
    | .payload | @base64d | fromjson
    | .predicate
    | select(.buildDefinition.internalParameters.expectedReleaseDigest == $digest)
    | select((.buildDefinition.externalParameters.children | sort_by(.architecture)) == $children)
    | select($source == "" or
        any(.buildDefinition.resolvedDependencies[]?; .uri == $source))
    | .buildDefinition.externalParameters.generation
    | select(type == "number" and floor == . and . >= 1)]
  | unique
  | if length == 1 then .[0] else error("ambiguous generation") end
')" || die "published Tilo provenance does not bind its digest, platforms, source, and exactly one positive generation"

# Signing a candidate does not publish it.  The immutable generation tag is the
# dense series' commit record, so require that signed tag to resolve to this
# exact index before installer media can consume it.
generation_image="code.sandbox.example/qos/tilo:gen$generation"
generation_signatures="$(cosign verify --key "$verify_key" "$generation_image")" \
  || die "published generation tag gen$generation has no valid release signature"
generation_digest="$(printf '%s' "$generation_signatures" | jq -er '
  [if type == "array" then .[] else . end
   | .critical.image["docker-manifest-digest"]
   | select(type == "string")]
  | unique
  | if length == 1 then .[0] else error("ambiguous generation digest") end
')" || die "published generation tag gen$generation does not bind exactly one digest"
[ "$generation_digest" = "sha256:$digest" ] \
  || die "published generation tag gen$generation binds $generation_digest, not sha256:$digest"

podman pull "${pull_args[@]}" "$image" >/dev/null \
  || die "could not pull the verified published Tilo image"
podman tag "$image" "$TILO_IMAGE"
embedded="$(podman run --rm --entrypoint cat "$TILO_IMAGE" \
  /usr/lib/tilo/generation 2>/dev/null | tr -d '[:space:]')"
[ "$embedded" = "$generation" ] \
  || die "published Tilo image embeds generation '$embedded', but provenance binds '$generation'"

printf '%s\n' "$generation"
