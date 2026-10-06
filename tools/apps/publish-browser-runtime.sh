#!/usr/bin/env bash
# The explicit main-only release-apps job supplies this run's engine artifacts.
set -euo pipefail
[[ $GITHUB_EVENT_NAME == workflow_dispatch && $GITHUB_REF == refs/heads/main ]]
[[ $GITHUB_SHA =~ ^[0-9a-f]{40}$ && $(git rev-parse HEAD) == "$GITHUB_SHA" ]]
: "${MAIN_AUDIENCE:?}" "${TAPP_PUBLISHER_IDENTITY:?}" "${ENGINE_ARCHITECTURES:?}"
host_arch=$(uname -m)
host_bin="engine-release/browser-engine-runtime-$host_arch/bin"
[[ -d $host_bin ]]
chmod 0755 "$host_bin/tilo-runtime-bundle" "$host_bin/tilo-app-package"
identity="$RUNNER_TEMP/tilo-browser-runtime-publisher.json"
trap 'rm -f "$identity"' EXIT
(umask 077; printf '%s' "$TAPP_PUBLISHER_IDENTITY" > "$identity")
unset TAPP_PUBLISHER_IDENTITY
trusted_key=$("$host_bin/tilo-runtime-bundle" key pubkey --identity "$identity")
# Verify every selected artifact before acquiring publisher OIDC or uploading any.
mapfile -t architectures < <(jq -er '.include[].arch' <<< "$ENGINE_ARCHITECTURES")
[[ ${#architectures[@]} -gt 0 ]]
for arch in "${architectures[@]}"; do
  [[ $arch == x86_64 || $arch == aarch64 ]]
  TILO_ENGINE_VERIFIER_BIN_DIR="$host_bin" \
    python3 -I tools/ci/python.py tools/apps/browser_runtime_release.py verify \
      --out "engine-release/browser-engine-runtime-$arch" --arch "$arch" \
      --source "$GITHUB_SHA" --sequence "$GITHUB_RUN_NUMBER" --trusted-key "$trusted_key"
  # Reconstruct the signed thin-object tree before any architecture is uploaded.
  mapfile -t tapps < <(find "engine-release/browser-engine-runtime-$arch/package" -maxdepth 1 -name '*.tapp' -type f)
  [[ ${#tapps[@]} == 1 ]]
  "$host_bin/tilo-app-package" entry --arch "$arch" --identity "$identity" \
    --objects "engine-release/browser-engine-runtime-$arch/package" \
    --url "https://apps.tilo.so/artifacts/engine.tapp" "${tapps[0]}" > /dev/null
done
jwt=$(curl --fail --silent --show-error \
  --header "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
  "$ACTIONS_ID_TOKEN_REQUEST_URL&audience=$MAIN_AUDIENCE" | jq -er .value)
echo "::add-mask::$jwt"
metadata="$RUNNER_TEMP/tilo-browser-engine-metadata.json"
jq -n '{name: {en: "Browser engine"},
        description: {en: "The Chromium engine the Browser renders web pages with. The Store installs it together with the Browser."},
        tagline: {en: "Web engine for the Browser"}, supported_languages: ["en"],
        source_url: "https://code.tilo.so/tilo/tilo"}' > "$metadata"
for arch in "${architectures[@]}"; do
  VERSION="0.1.$GITHUB_RUN_NUMBER+g$GITHUB_SHA" TILO_PACKAGE_TOKEN="$jwt" \
    PACKAGE_API="$GITHUB_SERVER_URL/api/packages/${GITHUB_REPOSITORY%%/*}/generic" \
    REGISTRY_URL="${TILO_REGISTRY_URL%/}" OWNER="${GITHUB_REPOSITORY%%/*}" \
    REGISTRY_CLI="$host_bin/tilo-app-package" REGISTRY_IDENTITY="$identity" \
    tools/apps/publish-runtime-package.sh "engine-release/browser-engine-runtime-$arch/package" \
      "$arch" so.tilo.runtime.chromium tilo-app-runtime-chromium "$metadata"
done
