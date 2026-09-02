#!/usr/bin/env bash
# Cheap release-boundary checks for the cross-platform qOS Imager pipeline.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$repo/.forgejo/workflows/release-imager.yml"
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

bash -n "$repo/apps/imager/packaging/package-host.sh" \
  "$repo/apps/imager/packaging/package-install-medium.sh" \
  "$repo/apps/imager/packaging/build-release-catalog.sh" \
  "$repo/apps/imager/packaging/publish-forgejo-release.sh" \
  "$repo/apps/imager/packaging/publish-homebrew-tap.sh" \
  "$repo/apps/imager/packaging/publish-site-release.sh" \
  "$repo/apps/imager/packaging/publish-winget.sh" \
  && ok 'imager release and promotion scripts parse' \
  || bad 'an imager release script does not parse'

grep -Fq 'QOS_REQUIRE_SIGNING=1' "$workflow" \
  && grep -Fq 'QOS_REQUIRE_NATIVE_PACKAGES=1' "$workflow" \
  && ok 'tag releases fail closed without signed native packages' \
  || bad 'tag releases can publish unsigned or substitute packages'

if grep -Eq 'uses: [^#[:space:]]+@(v[0-9]|main|master)([[:space:]#]|$)' "$workflow"; then
  bad 'release workflow executes a mutable third-party action reference'
else
  ok 'release workflow pins third-party actions by commit'
fi

grep -Fq 'id-token: write' "$workflow" \
  && grep -Fq 'IMAGER_CREDENTIAL_BROKER' "$workflow" \
  && grep -Fq 'QOS_LINUX_SIGNER' "$repo/apps/imager/packaging/package-host.sh" \
  && grep -Fq 'QOS_NOTARY_KEYCHAIN_PROFILE' "$repo/apps/imager/packaging/package-host.sh" \
  && grep -Fq 'QOS_WINDOWS_SIGNER' "$repo/apps/imager/packaging/package-host.ps1" \
  && ok 'native jobs use OIDC-backed signing without exported platform keys' \
  || bad 'short-lived signing path is not wired into native release jobs'

grep -Fq 'AppImage.asc' "$workflow" \
  && ! grep -Fqi 'qos-imager-linux-*.flatpak' "$workflow" \
  && ok 'every Linux download is signed and insecure Flatpak flashing is absent' \
  || bad 'Linux release artifacts are unsigned or still include privileged Flatpak flashing'

grep -Fq 'qos/homebrew-qos' "$repo/apps/imager/packaging/publish-homebrew-tap.sh" \
  && grep -Fq 'microsoft/winget-pkgs/pulls' "$repo/apps/imager/packaging/publish-winget.sh" \
  && grep -Fq -- '--retry 6 --retry-all-errors --retry-max-time 180' "$repo/apps/imager/packaging/publish-winget.sh" \
  && grep -Fq 'state=open&base=master&head=' "$repo/apps/imager/packaging/publish-winget.sh" \
  && grep -Fq '"$CREDENTIAL_BROKER" homebrew "$token_file"' "$workflow" \
  && grep -Fq '"$CREDENTIAL_BROKER" winget "$token_file"' "$workflow" \
  && grep -Fq 'ALLOW_STATIC_BOOTSTRAP' "$workflow" \
  && ok 'first-party Homebrew and public WinGet promotion are automated' \
  || bad 'a package-index promotion path is missing'

grep -Fq 'site-downloads:' "$workflow" \
  && grep -Fq 'test:release' "$repo/apps/imager/packaging/publish-site-release.sh" \
  && ok 'landing-page manifest advances only through the gated release workflow' \
  || bad 'landing-page release publication is not wired'

grep -Fq 'qos-installer-medium-${{ matrix.arch }}' "$workflow" \
  && grep -Fq 'qos-installer-catalog-v1.json' "$workflow" \
  && grep -Fq 'image_sha256' "$repo/apps/imager/src/release.rs" \
  && grep -Fq 'archive_sha256' "$repo/apps/imager/src/release.rs" \
  && ok 'native live media is catalogued and verified before writing' \
  || bad 'end-user install medium publication or verification is missing'

grep -Fq 'group: release-imager-promotion' "$workflow" \
  && grep -Fq 'refusing to replace newer Homebrew release' "$repo/apps/imager/packaging/publish-homebrew-tap.sh" \
  && grep -Fq 'refusing to replace newer landing-page release' "$repo/apps/imager/packaging/publish-site-release.sh" \
  && ok 'release promotion is serialized and monotonic' \
  || bad 'an older release can overwrite a newer promoted release'

exit "$fail"
