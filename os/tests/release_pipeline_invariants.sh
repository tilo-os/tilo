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

cache_driver_ok=1
for workflow in build-builder-image.yml build-image.yml build-kernel.yml qos-relay.yaml snapshot-rpms.yml; do
  file="$workflows/$workflow"
  if grep -Fq 'cache-to: type=registry' "$file" \
     && { ! grep -Fq 'docker/setup-buildx-action@8d2750c68a42422c14e847fe6c8ac0403b4cbd6f' "$file" \
          || ! grep -Fq 'driver: docker-container' "$file"; }; then
    printf 'registry cache workflow lacks a cache-export-capable Buildx driver: %s\n' \
      "$workflow" >&2
    cache_driver_ok=0
  fi
done
if [ "$cache_driver_ok" = 1 ]; then
  ok 'registry cache workflows select the docker-container Buildx driver'
else
  bad 'a registry cache workflow still uses the default docker driver'
fi

grep -Fq 'qos-kernel-buildcache:latest,mode=min' "$workflows/build-kernel.yml" \
  && ! grep -Fq 'qos-kernel-buildcache:latest,mode=max' "$workflows/build-kernel.yml" \
  && ok 'kernel cache export excludes its multi-gigabyte intermediate build tree' \
  || bad 'kernel cache export can duplicate the intermediate build tree and exhaust the runner'

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

grep -Fq 'publish-os-payload:' "$desktop" \
  && grep -Fq 'QOS_DESKTOP_RELEASE_AUDIENCE' "$desktop" \
  && grep -Fq '"with_browser":"1","publish":"true"' "$desktop" \
  && grep -Fq 'QOS_DESKTOP_PAYLOAD_URL_X86_64' "$os" \
  && grep -Fq 'QOS_PREBUILT_DESKTOP=${{ env.QOS_PREBUILT_DESKTOP }}' "$os" \
  && grep -Fq 'DESKTOP_RELEASE_ARGS=""' "$repo/os/Containerfile" \
  && ok 'OS composition reuses the install-tested native desktop payload' \
  || bad 'OS releases still rebuild the desktop component'

grep -Fq 'with_browser=0' "$os" \
  && grep -Fq 'WITH_BROWSER: ${{ env.QOS_WITH_BROWSER }}' "$os" \
  && grep -Fq 'max-parallelism = 2' "$os" \
  && grep -Fq 'find /build/target -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +' "$repo/os/Containerfile" \
  && grep -Fq 'CONTINUOUS_RELEASE: ${{ vars.QOS_CONTINUOUS_OS_RELEASE }}' "$os" \
  && ! grep -Fq 'token: ${{ secrets.REGISTRY_TOKEN }}' "$os" \
  && ! grep -Fq 'fetch-depth: 0' "$os" \
  && grep -Fq "if: steps.release-mode.outputs.validate == 'true'" "$os" \
  && grep -Fq 'installed OS, desktop, and app integration passed; publication was not requested' "$os" \
  && grep -Fq "if: steps.release-mode.outputs.publish == 'true'" "$os" \
  && ok 'ordinary OS integration checks omit the release-only browser engine' \
  || bad 'OS branch validation still rebuilds or requires the WPE release snapshot'

grep -Fq 'matrix: ${{ fromJSON(needs.catalog.outputs.linux-matrix) }}' "$imager" \
  && grep -Fq 'github.event.inputs.full_native' "$imager" \
  && grep -Fq 'runner":"imager-linux-x86_64"' "$imager" \
  && grep -Fq 'runner":"imager-linux-aarch64"' "$imager" \
  && ! grep -Fq 'pull_request:' "$imager" \
  && ! grep -Fq 'branches: ["main"]' "$imager" \
  && ok 'imager native jobs are reserved for explicit release rehearsals and tags' \
  || bad 'ordinary commits can still allocate or misroute native imager hosts'

grep -Fq 'release/app-collections.toml' "$apps" \
  && grep -Fq '$package_api/qos-app-collections/$VERSION/' "$apps" \
  && grep -Fq '$package_api/qos-app-$name/$VERSION/$file' "$apps" \
  && grep -Fq 'PRIOR_X86_URL' "$apps" \
  && grep -Fq 'if [[ $GITHUB_REF == refs/heads/main ]]; then' "$apps" \
  && grep -Fq 'dist/app-release/runtime/**' "$apps" \
  && grep -Fq 'update_variable QOS_APP_COLLECTION_URL' "$apps" \
  && grep -Fq '$GITHUB_EVENT_NAME == push && $GITHUB_REF == refs/heads/main' "$apps" \
  && grep -Fq '${REQUESTED_PUBLISH:-false} == true' "$apps" \
  && grep -Fq 'REQUESTED_ARCHITECTURES: ${{ github.event.inputs.architectures }}' "$apps" \
  && grep -Fq 'tools/app_release.py --architecture-matrix' "$apps" \
  && grep -Fq 'image: code.podesta.ai/qos/qos-builder:latest' "$apps" \
  && grep -Fq 'actions/upload-artifact@ff15f0306b3f739f7b6fd43fb5d26cd321bd4de5' "$apps" \
  && grep -Fq 'enable-openid-connect: true' "$apps" \
  && grep -Fq 'QOS_APP_RELEASE_TAG_AUDIENCE' "$apps" \
  && ! awk '/^  publish:/{publish=1} publish' "$apps" | grep -Fq 'secrets.REGISTRY_TOKEN' \
  && ok 'app collections are durable across workflows and use Forgejo-compatible artifacts' \
  || bad 'app release artifacts cannot be consumed by later component releases'

grep -Fq 'QOS_APP_COLLECTION_SHA256' "$os" \
  && grep -Fq 'QOS_APP_COLLECTION_URL_AARCH64' "$os" \
  && grep -Fq 'enable-openid-connect: true' "$os" \
  && grep -Fq 'QOS_OS_APP_COLLECTION_AUDIENCE' "$os" \
  && test "$(grep -Fc 'http://forgejo.forgejo.svc.cluster.local:3000/api/packages/qos/generic/*)' "$os")" -eq 2 \
  && test "$(grep -Fc -- "--proto '=https' --proto-redir '=https'" "$os")" -eq 2 \
  && test "$(grep -Fc -- "--proto '=http' --fail --retry 3" "$os")" -eq 2 \
  && ! grep -Fq '[[ $EVENT_NAME != pull_request ]] || exit 0' "$os" \
  && grep -Fq 'Authorization: Bearer $jwt' "$os" \
  && grep -Fq 'QOS_PREBUILT_APPS=${{ env.QOS_PREBUILT_APPS }}' "$os" \
  && grep -Fq 'QPKG_DIR=/out/prebuilt-apps' "$repo/os/Containerfile" \
  && grep -Fq 'cp -a /build/os/app-runtime/qos-runtime /usr/lib/' "$repo/os/Containerfile" \
  && grep -Fq 'RUNTIME_BUNDLE_IDENTITY_FILE' "$repo/dist/apps/package-apps.sh" \
  && grep -Fq 'shared runtime changed; the full collection must be rebuilt' "$apps" \
  && ok 'OS builds consume OIDC-authenticated pinned app and desktop inputs without rebuilding them' \
  || bad 'OS releases are still coupled to app source builds'

grep -Fq 'SCCACHE_SHA256_AARCH64=821a86343191aa1cbab74bd42f9e93c9a63bf85e4742945f40d3ae84193c1c77' "$repo/os/Containerfile" \
  && grep -Fq 'sccache_arch=aarch64' "$repo/os/Containerfile" \
  && grep -Fq 'SCCACHE_SHA256_AARCH64=821a86343191aa1cbab74bd42f9e93c9a63bf85e4742945f40d3ae84193c1c77' "$repo/os/Containerfile.builder" \
  && grep -Fq 'sccache_arch=aarch64' "$repo/os/Containerfile.builder" \
  && ok 'OS and CI builder images install checksum-pinned native cache clients' \
  || bad 'an ARM build image can still install the x86 compiler-cache client'

python3 "$repo/tools/app_release.py" --collection default > /dev/null \
  && python3 "$repo/tools/app_release.py" --collection recovery > /dev/null \
  && ok 'named app collections resolve against the live manifest catalogue' \
  || bad 'an app collection is stale or invalid'

exit "$fail"
