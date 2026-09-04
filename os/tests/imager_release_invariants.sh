#!/usr/bin/env bash
# Cheap release-boundary checks for the cross-platform qOS Imager pipeline.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$repo/.forgejo/workflows/release-imager.yml"
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

bash -n "$repo/apps/imager/packaging/package-host.sh" \
  "$repo/apps/imager/packaging/linux/qos-imager" \
  "$repo/apps/imager/packaging/macos/qos-imager" \
  "$repo/apps/imager/packaging/package-install-medium.sh" \
  "$repo/apps/imager/packaging/build-release-catalog.sh" \
  "$repo/apps/imager/packaging/verify-linux-release.sh" \
  "$repo/apps/imager/packaging/sign-linux-release-file.sh" \
  "$repo/apps/imager/packaging/publish-forgejo-release.sh" \
  "$repo/apps/imager/packaging/publish-homebrew-tap.sh" \
  "$repo/apps/imager/packaging/publish-site-release.sh" \
  "$repo/apps/imager/packaging/publish-winget.sh" \
  && ok 'imager release and promotion scripts parse' \
  || bad 'an imager release script does not parse'

bash "$repo/os/tests/forgejo_release_publication.sh" \
  || bad 'Forgejo release publication is not idempotent and immutable'

if command -v appstreamcli >/dev/null 2>&1; then
  appstreamcli validate --no-net \
    "$repo/apps/imager/packaging/linux/org.qos.Imager.metainfo.xml" \
    && ok 'Linux application metadata validates offline' \
    || bad 'Linux application metadata is invalid'
fi

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
  && [ "$(grep -Fc 'enable-openid-connect: true' "$workflow")" -eq 6 ] \
  && grep -Fq 'IMAGER_LINUX_CREDENTIAL_BROKER' "$workflow" \
  && grep -Fq 'IMAGER_WINDOWS_CREDENTIAL_BROKER' "$workflow" \
  && grep -Fq 'IMAGER_MACOS_CREDENTIAL_BROKER' "$workflow" \
  && grep -Fq 'IMAGER_HOMEBREW_CREDENTIAL_BROKER' "$workflow" \
  && grep -Fq 'IMAGER_WINGET_CREDENTIAL_BROKER' "$workflow" \
  && grep -Fq 'IMAGER_SITE_CREDENTIAL_BROKER' "$workflow" \
  && grep -Fq 'QOS_LINUX_SIGNER' "$repo/apps/imager/packaging/package-host.sh" \
  && grep -Fq 'QOS_NOTARY_KEYCHAIN_PROFILE' "$repo/apps/imager/packaging/package-host.sh" \
  && grep -Fq 'QOS_WINDOWS_SIGNER' "$repo/apps/imager/packaging/package-host.ps1" \
  && ok 'all credentialed jobs enable Forgejo OIDC with platform-specific brokers' \
  || bad 'short-lived credential path is not wired into every release job'

grep -Fq 'AppImage.asc' "$workflow" \
  && grep -Fq 'qos-installer-*.asc' "$workflow" \
  && grep -Fq 'qos-installer-catalog-v1.json' "$repo/apps/imager/packaging/verify-linux-release.sh" \
  && grep -Fq 'qos-release-key.asc' "$workflow" \
  && grep -Fq 'IMAGER_LINUX_SIGNING_FINGERPRINT' "$workflow" \
  && grep -Fq 'verify-linux-release.sh release-assets' "$workflow" \
  && grep -Fq 'verify-linux-release.sh" "$asset_root"' \
    "$repo/apps/imager/packaging/publish-forgejo-release.sh" \
  && ! grep -Fqi 'qos-imager-linux-*.flatpak' "$workflow" \
  && ok 'Linux signatures, release key, and protected fingerprint are a fail-closed publication gate' \
  || bad 'Linux release trust bundle is incomplete or bypassable'

for artifact in \
  qos-imager-windows-x86_64.zip \
  qos-imager-macos-x86_64.zip \
  qos-imager-macos-aarch64.zip; do
  grep -Fq "            $artifact" "$workflow" \
    && ok "$artifact is an explicit publication prerequisite" \
    || bad "$artifact is linked by the website but absent from the publication gate"
done
grep -Fq 'sha256sum --check --strict "$(basename "$path").sha256"' "$workflow" \
  && ok 'every required release sidecar is validated before publication' \
  || bad 'release digest sidecars are checked for existence but not content'

grep -Fq 'bash os/tests/imager_linux_release_trust.sh' \
    "$repo/.forgejo/workflows/check.yml" \
  && ok 'Linux release trust tamper test runs in required checks' \
  || bad 'Linux release trust tamper test is not wired into CI'

linux_launcher="$repo/apps/imager/packaging/linux/qos-imager"
mac_launcher="$repo/apps/imager/packaging/macos/qos-imager"
windows_launcher="$repo/apps/imager/packaging/windows/Start-qOS-Imager.ps1"
writer="$repo/apps/imager/src/write.rs"
if grep -Fq 'exec pkexec' "$linux_launcher" \
  || grep -Fq 'with administrator privileges' "$mac_launcher" \
  || grep -Fq -- '-Verb RunAs' "$windows_launcher"; then
  bad 'a native launcher still elevates the complete Imager GUI'
elif grep -Fq 'WriteBackend::PrivilegedHelper' "$writer" \
  && grep -Fq 'Command::new("pkexec")' "$writer" \
  && grep -Fq 'with administrator privileges' "$writer" \
  && grep -Fq -- '-Verb RunAs' "$writer" \
  && grep -Fq 'without_secrets' "$writer" \
  && grep -Fq 'write helper request changed after authorization' "$writer"; then
  ok 'native GUIs stay unprivileged and authenticate only a digest-bound secret-free writer'
else
  bad 'write-time least-privilege helper is incomplete for a native platform'
fi

for artifact in \
  'qos-imager-linux-*.AppImage' \
  'qos-imager-windows-x86_64.exe' \
  'qos-imager-setup-windows-x86_64.exe' \
  'qos-imager-macos-*.dmg'; do
  grep -Fq "$artifact" "$workflow" \
    && ok "$artifact is carried into the release" \
    || bad "$artifact is absent from the release workflow"
done
grep -Fq 'runner: runner-arm64' "$workflow" \
  && grep -Fq 'runner: macos-x64' "$workflow" \
  && grep -Fq 'runner: macos-arm64' "$workflow" \
  && grep -Fq 'runs-on: windows' "$workflow" \
  && ok 'Linux ARM64, Windows x64, and both macOS architectures have native lanes' \
  || bad 'the requested native release architecture matrix is incomplete'
grep -Fq 'Require the complete native release matrix' "$workflow" \
  && [ "$(grep -Fc 'if-no-files-found: error' "$workflow")" -ge 3 ] \
  && ok 'publication fails closed on any missing native artifact or digest' \
  || bad 'partial native artifact sets can reach publication'

grep -Fq 'Build the complete native Linux package set' "$workflow" \
  && grep -Fq 'Build the complete native Windows package set' "$workflow" \
  && grep -Fq 'Build the complete native macOS package set' "$workflow" \
  && [ "$(grep -Fc 'QOS_REQUIRE_NATIVE_PACKAGES: "1"' "$workflow")" -eq 3 ] \
  && grep -Fq 'Launch the packaged Linux UI offscreen' "$workflow" \
  && grep -Fq 'Install and launch the packaged Windows UI offscreen' "$workflow" \
  && grep -Fq 'Mount and launch the packaged macOS UI offscreen' "$workflow" \
  && grep -Fq 'system bin/"qos-imager-bin", "--shots"' \
    "$repo/apps/imager/packaging/homebrew/qos-imager.rb.in" \
  && ok 'every native package is required and executes its packaged UI before publication' \
  || bad 'native packaging can pass without building and launching an installable artifact'
grep -Fq 'qos-imager-linux-aarch64.tar.gz' \
    "$repo/apps/imager/packaging/homebrew/qos-imager.rb.in" \
  && grep -Fq 'qos-imager-linux-x86_64.tar.gz' \
    "$repo/apps/imager/packaging/homebrew/qos-imager.rb.in" \
  && grep -Fq 'qos-imager-macos-#{arch}.dmg' \
    "$repo/apps/imager/packaging/homebrew/qos-imager-cask.rb.in" \
  && ok 'Homebrew selects native Linux and macOS artifacts by architecture' \
  || bad 'Homebrew does not cover the requested Linux/macOS architectures'

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
  && grep -Fq "jq -r '.tag // empty'" "$repo/apps/imager/packaging/publish-site-release.sh" \
  && grep -Fq '.linuxSigningFingerprint = $fingerprint' "$repo/apps/imager/packaging/publish-site-release.sh" \
  && grep -Fq '.channels.homebrew = "available"' "$repo/apps/imager/packaging/publish-site-release.sh" \
  && grep -Fq '.channels.winget = $winget_state' "$repo/apps/imager/packaging/publish-site-release.sh" \
  && grep -Fq 'raw.githubusercontent.com/microsoft/winget-pkgs/master/manifests/q/qOS/Imager/${version}/qOS.Imager.yaml' "$repo/apps/imager/packaging/publish-site-release.sh" \
  && grep -Fq 'QOS_LINUX_SIGNING_FINGERPRINT: ${{ vars.IMAGER_LINUX_SIGNING_FINGERPRINT }}' "$workflow" \
  && ok 'landing-page manifest advances only through the gated release workflow' \
  || bad 'landing-page release publication is not wired'

grep -Fq 'qos-installer-medium-${{ matrix.arch }}' "$workflow" \
  && grep -Fq 'qos-installer-catalog-v1.json' "$workflow" \
  && grep -Fq 'image_sha256' "$repo/apps/imager/src/release.rs" \
  && grep -Fq 'archive_sha256' "$repo/apps/imager/src/release.rs" \
  && ok 'native live media is catalogued and verified before writing' \
  || bad 'end-user install medium publication or verification is missing'

grep -Fq 'key: github-assets-all-v1-' "$workflow" \
  && grep -Fq './os/dev/cache-github-assets.sh' "$workflow" \
  && grep -Fq 'podman login code.sandbox.example' "$workflow" \
  && grep -Fq './os/dev/install-cached-cosign.sh' "$workflow" \
  && ok 'installer media restores pinned inputs and can authenticate/verify frozen images' \
  || bad 'installer-media release lane lacks immutable inputs or private-image prerequisites'

grep -Fq 'key: imager-linux-tools-${{ matrix.arch }}-' "$workflow" \
  && grep -Fq 'linuxdeploy-1-alpha-20251107-1-${arch}.AppImage' "$workflow" \
  && grep -Fq 'appimagetool-1.9.1-${arch}.AppImage' "$workflow" \
  && grep -Fq 'type2-runtime-20251108-${arch}' "$workflow" \
  && grep -Fq -- '--runtime-file "$QOS_APPIMAGE_RUNTIME"' \
    "$repo/apps/imager/packaging/package-host.sh" \
  && grep -Fq 'implicit runtime downloads are forbidden' \
    "$repo/apps/imager/packaging/package-host.sh" \
  && ok 'AppImage tooling and runtime are checksum-pinned, cached, and offline at packaging time' \
  || bad 'AppImage packaging still depends on mutable or repeated GitHub downloads'

grep -Fq "group: release-imager-\${{ startsWith(github.ref, 'refs/tags/imager-v') && 'promotion' || github.ref }}" "$workflow" \
  && grep -Fq "cancel-in-progress: \${{ !startsWith(github.ref, 'refs/tags/imager-v') }}" "$workflow" \
  && grep -Fq 'refusing to replace newer Homebrew release' "$repo/apps/imager/packaging/publish-homebrew-tap.sh" \
  && grep -Fq 'refusing to replace newer landing-page release' "$repo/apps/imager/packaging/publish-site-release.sh" \
  && ok 'tag promotion is serialized while stale branch validation is superseded' \
  || bad 'release promotion can regress or stale validation can block the current commit'

exit "$fail"
