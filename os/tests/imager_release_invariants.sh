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
  "$repo/os/dev/test-installer-cycle.sh" \
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

grep -Fq '"$appdir/usr/share/metainfo/org.qos.Imager.appdata.xml"' \
  "$repo/apps/imager/packaging/package-host.sh" \
  && ok 'AppImage metadata uses the desktop-id filename expected by linuxdeploy' \
  || bad 'AppImage packaging will omit discoverable application metadata'

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

grep -Fq 'acquire-release-credentials.sh' "$workflow" \
  && grep -Fq 'Acquire-ReleaseCredentials.ps1' "$workflow" \
  && [ "$(grep -Fc 'acquire-release-token.sh' "$workflow")" -eq 3 ] \
  && grep -Fq 'credential output must not preexist' \
    "$repo/apps/imager/packaging/acquire-release-token.sh" \
  && grep -Fq 'lease file has an extended' \
    "$repo/apps/imager/packaging/acquire-release-credentials.sh" \
  && grep -Fq 'IMAGER_WINDOWS_PUBLISHER' "$workflow" \
  && grep -Fq 'QOS_WINDOWS_EXPECTED_PUBLISHER' \
    "$repo/apps/imager/packaging/package-host.ps1" \
  && grep -Fq 'IMAGER_MAC_TEAM_ID' "$workflow" \
  && grep -Fq 'IMAGER_MAC_CERT_SHA256' "$workflow" \
  && grep -Fq 'QOS_MAC_EXPECTED_CERT_SHA256' \
    "$repo/apps/imager/packaging/package-host.sh" \
  && [ "$(grep -Fc 'if: always()' "$workflow")" -ge 2 ] \
  && ok 'release brokers, signer paths, publisher identities, and cleanup fail closed' \
  || bad 'release credential outputs or signing identities are insufficiently constrained'

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
if grep -Fq 'host-media-writer = []' "$repo/apps/imager/Cargo.toml" \
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
  && grep -Fq 'homebrew "$CREDENTIAL_BROKER" "$token_file"' "$workflow" \
  && grep -Fq 'winget "$CREDENTIAL_BROKER" "$token_file"' "$workflow" \
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
  && grep -Fq 'podman login \' "$workflow" \
  && grep -Fq -- '--authfile "$REGISTRY_AUTH_FILE" code.sandbox.example' "$workflow" \
  && grep -Fq './os/dev/install-cached-cosign.sh' "$workflow" \
  && ok 'installer media restores pinned inputs and can authenticate/verify frozen images' \
  || bad 'installer-media release lane lacks immutable inputs or private-image prerequisites'

cycle="$repo/os/dev/test-installer-cycle.sh"
boot_verify="$repo/scripts/perf/qemu-install-boot-verify.sh"
if grep -Fq './os/dev/test-installer-cycle.sh' "$workflow" \
  && grep -Fq -- '--prepared-alongside' "$workflow" \
  && grep -Fq 'QOS_INSTALLER_CYCLE_MAX_INSTALL_SECONDS' "$workflow" \
  && grep -Fq 'qemu-system-aarch64' "$workflow" \
  && grep -Fq 'qemu-efi-aarch64 qemu-system-arm' "$workflow" \
  && grep -Fq 'edk2-aarch64 qemu-device-display-virtio-gpu-pci-gl qemu-system-aarch64-core' "$workflow" \
  && grep -Fq 'dosfstools e2fsprogs fdisk imagemagick mtools ntfs-3g' "$workflow" \
  && grep -Fq 'ImageMagick mtools ntfs-3g util-linux' "$workflow" \
  && grep -Fq 'qemu-img "$qemu_binary" sfdisk socat tesseract' "$workflow" \
  && grep -Fq 'name: qos-installer-full-cycle-${{ matrix.arch }}' "$workflow" \
  && grep -Fq '"$vm" boot-installer --gl --no-tpm' "$cycle" \
  && grep -Fq '"$vm" boot-target --gl --no-tpm' "$cycle" \
  && grep -Fq '"$vm" login --passcode-only "$QOS_PASS"' "$cycle" \
  && grep -Fq '"$vm" init-cycle' "$cycle" \
  && grep -Fq 'cargo run -q -p imager --locked --example embed-profile' "$cycle" \
  && grep -Fq 'truncate -s "+$((64 * 1024))" "$profiled_raw"' "$cycle" \
  && grep -Fq 'profiled live installer did not start at Disk' "$cycle" \
  && grep -Fq 'embedded username did not reach the account screen' "$cycle" \
  && grep -Fq 'customization modified the released installer medium' "$cycle" \
  && grep -Fq 'Windows filesystem bytes changed during alongside install' "$cycle" \
  && grep -Fq 'Linux filesystem bytes changed during alongside install' "$cycle" \
  && grep -Fq 'qos-windows.conf' "$cycle" \
  && grep -Fq 'qos-linux-fedora.conf' "$cycle" \
  && grep -Fq 'confirmation=/dev/vdb4' "$cycle" \
  && grep -Fq "wait_private_screen 'qOS" "$cycle" \
  && grep -Fq '"$vm" keys ret tab spc tab tab ret' "$cycle" \
  && grep -Fq '"$vm" keys spc tab tab ret' "$cycle" \
  && grep -Fq 'rm -f -- "$run/private-observe.png"' "$cycle" \
  && grep -Fq '::qos-recovery-key::' "$cycle" \
  && grep -Fq '.artifact-secret-canary.txt' "$cycle" \
  && grep -Fq 'test ! -s "$out/.artifact-secret-canary.txt"' "$cycle" \
  && ! grep -Fq 'QOS_INSTALLER_CYCLE_STORAGE:-$out/storage' "$cycle" \
  && ! grep -Fq 'cp -f "$run/serial.log" "$out/installer-serial.log"' "$cycle" \
  && grep -Fq "wait_screen 08-oobe-intro" "$cycle" \
  && grep -Fq "wait_screen 09-oobe-ready" "$cycle" \
  && grep -Fq '"$vm" keys tab tab ret' "$cycle" \
  && grep -Fq "wait_screen 10-tour-move" "$cycle" \
  && grep -Fq "wait_screen 11-tour-arrange" "$cycle" \
  && grep -Fq "wait_screen 12-tour-share" "$cycle" \
  && grep -Fq "wait_screen 13-desktop-config" "$cycle" \
  && ! grep -Eq '(^|[[:space:]])(ssh|journal)([[:space:]]|$)|--stub-ui|--shots' "$cycle"; then
  ok 'every tagged medium must pass a profiled graphical install, encrypted boot, login, OOBE, and desktop cycle'
else
  bad 'release media lack a fail-closed native full graphical installation cycle'
fi

grep -Fq 'BOOT_SIGNAL="serial-getty"' "$boot_verify" \
  && grep -Fq 'BOOT_MS=$(( ($(date +%s) - BOOT_START) * 1000 ))' "$boot_verify" \
  && grep -Fq '"boot_signal": "$BOOT_SIGNAL"' "$boot_verify" \
  && grep -Fq 'boot reached $BOOT_SIGNAL in ${BOOT_MS}ms' "$boot_verify" \
  && ok 'serial-getty boot proof reports an explicit signal and nonzero wall time' \
  || bad 'serial-getty fallback can report a misleading zero-duration boot'

[ "$(grep -Fc '      - "os/**"' "$workflow")" -eq 2 ] \
  && [ "$(grep -Fc '      - "shell/**"' "$workflow")" -eq 2 ] \
  && grep -Fq './os/dev/assert-native-arch.sh "${{ matrix.arch }}"' "$workflow" \
  && grep -Fq 'QOS_VM_CYCLE: release-${{ github.run_id }}-${{ github.run_attempt }}-${{ matrix.arch }}' "$workflow" \
  && grep -Fq -- '--authfile "$REGISTRY_AUTH_FILE"' "$workflow" \
  && ! grep -Fq 'QOS_REGISTRY_CREDS:' "$workflow" \
  && ok 'installer-medium changes trigger native, short-lived-authenticated architecture-matched builds' \
  || bad 'installer-medium trigger, architecture, or registry-auth boundary is incomplete'

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
