#!/usr/bin/env bash
# Cheap release-boundary checks for the cross-platform Tilo Imager pipeline.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$repo/.forgejo/workflows/release-imager.yml"
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

bash -n "$repo/apps/imager/packaging/package-host.sh" \
  "$repo/apps/imager/packaging/package-flatpak.sh" \
  "$repo/apps/imager/packaging/verify-appimage-runtime.sh" \
  "$repo/apps/imager/packaging/linux/tilo-imager" \
  "$repo/apps/imager/packaging/macos/tilo-imager" \
  "$repo/apps/imager/packaging/package-install-medium.sh" \
  "$repo/apps/imager/packaging/build-release-catalog.sh" \
  "$repo/apps/imager/packaging/verify-linux-release.sh" \
  "$repo/apps/imager/packaging/sign-linux-release-file.sh" \
  "$repo/apps/imager/packaging/assert-github-installation-token.sh" \
  "$repo/apps/imager/packaging/revoke-github-installation-token.sh" \
  "$repo/apps/imager/packaging/publish-github-release.sh" \
  "$repo/apps/imager/packaging/publish-forgejo-release.sh" \
  "$repo/apps/imager/packaging/publish-internal-bundle.sh" \
  "$repo/apps/imager/packaging/publish-target-policy.sh" \
  "$repo/os/dev/export-internal-bundle.sh" \
  "$repo/os/tests/internal_bundle_export.sh" \
  "$repo/os/tests/internal_bundle_catalog.sh" \
  "$repo/apps/imager/packaging/publish-homebrew-tap.sh" \
  "$repo/apps/imager/packaging/publish-site-release.sh" \
  "$repo/apps/imager/packaging/monitor-winget-acceptance.sh" \
  "$repo/apps/imager/packaging/publish-winget.sh" \
  "$repo/os/dev/use-published-image.sh" \
  "$repo/os/dev/test-installer-cycle.sh" \
  && ok 'imager release and promotion scripts parse' \
  || bad 'an imager release script does not parse'

bash "$repo/os/tests/forgejo_release_publication.sh" \
  || bad 'GitHub release publication is not target-scoped, idempotent, and immutable'
bash "$repo/os/tests/public_release_origin.sh" \
  || bad 'installer catalog is not bound to the configured public release origin'
bash "$repo/os/tests/internal_bundle_catalog.sh" \
  || bad 'internal installer catalog entries are not bound to exact published manifests'
target_job="$(sed -n '/^  target_policy:/,/^  medium:/p' "$workflow")"
if grep -Fq "if: startsWith(github.ref, 'refs/tags/imager-v')" <<<"$target_job" \
   && grep -Fq './os/dev/use-published-image.sh "$TILO_PUBLISHED_OS_IMAGE"' <<<"$target_job" \
   && grep -Fq 'export-target-policy.py export' <<<"$target_job" \
   && grep -Fq -- '--expected-source "$TILO_EXPECTED_OS_SOURCE_REVISION"' <<<"$target_job" \
   && grep -Fq 'publish-target-policy.sh dist/tilo-target-policy' <<<"$target_job" \
   && grep -Fq 'name: tilo-target-policy-${{ matrix.arch }}' <<<"$target_job" \
   && grep -Fq 'target-policy-catalog-${{ matrix.arch }}.json' <<<"$target_job" \
   && grep -Fq 'needs: [plan, medium, target_policy]' "$workflow" \
   && grep -Fq 'export-target-policy.py" verify' "$repo/apps/imager/packaging/build-release-catalog.sh" \
   && grep -Fq 'target-policy-catalog-*.json|tilo-target-policy-*.tar' \
      "$repo/apps/imager/packaging/publish-github-release.sh" \
      "$repo/apps/imager/packaging/publish-forgejo-release.sh"; then
  ok 'tag releases export and publish one signed target policy from the exact OS image'
else
  bad 'target policy producer, publication or catalog wiring is missing'
fi
[ "$(grep -Fc "startsWith(github.ref, 'refs/tags/imager-v') && vars.IMAGER_INTERNAL_BUNDLE == 'publish'" "$workflow")" -eq 2 ] \
  && grep -Fq 'IMAGER_INTERNAL_BUNDLE_CREDENTIAL_BROKER' "$workflow" \
  && grep -Fq 'sudo -n bash os/tests/internal_bundle_export.sh' "$workflow" \
  && grep -Fq 'acquire-release-token.sh" internal-bundle' "$repo/apps/imager/packaging/publish-internal-bundle.sh" \
  && ok 'internal installer bundles publish only from opted-in, tested tag builds' \
  || bad 'internal installer bundle publication is not gated on tags, opt-in and the export test'
bash "$repo/os/tests/winget_publication_idempotency.sh" \
  || bad 'WinGet publication retry does not recognize exact accepted manifests'
bash "$repo/os/tests/winget_acceptance_monitor.sh" \
  || bad 'WinGet acceptance monitor is not fail-closed and version-bound'

if command -v appstreamcli >/dev/null 2>&1; then
  appstreamcli validate --no-net \
    "$repo/apps/imager/packaging/linux/org.tilo.Imager.metainfo.xml" \
    && ok 'Linux application metadata validates offline' \
    || bad 'Linux application metadata is invalid'
fi

grep -Fq '"$appdir/usr/share/metainfo/org.tilo.Imager.appdata.xml"' \
  "$repo/apps/imager/packaging/package-host.sh" \
  && ok 'AppImage metadata uses the desktop-id filename expected by linuxdeploy' \
  || bad 'AppImage packaging will omit discoverable application metadata'

appimage_verify="$repo/apps/imager/packaging/verify-appimage-runtime.sh"
flatpak_package="$repo/apps/imager/packaging/package-flatpak.sh"
if grep -Fq 'APPIMAGE_EXTRACT_AND_RUN=1 "$appimage"' "$appimage_verify" \
   && grep -Fq '"$appimage" \' "$appimage_verify" \
   && grep -Fq "awk '\$2 == \"fuse\"" "$appimage_verify" \
   && grep -Fq 'refusing extract-only release verification' "$appimage_verify" \
   && grep -Fq 'appimage-runtime-mode.txt' "$appimage_verify" \
   && grep -Fq 'GITHUB_STEP_SUMMARY' "$appimage_verify"; then
  ok 'AppImage verifies extraction and its normal FUSE runtime with a bounded fallback'
else
  bad 'AppImage can publish without proving its normal runtime or recording a genuine no-FUSE fallback'
fi

if grep -Fq 'export FLATPAK_USER_DIR="$isolated_install"' "$flatpak_package" \
   && grep -Fq -- '--disable-rofiles-fuse' "$flatpak_package" \
   && grep -Fq -- '--state-dir="$work/state"' "$flatpak_package" \
   && grep -Fq 'flatpak install --user --noninteractive --no-deps --bundle "$artifact"' "$flatpak_package" \
   && grep -Fq 'flatpak run --user org.tilo.Imager --tilo-package-smoke' "$flatpak_package" \
   && grep -Fq 'flatpak run --user org.tilo.Imager --tilo-flatpak-probe-smoke' "$flatpak_package" \
   && grep -Fq 'flatpak run --user --filesystem="$smoke" org.tilo.Imager' "$flatpak_package" \
   && grep -Fq 'test -s "$smoke/flatpak-installed.png"' "$flatpak_package" \
   && ! grep -Fq 'flatpak-builder "$flatpak_scope" --run' "$flatpak_package"; then
  ok 'the emitted Flatpak bundle is installed and launched from an isolated user installation'
else
  bad 'Flatpak verification bypasses the emitted bundle installation path'
fi

grep -Fq 'TILO_REQUIRE_SIGNING=1' "$workflow" \
  && grep -Fq 'TILO_REQUIRE_NATIVE_PACKAGES=1' "$workflow" \
  && grep -Fq 'TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY: ${{ vars.TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY }}' "$workflow" \
  && ok 'tag releases fail closed without signed native packages' \
  || bad 'tag releases can publish unsigned or substitute packages'

if grep -Fq 'TILO_INSTALLER_RELEASE_TAG="rehearsal-${GITHUB_SHA}"' "$workflow" \
   && grep -Fq 'TILO_INSTALLER_CHANNEL=rehearsal' "$workflow" \
   && grep -Fq 'rehearsal media require an explicit source-bound tag and version' \
     "$repo/apps/imager/packaging/package-install-medium.sh"; then
  ok 'manual rehearsals package source-bound non-stable installer media'
else
  bad 'manual release rehearsal cannot package its tested installer medium honestly'
fi

if grep -Eq 'uses: [^#[:space:]]+@(v[0-9]|main|master)([[:space:]#]|$)' "$workflow"; then
  bad 'release workflow executes a mutable third-party action reference'
else
  ok 'release workflow pins third-party actions by commit'
fi

grep -Fq 'id-token: write' "$workflow" \
  && [ "$(grep -Fc 'enable-openid-connect: true' "$workflow")" -eq 9 ] \
  && grep -Fq 'IMAGER_LINUX_CREDENTIAL_BROKER' "$workflow" \
  && grep -Fq 'IMAGER_WINDOWS_CREDENTIAL_BROKER' "$workflow" \
  && grep -Fq 'IMAGER_MACOS_CREDENTIAL_BROKER' "$workflow" \
  && grep -Fq 'IMAGER_RELEASE_CREDENTIAL_BROKER' "$workflow" \
  && grep -Fq 'IMAGER_HOMEBREW_CREDENTIAL_BROKER' "$workflow" \
  && grep -Fq 'IMAGER_WINGET_CREDENTIAL_BROKER' "$workflow" \
  && grep -Fq 'IMAGER_SITE_CREDENTIAL_BROKER' "$workflow" \
  && grep -Fq 'TILO_PCR_PREDICTION_SIGNER' "$workflow" \
  && grep -Fq 'TILO_LINUX_SIGNER' "$repo/apps/imager/packaging/package-host.sh" \
  && grep -Fq 'TILO_NOTARY_KEYCHAIN_PROFILE' "$repo/apps/imager/packaging/package-host.sh" \
  && grep -Fq 'TILO_WINDOWS_SIGNER' "$repo/apps/imager/packaging/package-host.ps1" \
  && ok 'all credentialed jobs enable Forgejo OIDC with platform-specific brokers' \
  || bad 'short-lived credential path is not wired into every release job'

grep -Fq 'acquire-release-credentials.sh' "$workflow" \
  && grep -Fq 'Acquire-ReleaseCredentials.ps1' "$workflow" \
  && [ "$(grep -Fc 'acquire-release-token.sh' "$workflow")" -eq 4 ] \
  && grep -Fq 'credential output must not preexist' \
    "$repo/apps/imager/packaging/acquire-release-token.sh" \
  && grep -Fq 'lease file has an extended' \
    "$repo/apps/imager/packaging/acquire-release-credentials.sh" \
  && grep -Fq 'IMAGER_WINDOWS_PUBLISHER' "$workflow" \
  && grep -Fq 'TILO_WINDOWS_EXPECTED_PUBLISHER' \
    "$repo/apps/imager/packaging/package-host.ps1" \
  && grep -Fq 'IMAGER_MAC_TEAM_ID' "$workflow" \
  && grep -Fq 'IMAGER_MAC_CERT_SHA256' "$workflow" \
  && grep -Fq 'TILO_MAC_EXPECTED_CERT_SHA256' \
    "$repo/apps/imager/packaging/package-host.sh" \
  && grep -Fq '$Current = if ($Current -is [IO.FileInfo])' \
    "$repo/apps/imager/packaging/Acquire-ReleaseCredentials.ps1" \
  && grep -Fq 'and -not $TrustedPrincipal' \
    "$repo/apps/imager/packaging/Acquire-ReleaseCredentials.ps1" \
  && grep -Fq "'S-1-5-32-544'" \
    "$repo/apps/imager/packaging/Acquire-ReleaseCredentials.ps1" \
  && ! grep -Fq '(?i)(SYSTEM|Administrators' \
    "$repo/apps/imager/packaging/Acquire-ReleaseCredentials.ps1" \
  && [ "$(grep -Fc 'if: always()' "$workflow")" -ge 2 ] \
  && ok 'release brokers, signer paths, publisher identities, and cleanup fail closed' \
  || bad 'release credential outputs or signing identities are insufficiently constrained'

publisher="$repo/apps/imager/packaging/publish-github-release.sh"
publish_job="$(sed -n '/^  publish:/,/^  homebrew:/p' "$workflow")"
if grep -Fq "release_repository=\"\${TILO_RELEASE_REPOSITORY:-tilo-os/releases}\"" "$publisher" \
   && grep -Fq 'release repository must be public' "$publisher" \
   && grep -Fq 'assert-github-installation-token.sh' "$publisher" \
   && grep -Fq 'release "$CREDENTIAL_BROKER" "$token_file"' "$workflow" \
   && grep -Fq 'IMAGER_PUBLIC_RELEASE_REPOSITORY' "$workflow" \
   && grep -Fq 'persist-credentials: false' <<<"$publish_job" \
   && ! grep -Fq '${{ github.token }}' <<<"$publish_job" \
   && grep -Fq '.releaseRoot = $release_root' "$repo/apps/imager/packaging/publish-site-release.sh" \
   && grep -Fq 'landing page does not expose required release asset' "$repo/apps/imager/packaging/publish-site-release.sh" \
   && ! grep -Fq 'tilo/tilo/releases/download' \
      "$repo/apps/imager/packaging/package-install-medium.sh" \
      "$repo/apps/imager/packaging/build-release-catalog.sh" \
      "$repo/apps/imager/packaging/publish-homebrew-tap.sh" \
      "$repo/apps/imager/packaging/package-host.ps1" "$workflow"; then
  ok 'public binaries use a dedicated public repository and a target-only OIDC lease'
else
  bad 'binary publication can target the private source repository or reuse its job token'
fi

mac_broker="$repo/apps/imager/packaging/acquire-release-credentials.sh"
token_broker="$repo/apps/imager/packaging/acquire-release-token.sh"
runner_launcher="$repo/tools/ci/runners/imager-ephemeral-linux.sh"
homebrew_publish="$repo/apps/imager/packaging/publish-homebrew-tap.sh"
site_publish="$repo/apps/imager/packaging/publish-site-release.sh"
if ! grep -Fq 'declare -A' "$mac_broker" \
   && grep -Fq '"$broker" "$platform" "$lease" >/dev/null' "$mac_broker" \
   && grep -Fq '$null = & $Trusted windows $Lease' \
     "$repo/apps/imager/packaging/Acquire-ReleaseCredentials.ps1" \
   && grep -Fq '"$broker" "$kind" "$output" >/dev/null' \
     "$repo/apps/imager/packaging/acquire-release-token.sh" \
   && ! grep -Fq 'realpath -e' "$mac_broker" \
   && ! grep -Fq 'realpath -e' "$token_broker" \
   && ! grep -Fq 'realpath -e' "$runner_launcher" \
   && ! grep -Fq '${EXPECTED_CERT_SHA256^^}' "$workflow" \
   && ! grep -Fq 'sort -V' "$homebrew_publish" \
   && [ "$(grep -Fc 'Out-File -FilePath $env:GITHUB_ENV' "$workflow")" -eq \
        "$(grep -Fc 'Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append' "$workflow")" ]; then
  ok 'native release credential and promotion paths support stock macOS Bash and Windows PowerShell'
else
  bad 'native release automation exposes broker stdout, depends on Bash 4/GNU sort, or writes a non-UTF-8 Windows environment file'
fi

source <(sed -n '/^version_is_newer()/,/^}/p' "$site_publish")
if version_is_newer 1.10.0 1.9.999 \
   && version_is_newer 10000000000.0.0 9999999999.999.999 \
   && ! version_is_newer 1.0.0 1.0.0 \
   && ! version_is_newer 1.2.3 2.0.0 \
   && ! grep -Fq 'sort -V' "$site_publish" \
   && grep -Fq 'existing landing-page release has invalid tag' "$site_publish"; then
  ok 'landing-page promotion validates and compares release versions without numeric truncation'
else
  bad 'landing-page promotion can overwrite malformed or newer release state'
fi

# Load only the side-effect-free comparator, then exercise boundaries which a
# fixed-width numeric key or awk floating-point comparison gets wrong.
source <(sed -n '/^version_is_newer()/,/^}/p' "$homebrew_publish")
if version_is_newer 1.10.0 1.9.999 \
   && version_is_newer 10000000000.0.0 9999999999.999.999 \
   && ! version_is_newer 1.0.0 1.0.0 \
   && ! version_is_newer 1.2.3 2.0.0 \
   && grep -Fq 'existing Homebrew formula has no parseable version' "$homebrew_publish"; then
  ok 'Homebrew promotion compares semantic version components without numeric truncation'
else
  bad 'Homebrew promotion can replace a newer release or reject a valid upgrade'
fi

if python3 "$repo/apps/imager/packaging/test_release_catalog.py" \
    && [ "$(grep -c -- '--copy-to apps/imager/release-catalog.json' "$workflow")" -eq 3 ] \
    && grep -Fq 'sign-installer-release.sh' "$workflow" \
    && grep -Fq 'media_list="$(python3' "$repo/apps/imager/packaging/verify-linux-release.sh"; then
  ok 'all native consumers select one supported catalog and release trust covers its media inventory'
else
  bad 'versioned installer catalog selection or media trust coverage is incomplete'
fi

grep -Fq 'AppImage.asc' "$workflow" \
  && grep -Fq 'tilo-installer-*.asc' "$workflow" \
  && grep -Fq 'release-catalog.py' "$repo/apps/imager/packaging/verify-linux-release.sh" \
  && grep -Fq 'tilo-release-key.asc' "$workflow" \
  && grep -Fq 'IMAGER_LINUX_SIGNING_FINGERPRINT' "$workflow" \
  && grep -Fq 'verify-linux-release.sh release-assets' "$workflow" \
  && grep -Fq 'verify-linux-release.sh" "$asset_root"' \
    "$repo/apps/imager/packaging/publish-github-release.sh" \
  && grep -Fq 'tilo-imager-linux-*.flatpak.asc' "$workflow" \
  && ok 'Linux signatures, release key, and protected fingerprint are a fail-closed publication gate' \
  || bad 'Linux release trust bundle is incomplete or bypassable'

for artifact in \
  tilo-imager-windows-x86_64.zip \
  tilo-imager-windows-aarch64.zip; do
  grep -Fq "            $artifact" "$workflow" \
    && ok "$artifact is an explicit publication prerequisite" \
    || bad "$artifact is linked by the website but absent from the publication gate"
done
if python3 -m unittest discover -s "$repo/tools/tests" -p test_imager_enabled_platforms.py; then
  ok 'enabled Linux/Windows/macOS releases retain a strict complete artifact matrix'
else
  bad 'enabled native platform matrix or publication gate failed'
fi
grep -Fq 'sha256sum --check --strict "$(basename "$path").sha256"' "$workflow" \
  && ok 'every required release sidecar is validated before publication' \
  || bad 'release digest sidecars are checked for existence but not content'

grep -Fxq 'os/tests/imager_linux_release_trust.sh' \
    "$repo/tools/security-boundary-invariants.txt" \
  && grep -Fqx 'run = "bash tools/run-security-boundary-invariants.sh"' "$repo/t.toml" \
  && ok 'Linux release trust tamper test runs in required checks' \
  || bad 'Linux release trust tamper test is not wired into CI'

linux_launcher="$repo/apps/imager/packaging/linux/tilo-imager"
mac_launcher="$repo/apps/imager/packaging/macos/tilo-imager"
windows_launcher="$repo/apps/imager/packaging/windows/Start-Tilo-Imager.ps1"
writer="$repo/apps/imager/src/write.rs"
# The writer's privileged half (helper, UDisks2 descriptor) lives in the backend.
backend="$repo/crates/platform/imager-backend"
write_helper="$backend/src/write/helper.rs"
if grep -Fq 'host-media-writer = ["installation", "imager-backend/host-media-writer"]' "$repo/apps/imager/Cargo.toml" \
  && grep -Fq 'host-media-writer = []' "$backend/Cargo.toml" \
  && grep -Fq 'cfg!(feature = "host-media-writer")' "$writer" \
  && [ "$(grep -Fc 'cargo test -p imager --lib --locked --features host-media-writer' "$workflow")" -eq 3 ] \
  && grep -Fq -- '--features host-media-writer' \
    "$repo/apps/imager/packaging/package-host.sh" \
  && grep -Fq -- '--features host-media-writer' \
    "$repo/apps/imager/packaging/package-host.ps1"; then
  ok 'every host package compiles in the real short-lived media writer'
else
  bad 'a host package can ship with its media writer disabled'
fi
flatpak_manifest="$repo/apps/imager/packaging/flatpak/org.tilo.Imager.yml"
flatpak_package="$repo/apps/imager/packaging/package-flatpak.sh"
if grep -Fq -- '--system-talk-name=org.freedesktop.UDisks2' "$flatpak_manifest" \
  && ! grep -Eq -- '--(device=all|filesystem=host|talk-name=org.freedesktop.Flatpak)' "$flatpak_manifest" \
  && grep -Fq 'flatpak-media-writer = ["installation", "imager-backend/flatpak-media-writer"]' "$repo/apps/imager/Cargo.toml" \
  && grep -Fq 'flatpak-media-writer = ["block-devices/udisks", "dep:zbus"]' "$backend/Cargo.toml" \
  && grep -Fq 'O_EXCL | libc::O_SYNC | libc::O_CLOEXEC' \
    "$backend/src/write/udisks.rs" \
  && grep -Fq 'cargo test -p imager --lib --locked --features flatpak-media-writer' "$workflow" \
  && grep -Fq 'apps/imager/packaging/package-flatpak.sh' "$workflow" \
  && grep -Fq -- '--tilo-flatpak-probe-smoke' "$flatpak_package" \
  && grep -Fq -- '--disable-download' "$flatpak_package"; then
  ok 'Flatpak uses only a polkit-mediated exclusive UDisks2 descriptor and is release-gated'
else
  bad 'Flatpak raw-media authority or release packaging is too broad/incomplete'
fi
if grep -Fq 'exec pkexec' "$linux_launcher" \
  || grep -Fq 'with administrator privileges' "$mac_launcher" \
  || grep -Fq -- '-Verb RunAs' "$windows_launcher"; then
  bad 'a native launcher still elevates the complete Imager GUI'
elif grep -Fq 'WriteBackend::PrivilegedHelper' "$writer" \
  && grep -Fq 'without_secrets' "$writer" \
  && grep -Fq 'Command::new("pkexec")' "$write_helper" \
  && grep -Fq 'with administrator privileges' "$write_helper" \
  && grep -Fq -- '-Verb RunAs' "$write_helper" \
  && grep -Fq 'write helper request changed after authorization' "$write_helper"; then
  ok 'native GUIs stay unprivileged and authenticate only a digest-bound secret-free writer'
else
  bad 'write-time least-privilege helper is incomplete for a native platform'
fi

for artifact in \
  'tilo-imager-linux-*.AppImage' \
  'tilo-imager-linux-*.flatpak' \
  'tilo-imager-windows-x86_64.exe' \
  'tilo-imager-setup-windows-x86_64.exe' \
  'tilo-imager-windows-aarch64.exe' \
  'tilo-imager-setup-windows-aarch64.exe' \
  'tilo-imager-macos-*.dmg'; do
  grep -Fq "$artifact" "$workflow" \
    && ok "$artifact is carried into the release" \
    || bad "$artifact is absent from the release workflow"
done
grep -Fq 'runner: imager-linux-aarch64' "$workflow" \
  && grep -Fq 'runner: imager-macos-x86_64' "$workflow" \
  && grep -Fq 'runner: imager-macos-aarch64' "$workflow" \
  && grep -Fq 'runner: imager-windows-x86_64' "$workflow" \
  && grep -Fq 'runner: imager-windows-aarch64' "$workflow" \
  && ok 'Linux, Windows, and macOS have native x86-64 and ARM64 lanes' \
  || bad 'the requested native release architecture matrix is incomplete'
grep -Fq 'Require the complete native release matrix' "$workflow" \
  && [ "$(grep -Fc 'if-no-files-found: error' "$workflow")" -ge 3 ] \
  && ok 'publication fails closed on any missing native artifact or digest' \
  || bad 'partial native artifact sets can reach publication'

grep -Fq 'Build the complete native Linux package set' "$workflow" \
  && grep -Fq 'Build the complete native Windows package set' "$workflow" \
  && grep -Fq 'Build the complete native macOS package set' "$workflow" \
  && [ "$(grep -Fc 'TILO_REQUIRE_NATIVE_PACKAGES: "1"' "$workflow")" -eq 3 ] \
  && grep -Fq 'Launch the packaged Linux UI offscreen' "$workflow" \
  && grep -Fq 'Install and launch the packaged Windows UI offscreen' "$workflow" \
  && grep -Fq 'Mount and launch the packaged macOS UI offscreen' "$workflow" \
  && grep -Fq 'xcrun stapler validate "$app"' \
    "$repo/apps/imager/packaging/package-host.sh" \
  && grep -Fq 'xcrun stapler validate "$out/$artifact_base.dmg"' \
    "$repo/apps/imager/packaging/package-host.sh" \
  && grep -Fq 'system bin/"tilo-imager-bin", "--shots"' \
    "$repo/apps/imager/packaging/homebrew/tilo-imager.rb.in" \
  && ok 'every native package is required and executes its packaged UI before publication' \
  || bad 'native packaging can pass without building and launching an installable artifact'
grep -Fq 'tilo-imager-linux-aarch64.tar.gz' \
    "$repo/apps/imager/packaging/homebrew/tilo-imager.rb.in" \
  && grep -Fq 'tilo-imager-linux-x86_64.tar.gz' \
    "$repo/apps/imager/packaging/homebrew/tilo-imager.rb.in" \
  && grep -Fq 'tilo-imager-macos-#{arch}.dmg' \
    "$repo/apps/imager/packaging/homebrew/tilo-imager-cask.rb.in" \
  && ok 'Homebrew selects native Linux and macOS artifacts by architecture' \
  || bad 'Homebrew does not cover the requested Linux/macOS architectures'

grep -Fq 'tilo-os/homebrew-tilo' "$repo/apps/imager/packaging/publish-homebrew-tap.sh" \
  && [ "$(grep -Fc 'brew tap tilo/tilo https://github.com/tilo-os/homebrew-tilo.git' "$workflow")" -eq 2 ] \
  && ! grep -Fq 'code.tilo.so/tilo/homebrew-tilo.git' "$workflow" \
  && grep -Fq 'microsoft/winget-pkgs/pulls' "$repo/apps/imager/packaging/publish-winget.sh" \
  && grep -Fq -- '--retry 6 --retry-all-errors --retry-max-time 180' "$repo/apps/imager/packaging/publish-winget.sh" \
  && grep -Fq 'state=open&base=master&head=' "$repo/apps/imager/packaging/publish-winget.sh" \
  && grep -Fq 'homebrew "$CREDENTIAL_BROKER" "$token_file"' "$workflow" \
  && grep -Fq 'winget "$CREDENTIAL_BROKER" "$token_file"' "$workflow" \
  && grep -Fq 'IMAGER_HOMEBREW_CREDENTIAL_BROKER' "$workflow" \
  && grep -Fq 'IMAGER_WINGET_CREDENTIAL_BROKER' "$workflow" \
  && ! grep -Fq 'ALLOW_STATIC_BOOTSTRAP' "$workflow" \
  && ok 'first-party Homebrew and public WinGet promotion are automated' \
  || bad 'a package-index promotion path is missing'

grep -Fq 'site-downloads:' "$workflow" \
  && grep -Fq 'test:release' "$repo/apps/imager/packaging/publish-site-release.sh" \
  && grep -Fq "jq -r '.tag // empty'" "$repo/apps/imager/packaging/publish-site-release.sh" \
  && grep -Fq '.linuxSigningFingerprint = $fingerprint' "$repo/apps/imager/packaging/publish-site-release.sh" \
  && grep -Fq '.channels.homebrew = "available"' "$repo/apps/imager/packaging/publish-site-release.sh" \
  && grep -Fq '.channels.winget = $winget_state' "$repo/apps/imager/packaging/publish-site-release.sh" \
  && grep -Fq 'linuxX64Flatpak linuxArm64Flatpak linuxX64AppImage linuxArm64AppImage' \
    "$repo/apps/imager/packaging/publish-site-release.sh" \
  && grep -Fq 'landing page does not expose required release asset' \
    "$repo/apps/imager/packaging/publish-site-release.sh" \
  && grep -Fq "'brew install --cask tilo-imager' 'brew install tilo-imager'" \
    "$repo/apps/imager/packaging/publish-site-release.sh" \
  && grep -Fq 'TILO_RELEASE_REPOSITORY="$release_repository"' \
    "$repo/apps/imager/packaging/monitor-winget-acceptance.sh" \
  && grep -Fq 'raw.githubusercontent.com/microsoft/winget-pkgs/master/manifests/q/Tilo/Imager/${version}/Tilo.Imager.yaml' "$repo/apps/imager/packaging/publish-site-release.sh" \
  && grep -Fq 'TILO_LINUX_SIGNING_FINGERPRINT: ${{ vars.IMAGER_LINUX_SIGNING_FINGERPRINT }}' "$workflow" \
  && ok 'landing-page manifest advances only through the gated release workflow' \
  || bad 'landing-page release publication is not wired'

grep -Fq 'tilo-installer-medium-${{ matrix.arch }}' "$workflow" \
  && grep -Fq 'tilo-installer-catalog-v1.json' "$workflow" \
  && grep -Fq 'image_sha256' "$repo/apps/imager/src/release.rs" \
  && grep -Fq 'archive_sha256' "$repo/apps/imager/src/release.rs" \
  && grep -Fq 'expanded_sha256=' "$repo/apps/imager/packaging/package-install-medium.sh" \
  && grep -Fq 'expanded installer bytes do not match' "$repo/apps/imager/packaging/package-install-medium.sh" \
  && ok 'native live media is catalogued and verified before writing' \
  || bad 'end-user install medium publication or verification is missing'

grep -Fq 'uses: ./.forgejo/actions/cache-github-assets' "$workflow" \
  && grep -Fqx '          key: github-assets-all-v1' "$workflow" \
  && grep -Fq 'run: ./os/dev/cache-github-assets.sh $ASSETS' \
    "$repo/.forgejo/actions/cache-github-assets/action.yml" \
  && grep -Fq 'podman login \' "$workflow" \
  && grep -Fq -- '--authfile "$REGISTRY_AUTH_FILE" code.tilo.so' "$workflow" \
  && grep -Fq './os/dev/install-cached-cosign.sh' "$workflow" \
  && ok 'installer media restores pinned inputs and can authenticate/verify frozen images' \
  || bad 'installer-media release lane lacks immutable inputs or private-image prerequisites'

published_image="$repo/os/dev/use-published-image.sh"
if grep -Fq 'TILO_INSTALLER_OS_IMAGE' "$workflow" \
   && grep -Fq 'published_os_image' "$workflow" \
   && grep -Fq './os/dev/use-published-image.sh "$TILO_PUBLISHED_OS_IMAGE"' "$workflow" \
   && ! grep -Fq './os/dev/build-image.sh' "$workflow" \
   && grep -Fq 'registry.tilo.so/tilo/tilo@sha256:' "$published_image" \
   && grep -Fq 'cosign verify --key "$verify_key" "$image"' "$published_image" \
   && grep -Fq 'cosign verify-attestation --key "$verify_key"' "$published_image" \
   && grep -Fq 'externalParameters.generation' "$published_image" \
   && grep -Fq 'cp -- "$REGISTRY_AUTH_FILE" "$auth_dir/config.json"' "$published_image" \
   && grep -Fq 'export DOCKER_CONFIG="$auth_dir"' "$published_image" \
   && grep -Fq 'trap cleanup_auth EXIT' "$published_image" \
   && grep -Fq 'podman manifest inspect "${manifest_args[@]}" "$image"' "$published_image" \
   && grep -Fq 'externalParameters.children | sort_by(.architecture)' "$published_image" \
   && grep -Fq 'generation_image="registry.tilo.so/tilo/tilo:gen$generation"' "$published_image" \
   && grep -Fq 'generation_digest' "$published_image" \
   && grep -Fq 'published generation tag gen$generation binds' "$published_image" \
   && grep -Fq "TILO_EXPECTED_OS_SOURCE_REVISION: \${{ github.event.inputs.published_os_source_revision || (startsWith(github.ref, 'refs/tags/imager-v') && github.sha) || '' }}" "$workflow" \
   && grep -Fq 'rehearsal overlays $GITHUB_SHA onto verified published generation' "$workflow" \
   && grep -Fq 'resolvedDependencies[]?; .uri == $source' "$published_image" \
   && grep -Fq 'podman tag "$image" "$TILO_IMAGE"' "$published_image"; then
  ok 'installer media consume one verified, digest-pinned published OS generation'
else
  bad 'installer media can allocate an unpublished generation or consume an unverified OS source'
fi

if grep -Fq 'published_os_source_revision:' "$workflow" \
   && grep -Fq 'PUBLISHED_OS_SOURCE_REVISION:-} =~ ^[0-9a-f]{40}$' "$workflow" \
   && grep -Fq 'git merge-base --is-ancestor "$PUBLISHED_OS_SOURCE_REVISION" "$GITHUB_SHA"' "$workflow" \
   && grep -Fq 'ref: ${{ github.sha }}' "$workflow" \
   && grep -Fq 'git merge-base --is-ancestor "$TILO_EXPECTED_OS_SOURCE_REVISION" HEAD' "$workflow" \
   && grep -Fq 'verified CI $target_profile target $target_image from source $TILO_EXPECTED_OS_SOURCE_REVISION generation ' "$workflow" \
   && grep -Fq -- '--arg build_revision "$(git rev-parse HEAD)"' "$workflow" \
   && grep -Fq 'signed $profile target source differs from pinned OS source' \
      "$repo/os/dev/prepare-ci-installer-target.sh"; then
  ok 'offline media use descendant main installer source and pin the signed OS target revision'
else
  bad 'offline medium does not bind its installer and signed OS source revisions'
fi

if grep -Fq "TILO_PCR_PREDICTION_SIGNER: \${{ startsWith(github.ref, 'refs/tags/imager-v')" "$workflow" \
   && grep -Fq "TILO_EFI_SIGNING_CERT_SHA256: \${{ startsWith(github.ref, 'refs/tags/imager-v')" "$workflow"; then
  ok 'production PCR signing interfaces are withheld from manual imager rehearsals'
else
  bad 'manual imager rehearsals can receive a production PCR signing interface'
fi

cycle="$repo/os/dev/test-installer-cycle.sh"
release_vars="$repo/os/dev/prepare-release-secureboot-vars.sh"
boot_verify="$repo/scripts/perf/qemu-install-boot-verify.sh"
image_tools_line="$(grep -nF 'name: Install native image-build and full-cycle VM dependencies' "$workflow" | cut -d: -f1 || true)"
image_auth_line="$(grep -nF 'name: Authenticate the native image builder' "$workflow" | cut -d: -f1 || true)"
if grep -Fq './os/dev/test-installer-cycle.sh' "$workflow" \
  && grep -Fq 'needs: plan' "$workflow" \
  && [ -n "$image_tools_line" ] && [ -n "$image_auth_line" ] \
  && [ "$image_tools_line" -lt "$image_auth_line" ] \
  && grep -Fq 'ntfs-3g podman' "$workflow" \
  && grep -Fq 'mtype ntfscp podman qemu-img' "$workflow" \
  && grep -Fq 'cycle_graphics=(--graphics software)' "$workflow" \
  && grep -Fq 'export TILO_ACCEL=tcg' "$workflow" \
  && grep -Fq 'if [[ "$GITHUB_REF" == refs/tags/imager-v* ]]; then' "$workflow" \
  && grep -Fq 'test -c /dev/kvm && test -r /dev/kvm && test -w /dev/kvm' "$workflow" \
  && grep -Fq -- '--prepared-alongside' "$workflow" \
  && grep -Fq -- '--guided-alongside' "$workflow" \
  && grep -Fq 'dist/installer-guided-alongside-cycle' "$workflow" \
  && grep -Fq 'TILO_INSTALLER_CYCLE_MAX_INSTALL_SECONDS' "$workflow" \
  && grep -Fq 'qemu-system-aarch64' "$workflow" \
  && grep -Fq 'qemu-efi-aarch64 qemu-system-arm' "$workflow" \
  && grep -Fq 'edk2-aarch64 qemu-device-display-virtio-gpu-pci-gl qemu-system-aarch64-core' "$workflow" \
  && grep -Fq 'clang dosfstools e2fsprogs fdisk imagemagick lld mtools ntfs-3g' "$workflow" \
  && grep -Fq 'clang dosfstools e2fsprogs ImageMagick lld mtools ntfs-3g util-linux' "$workflow" \
  && grep -Fq 'clang debugfs ld.lld mattrib mcopy mdir mmd mkfs.ext4 mkfs.ntfs mkfs.vfat' "$workflow" \
  && grep -Fq 'qemu-img "$qemu_binary" sfdisk socat tesseract' "$workflow" \
  && grep -Fq 'name: tilo-installer-full-cycle-${{ matrix.arch }}' "$workflow" \
  && grep -Fxq 'os/tests/installer_cycle_graphics.py' "$repo/tools/security-boundary-invariants.txt" \
  && grep -Fq 'Build and pin the isolated installer profile encoder' "$workflow" \
  && grep -Fq -- '--manifest-path tools/imager-profile/Cargo.toml' "$workflow" \
  && grep -Fq 'encoder_sha256="$(sha256sum "$encoder"' "$workflow" \
  && grep -Fq 'TILO_PROFILE_ENCODER=%s\nTILO_PROFILE_ENCODER_SHA256=%s\n' "$workflow" \
  && grep -Fq '"$vm" login --passcode-only "$TILO_PASS"' "$cycle" \
  && grep -Fq '"$vm" init-cycle' "$cycle" \
  && grep -Fq 'truncate -s "+$((64 * 1024))" "$profiled_raw"' "$cycle" \
  && grep -Fq 'profiled live installer did not start at Disk' "$cycle" \
  && grep -Fq 'embedded username did not reach the account screen' "$cycle" \
  && grep -Fq 'customization modified the released installer medium' "$cycle" \
  && grep -Fq 'install_mode=%s' "$cycle" \
  && grep -Fq '"$unlock" "$install_mode" "$storage"' "$cycle" \
  && grep -Fq 'cold_boot_to_oobe_seconds=' "$cycle" \
  && grep -Fq 'cold_reboot_to_greeter_seconds=' "$cycle" \
  && grep -Fq 'boot-timing.txt' "$cycle" \
  && grep -Fq 'Windows filesystem bytes changed during alongside install' "$cycle" \
  && grep -Fq 'Linux filesystem bytes changed during alongside install' "$cycle" \
  && grep -Fq 'Windows EFI loader bytes changed during alongside install' "$cycle" \
  && grep -Fq 'Linux EFI loader bytes changed during alongside install' "$cycle" \
  && grep -Fq 'build-foreign-boot-fixture.sh' "$cycle" \
  && grep -Fq 'Windows fixture entry never executed from the installed boot menu' "$cycle" \
  && grep -Fq 'Linux fixture entry never executed from the installed boot menu' "$cycle" \
  && grep -Fq 'real_windows_boot_verified=false' "$cycle" \
  && grep -Fq 'real_linux_distribution_boot_verified=false' "$cycle" \
  && grep -Fq 'foreign shared EFI partition changed during guided alongside install' "$cycle" \
  && grep -Fq 'tilo-windows.conf' "$cycle" \
  && grep -Fq 'tilo-linux-fedora.conf' "$cycle" \
  && grep -Fq 'efi /EFI/Microsoft/Boot/bootmgfw.efi' "$cycle" \
  && grep -Fq 'efi /EFI/Fedora/$linux_loader' "$cycle" \
  && grep -Fq 'prepared alongside GPT layout changed' "$cycle" \
  && grep -Fq 'expected six GPT partitions' "$cycle" \
  && grep -Fq 'guided Tilo EFI partition has no architecture-matched systemd-boot binary' "$cycle" \
  && grep -Fq 'guided Tilo EFI partition contains an invalid cross-ESP chainloader shortcut' "$cycle" \
  && grep -Fq 'confirmation=/dev/vdb5' "$cycle" \
  && grep -Fq 'confirmation=/dev/vdb' "$cycle" \
  && grep -Fq 'python3 "$dev_dir/installer-console.py" --socket "$console"' "$cycle" \
  && grep -Fq 'ic invoke begin@installer.review "{\"confirmation_phrase\":\"$confirmation\"}"' "$cycle" \
  && grep -Fq 'installer began an install confirmed with the wrong device' "$cycle" \
  && grep -Fq -- '--secret passphrase=TILO_PASS --secret confirmation=TILO_PASS' "$cycle" \
  && grep -Fq 'confirm-saved@installer.recovery' "$cycle" \
  && grep -Fq 'power-off@installer.power' "$cycle" \
  && ! grep -Fq 'finish-recovery' "$cycle" \
  && grep -Fq '"$vm" keys spc tab tab ret' "$cycle" \
  && grep -Fq '::tilo-recovery-key::' "$cycle" \
  && grep -Fq '.artifact-secret-canary.txt' "$cycle" \
  && grep -Fq 'test ! -s "$out/.artifact-secret-canary.txt"' "$cycle" \
  && ! grep -Fq 'TILO_INSTALLER_CYCLE_STORAGE:-$out/storage' "$cycle" \
  && ! grep -Fq 'cp -f "$run/serial.log" "$out/installer-serial.log"' "$cycle" \
  && grep -Fq "wait_screen 08-oobe-intro" "$cycle" \
  && grep -Fq "wait_screen 09-oobe-backup" "$cycle" \
  && grep -Fq "wait_screen 10-oobe-backup-resumed" "$cycle" \
  && grep -Fq '"$vm" keys tab tab tab spc tab ret' "$cycle" \
  && grep -Fq "wait_screen 11-oobe-ready" "$cycle" \
  && grep -Fq '"$vm" keys tab tab ret' "$cycle" \
  && grep -Fq "wait_screen 12-tour-move" "$cycle" \
  && grep -Fq "wait_screen 13-tour-arrange" "$cycle" \
  && grep -Fq "wait_screen 14-tour-share" "$cycle" \
  && grep -Fq "wait_screen 15-tour-start" "$cycle" \
  && grep -Fq "wait_screen 16-desktop-config" "$cycle" \
  && grep -Fq "wait_screen 16-browser-picker" "$cycle" \
  && grep -Fq "wait_screen 17-browser-open" "$cycle" \
  && grep -Fq "wait_screen 18-browser-offline" "$cycle" \
  && grep -Fq "wait_screen 19-reboot-greeter" "$cycle" \
  && grep -Fq "wait_screen 20-reboot-login" "$cycle" \
  && ! grep -Eq '(^|[[:space:]])(ssh|journal)([[:space:]]|$)|--stub-ui|--shots' "$cycle"; then
  ok 'trusted main and tagged media must pass erase, prepared-alongside, and guided-alongside graphical install cycles'
else
  bad 'release media lack a fail-closed native full graphical installation cycle'
fi

if grep -Fq "if: startsWith(github.ref, 'refs/tags/imager-v') && matrix.arch == 'x86_64'" "$workflow" \
  && grep -Fq 'python3-virt-firmware qemu-system-x86' "$workflow" \
  && grep -Fq 'command -v swtpm virt-fw-vars' "$workflow" \
  && grep -Fq './os/dev/prepare-release-secureboot-vars.sh' "$workflow" \
  && grep -Fq 'TILO_SECBOOT_KEYS="${TILO_RELEASE_SECBOOT_VARS:?}"' "$workflow" \
  && grep -Fq -- '--software-tpm' "$workflow" \
  && grep -Fq 'dist/installer-software-tpm-pin-cycle' "$workflow" \
  && grep -Fq '"$signer" export-efi-cert' "$release_vars" \
  && grep -Fq 'tilo-crypto certificate-sha256' "$release_vars" \
  && grep -Fq -- '--add-db a0baa8a3-041d-48a8-bc87-c36d121b5e3d "$work/release-efi.crt"' "$release_vars" \
  && grep -Fq -- '--microsoft-kek none --no-microsoft --secure-boot' "$release_vars" \
  && ! grep -Fq 'TILO_VM_ARCH=aarch64' "$release_vars"; then
  ok 'tagged x86 media must pass the graphical TPM2/PIN cycle with the exact release certificate enforced'
else
  bad 'tag promotion can bypass the supported x86 measured-boot TPM2/PIN installation cycle'
fi

grep -Fq 'BOOT_SIGNAL="serial-getty"' "$boot_verify" \
  && grep -Fq 'BOOT_MS=$(( ($(date +%s) - BOOT_START) * 1000 ))' "$boot_verify" \
  && grep -Fq '"boot_signal": "$BOOT_SIGNAL"' "$boot_verify" \
  && grep -Fq 'boot reached $BOOT_SIGNAL in ${BOOT_MS}ms' "$boot_verify" \
  && ok 'serial-getty boot proof reports an explicit signal and nonzero wall time' \
  || bad 'serial-getty fallback can report a misleading zero-duration boot'

[ "$(grep -Fc 'branches: ["main"]' "$workflow")" -eq 1 ] \
  && grep -Fq "if: github.event_name != 'push' || github.ref != 'refs/heads/main'" "$workflow" \
  && [ "$(grep -Fc 'pull_request:' "$workflow")" -eq 0 ] \
  && grep -Fq 'runner":"imager-linux-x86_64"' "$workflow" \
  && grep -Fq 'runner":"imager-linux-aarch64"' "$workflow" \
  && grep -Fq 'runner":"runner-disk-100g"' "$workflow" \
  && grep -Fq 'runner":"runner-disk-100g-arm64"' "$workflow" \
  && grep -Fq 'matrix: ${{ fromJSON(needs.plan.outputs.medium-matrix) }}' "$workflow" \
  && grep -Fq "github.event.inputs.installer_only != 'true'" "$workflow" \
  && grep -Fq './os/dev/assert-native-arch.sh "${{ matrix.arch }}"' "$workflow" \
  && grep -Fq 'TILO_VM_CYCLE: release-${{ github.run_id }}-${{ github.run_attempt }}-${{ matrix.arch }}' "$workflow" \
  && grep -Fq -- '--authfile "$REGISTRY_AUTH_FILE"' "$workflow" \
  && ! grep -Fq 'TILO_REGISTRY_CREDS:' "$workflow" \
  && ok 'installer media runs only on explicit, architecture-matched native release or rehearsal hosts' \
  || bad 'installer-medium event, architecture, or registry-auth boundary is incomplete'

grep -Fq 'key: imager-linux-tools-${{ matrix.arch }}' "$workflow" \
  && grep -Fq 'linuxdeploy-1-alpha-20251107-1-${{ matrix.arch }}.AppImage' "$workflow" \
  && grep -Fq 'appimagetool-1.9.1-${{ matrix.arch }}.AppImage' "$workflow" \
  && grep -Fq 'type2-runtime-20251108-${{ matrix.arch }}' "$workflow" \
  && grep -Fq '"os/external-assets/cache/linuxdeploy-1-alpha-20251107-1-${arch}.AppImage"' "$workflow" \
  && grep -Fq -- '--runtime-file "$TILO_APPIMAGE_RUNTIME"' \
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

# t's scheduled check (tools/ci/products.toml): every six hours, never cached, with the site
# token from t's server config as its only credential, and refusing to run without it.
winget_reconcile="$repo/apps/imager/packaging/monitor-winget-acceptance.sh"
winget_check=bad
if python3 - "$repo/tools/ci/products.toml" <<'PY'
import subprocess, sys, tomllib
checks = {c["name"]: c for m in tomllib.load(open(sys.argv[1], "rb"))["matrix"] for c in m["check"]}
check = checks["imager-winget-acceptance"]
assert check["on"] == ["schedule:23 */6 * * *", "dispatch"], check["on"]
assert check["cache"] is False and check["secrets"] == ["TILO_SITE_TOKEN"]
assert "id_token" not in check and "env" not in check
assert "TILO_SITE_GIT_TOKEN=$TILO_SITE_TOKEN" in check["run"]
assert "bash apps/imager/packaging/monitor-winget-acceptance.sh" in check["run"]
unset = subprocess.run(["bash", "-c", check["run"]], env={"PATH": "/usr/bin:/bin"},
                       capture_output=True, text=True)
assert unset.returncode != 0 and "TILO_SITE_TOKEN is not set" in unset.stderr, unset.stderr
PY
then
  winget_check=ok
fi
[[ $winget_check == ok ]] \
  && grep -Fq 'state="$(jq -r '\''.channels.winget // empty'\'' "$manifest")"' "$winget_reconcile" \
  && grep -Fq 'GITHUB_REF_NAME="$tag" TILO_RELEASE_SERVER="$release_server"' "$winget_reconcile" \
  && grep -Fq 'published landing-page release has invalid WinGet state' "$winget_reconcile" \
  && ok 'WinGet acceptance is reconciled automatically, with a site token only that check holds' \
  || bad 'WinGet acceptance still requires a manual site-promotion rerun'

exit "$fail"
