#!/usr/bin/env bash
# Cheap policy checks for the component release boundaries. These catch the
# expensive mistakes (coupled tags, full native matrices on every PR, ignored
# Forgejo keys) before a runner is allocated.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
workflows="$repo/.forgejo/workflows"
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

if grep -R -n -E '^[[:space:]]*permissions:' "$workflows"; then
  bad 'Forgejo-ignored permissions keys remain in workflows'
else
  ok 'workflows use Forgejo authorization/OIDC controls without ignored permissions keys'
fi

desktop="$workflows/package-desktop.yml"
apps="$workflows/release-apps.yml"
os="$workflows/build-image.yml"
imager="$workflows/release-imager.yml"

grep -Fq 'tags: ["desktop-v*"]' "$desktop" \
  && grep -Fq 'tags: ["apps-v*", "app-*-v*"]' "$apps" \
  && grep -Fq 'tags: ["v*"]' "$os" \
  && ok 'desktop, app and OS component tags are disjoint' \
  || bad 'a component tag can accidentally release another component'

grep -Fq 'branches: ["main", "codex/package-desktop-release"]' "$desktop" \
  && [ "$(grep -c '      - "shell/\*\*"' "$desktop")" -eq 2 ] \
  && grep -Fq 'runner:"runner-arm64"' "$desktop" \
  && grep -Fq 'verify-release-artifacts.sh release "$version" amd64,arm64' "$desktop" \
  && ok 'desktop push and pull-request triggers are path-scoped' \
  || bad 'desktop packaging boundaries or native release lanes regressed'

grep -Fq 'with_browser=0' "$os" \
  && grep -Fq 'WITH_BROWSER: ${{ env.QOS_WITH_BROWSER }}' "$os" \
  && ok 'ordinary OS integration checks omit the release-only browser engine' \
  || bad 'OS branch validation still rebuilds or requires the WPE release snapshot'

grep -Fq 'matrix: ${{ fromJSON(needs.catalog.outputs.linux-matrix) }}' "$imager" \
  && grep -Fq 'github.event.inputs.full_native' "$imager" \
  && ok 'imager branch validation uses one native smoke lane' \
  || bad 'imager changes still allocate the complete host matrix'

python3 "$repo/tools/check_workflow_package_paths.py" \
  imager .forgejo/workflows/release-imager.yml \
  && ok 'imager path filters match its live workspace dependency closure' \
  || bad 'imager dependency path filters are stale'

grep -Fq 'release/app-collections.toml' "$apps" \
  && grep -Fq '$package_api/qos-app-collections/$VERSION/' "$apps" \
  && grep -Fq '$package_api/qos-app-$name/$VERSION/$file' "$apps" \
  && grep -Fq 'PRIOR_X86_URL' "$apps" \
  && grep -Fq 'update_variable QOS_APP_COLLECTION_URL' "$apps" \
  && grep -Fq '$GITHUB_REF == refs/heads/main' "$apps" \
  && grep -Fq 'REQUESTED_ARCHITECTURES: ${{ github.event.inputs.architectures }}' "$apps" \
  && grep -Fq 'tools/app_release.py --architecture-matrix' "$apps" \
  && grep -Fq 'actions/upload-artifact@ff15f0306b3f739f7b6fd43fb5d26cd321bd4de5' "$apps" \
  && grep -Fq 'enable-openid-connect: true' "$apps" \
  && grep -Fq 'QOS_APP_RELEASE_TAG_AUDIENCE' "$apps" \
  && ! grep -Fq 'secrets.REGISTRY_TOKEN' "$apps" \
  && ok 'app collections are durable across workflows and use Forgejo-compatible artifacts' \
  || bad 'app release artifacts cannot be consumed by later component releases'

grep -Fq 'QOS_APP_COLLECTION_SHA256' "$os" \
  && grep -Fq 'QOS_APP_COLLECTION_URL_AARCH64' "$os" \
  && grep -Fq 'enable-openid-connect: true' "$os" \
  && grep -Fq 'QOS_OS_APP_COLLECTION_AUDIENCE' "$os" \
  && grep -Fq '[[ $EVENT_NAME != pull_request ]] || exit 0' "$os" \
  && grep -Fq 'Authorization: Bearer $jwt' "$os" \
  && grep -Fq 'QOS_PREBUILT_APPS=${{ env.QOS_PREBUILT_APPS }}' "$os" \
  && grep -Fq 'QPKG_DIR=/out/prebuilt-apps' "$repo/os/Containerfile" \
  && ok 'trusted OS builds consume an OIDC-authenticated pinned app collection without rebuilding apps' \
  || bad 'OS releases are still coupled to app source builds'

python3 "$repo/tools/app_release.py" --collection default > /dev/null \
  && python3 "$repo/tools/app_release.py" --collection recovery > /dev/null \
  && ok 'named app collections resolve against the live manifest catalogue' \
  || bad 'an app collection is stale or invalid'

exit "$fail"
