#!/usr/bin/env bash
# Existing release-apps authority; only an explicit immutable main event.
set -euo pipefail
[[ $GITHUB_EVENT_NAME == workflow_dispatch && $GITHUB_REF == refs/heads/main ]]
[[ $GITHUB_SHA =~ ^[0-9a-f]{40}$ && $(git rev-parse HEAD) == "$GITHUB_SHA" ]]
[[ $(uname -m) == x86_64 ]]
: "${MAIN_AUDIENCE:?}" "${TAPP_PUBLISHER_IDENTITY:?}"
out=signal-release/signal-provider-x86_64
bin="$out/bin"
chmod 0755 "$bin/tilo-app-package" "$bin/tilo-runtime-bundle"
identity="$RUNNER_TEMP/tilo-signal-app-publisher.json"
trap 'rm -f "$identity"' EXIT
(umask 077; printf '%s' "$TAPP_PUBLISHER_IDENTITY" > "$identity")
unset TAPP_PUBLISHER_IDENTITY
trusted_key=$("$bin/tilo-runtime-bundle" key pubkey --identity "$identity")
TILO_SIGNAL_VERIFIER_BIN_DIR="$bin" python3 -I tools/ci/python.py tools/apps/signal_provider_release.py verify \
  --out "$out" --source "$GITHUB_SHA" --sequence "$GITHUB_RUN_NUMBER" --trusted-key "$trusted_key"
# Reconstruct both signed object trees before acquiring publisher OIDC.
for directory in runtime-package provider-package; do
  mapfile -t packages < <(find "$out/$directory" -maxdepth 1 -name '*.tapp' -type f)
  [[ ${#packages[@]} == 1 ]]
  "$bin/tilo-app-package" entry --arch x86_64 --identity "$identity" --objects "$out/$directory" \
    --url 'https://apps.tilo.so/artifacts/signal.tapp' "${packages[0]}" > /dev/null
done
jwt=$(curl --fail --silent --show-error \
  --header "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
  "$ACTIONS_ID_TOKEN_REQUEST_URL&audience=$MAIN_AUDIENCE" | jq -er .value)
echo "::add-mask::$jwt"
metadata="$RUNNER_TEMP/tilo-signal-metadata.json"
for directory in runtime-package provider-package; do
  if [[ $directory == runtime-package ]]; then
    package=so.tilo.runtime.signal
    artifact=tilo-app-runtime-signal
    name='Signal runtime'
  else
    package=so.tilo.signal-provider
    artifact=tilo-app-signal-provider
    name='Signal provider'
  fi
  jq -n --arg name "$name" '{name: {en: $name},
    description: {en: "Optional private Signal service for Messages, started only after protected account and network review."},
    tagline: {en: "Private Messages service"}, supported_languages: ["en"],
    source_url: "https://code.tilo.so/tilo/tilo"}' > "$metadata"
  VERSION="0.1.$GITHUB_RUN_NUMBER+g$GITHUB_SHA" TILO_PACKAGE_TOKEN="$jwt" \
    PACKAGE_API="$GITHUB_SERVER_URL/api/packages/${GITHUB_REPOSITORY%%/*}/generic" \
    REGISTRY_URL="${TILO_REGISTRY_URL%/}" OWNER="${GITHUB_REPOSITORY%%/*}" \
    REGISTRY_CLI="$bin/tilo-app-package" REGISTRY_IDENTITY="$identity" \
    bash tools/apps/publish-runtime-package.sh "$out/$directory" x86_64 "$package" "$artifact" "$metadata"
done
