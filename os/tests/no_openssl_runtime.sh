#!/usr/bin/env bash
# Keep OpenSSL and its sbsigntools consumers out of Tilo, including the kernel
# build toolchain.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fail=0
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }
ok() { printf 'ok    %s\n' "$*"; }
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

for package in openssl native-tls; do
  if RUSTC_WRAPPER= cargo tree --manifest-path "$repo/Cargo.toml" --workspace -i "$package" -e normal \
      >/dev/null 2>&1; then
    bad "active Rust dependency graph contains $package"
  else
    ok "active Rust dependency graph contains no $package crate"
  fi
done
for package in aws-lc-rs aws-lc-sys; do
  if RUSTC_WRAPPER= cargo tree --manifest-path "$repo/Cargo.toml" --workspace -i "$package" -e normal \
      >/dev/null 2>&1; then
    bad "active Rust dependency graph contains duplicate crypto provider $package"
  else
    ok "active Rust dependency graph uses no $package provider"
  fi
done

manifest_hits="$(find "$repo/os" "$repo/.forgejo" -type f \
    \( -name 'Containerfile*' -o -name '*.yml' -o -name '*.yaml' \) -print0 \
  | xargs -0 -r rg -n -i '\b(openssl(-devel)?|sbsigntools)\b' \
  | grep -Ev '(^|:)[0-9]+:[[:space:]]*#|rpm -q|microdnf remove|dnf remove|libssl|libcrypto' \
  | grep -Ev '^.*/os/Containerfile:[0-9]+:RUN --mount=type=bind,from=public-trust-tools,source=/usr/bin/openssl,target=/run/tilo-build-tools/openssl,ro \\$' \
  | grep -Ev ': *&& (! )?printf '"'"'%s\\n'"'"' "\$installed" \| (awk '"'"'/\^\([[:alnum:]_|-]+\)\$/'"'"'|grep -Eq '"'"'\^\([[:alnum:]_|-]+\)\$'"'"') *\\?$' \
  || true)"
if [ -n "$manifest_hits" ]; then
  printf '%s\n' "$manifest_hits" >&2
  bad 'a build manifest or workflow actively installs OpenSSL/sbsigntools'
else
  ok 'runtime manifests and workflows avoid OpenSSL/sbsigntools outside the isolated upstream bootc builder'
fi

trust_tool_bind="$(grep -Fxc \
  'RUN --mount=type=bind,from=public-trust-tools,source=/usr/bin/openssl,target=/run/tilo-build-tools/openssl,ro \' \
  "$repo/os/Containerfile")"
if [ "$trust_tool_bind" -eq 1 ] \
  && grep -Fq 'FROM ${BASE_REF} AS public-trust-tools' "$repo/os/Containerfile" \
  && grep -Fq 'PATH="/run/tilo-build-tools:$PATH" python3 /run/tilo-trust-tools/install-public-trust.py' \
       "$repo/os/Containerfile"; then
  ok 'late public trust parsing receives one read-only build mount outside the runtime PATH'
else
  bad 'late public trust parser is not confined to its exact read-only build mount'
fi

crypto_source="$repo/crates/system/tilo-crypto/src/lib.rs"
development_signer="$repo/os/dev/development-signer.py"
development_signer_setup="$repo/os/dev/setup-development-signer.py"
development_trust="$repo/os/dev/development-trust.py"
fex_buildroot_container="$repo/os/Containerfile.fex-buildroot"
development_boot_fixture="$repo/os/tests/test_development_boot.py"
development_signer_fixture="$repo/os/tests/development_signer.py"
tls_registry_fixture="$repo/os/tests/tls_registry_fixture.py"
if grep -Eq 'pkcs1v15::SigningKey|SigningKey::<Sha(256|512)>' "$crypto_source"; then
  bad 'Tilo-owned RSA signing regressed from ring to the timing-vulnerable RustCrypto operation'
elif grep -Fq 'AuthenticodeSignature::new' "$crypto_source"; then
  bad 'EFI Authenticode regressed to Picky RSA private signing'
elif grep -Eq '(^|[^[:alnum:]_])decrypt(_|\()' "$crypto_source"; then
  bad 'tilo-crypto exposes an RSA decryption path affected by RUSTSEC-2023-0071'
elif [ "$(grep -c 'sign_rsa_pkcs1_ring(' "$crypto_source")" -lt 4 ]; then
  bad 'not every Tilo-owned rollback/module/EFI-variable RSA signer uses ring'
else
  ok 'Tilo-owned RSA private operations use ring and expose no Marvin decryption oracle'
fi
if grep -Eq "(/usr/bin/(openssl|sbsign)|\['(openssl|sbsign)')" \
    "$development_signer" "$development_signer_setup"; then
  bad 'development key provisioning or private operations still invoke OpenSSL/sbsigntools'
else
  ok 'development key provisioning and private operations use the Rust-native signer'
fi
if grep -Eq "(^|[^[:alnum:]_])openssl([^[:alnum:]_]|$)" "$development_trust"; then
  bad 'development public trust parsing still invokes OpenSSL'
else
  ok 'development Cosign, PCR, and certificate identities use Rust-native parsing'
fi
if rg -n -i '(/usr/bin/(openssl|sbsign|sbverify)|\b(openssl|sbsign|sbverify)\b)' \
    "$development_signer" "$development_signer_setup" \
    "$development_trust" "$repo/os/dev/verify-development-loader.py" \
    "$repo/os/dev/verify-installed-development-disk.py" \
    "$repo/os/dev/development_boot.py" "$repo/os/dev/development_prediction.py" \
    "$repo/os/dev/retain-development-rootfs.py" "$repo/os/dev/signing_spool.py" >/dev/null; then
  bad 'development trust tooling still invokes OpenSSL or sbsigntools'
else
  ok 'retained development trust tooling uses only Rust-native crypto'
fi
if grep -Eq 'dnf .*install.*openssl-devel|^[[:space:]]+.*openssl-devel' \
    "$fex_buildroot_container"; then
  bad 'FEX cross-build tooling still installs OpenSSL development files'
else
  ok 'FEX cross-build tooling does not carry OpenSSL development files'
fi
if rg -n -i '\b(openssl|sbsign|sbverify)\b' \
    "$development_boot_fixture" "$development_signer_fixture" \
    "$repo/os/tests/test_signing_spool.py" \
    "$repo/os/tests/test_development_trust.py" \
    "$repo/os/tests/development_source_registry.py" \
    "$tls_registry_fixture" \
    "$repo/os/tests/development_rootfs.py" \
    "$repo/os/tests/Containerfile.development-source" \
    "$repo/os/tests/test_rebind_development_trust.py" \
    "$repo/os/tests/test_complete_initramfs_rebind.py" >/dev/null; then
  bad 'development EFI/private-signing fixtures still invoke OpenSSL or sbsigntools'
else
  ok 'development EFI/private-signing fixtures use Rust-native crypto'
fi
if RUSTC_WRAPPER= cargo tree --manifest-path "$repo/Cargo.toml" -p tilo-crypto -d \
    | grep -Eq '^rsa v'; then
  bad 'privileged tilo-crypto carries duplicate RSA implementations'
else
  ok 'privileged crypto tooling carries one audited RSA parser/verifier implementation'
fi
if cargo tree --workspace -i openssl-probe >/dev/null 2>&1; then
  bad 'OpenSSL path-probing code remains in the Rust dependency graph'
else
  ok 'Rustls trust loading has no openssl-probe dependency'
fi

lock_hits="$(find "$repo" -name Cargo.lock -not -path '*/target/*' -print0 \
  | xargs -0 -r rg -l '^name = "(openssl|openssl-sys|openssl-probe|native-tls)"' || true)"
if [ -n "$lock_hits" ]; then
  printf '%s\n' "$lock_hits" >&2
  bad 'an OpenSSL/native-tls crate remains in a repository Cargo lockfile'
else
  ok 'all repository Cargo lockfiles are free of OpenSSL/native-tls crates'
fi

if rg -n '^[[:space:]]*efitools([[:space:]\\]|$)' \
    "$repo/os/Containerfile" "$repo/os/Containerfile.host" >/dev/null; then
  bad 'runtime image still installs efitools, whose firmware utilities link libcrypto'
else
  ok 'runtime image does not install libcrypto-linked efitools'
fi

# These exact selectors operate on rpm's installed-name inventory, not package
# install arguments. Require both the removal and a fresh absence check.
inventory_cleanup() {
  local block="$1" selection="$2" removal="$3" absent="$4"
  [ "$(grep -Fc "installed=\"\$(rpm -qa --qf '%{NAME}\\n')\"" <<<"$block")" -ge 2 ] \
    && grep -Fq "awk '/$selection/'" <<<"$block" \
    && grep -Fq "$removal" <<<"$block" \
    && grep -Fq "! printf '%s\\n' \"\$installed\" | grep -Eq '$absent'" <<<"$block"
}
runtime_wayland="$(sed -n '/# The frozen snapshot already resolves these development specs/,/FINALIZE_RPMDB/p' \
  "$repo/os/Containerfile")"
if grep -Fq 'rpm -e --nodeps wayland-devel libxkbcommon-devel libinput-devel' <<<"$runtime_wayland" \
  && inventory_cleanup "$runtime_wayland" \
    '^(pkgconf-pkg-config|pkgconf|pkgconf-m4|libpkgconf)$' \
    'xargs -r rpm -e --nodeps' \
    '^(wayland-devel|libxkbcommon-devel|libinput-devel|pkgconf-pkg-config|pkgconf|pkgconf-m4|libpkgconf)$'; then
  ok 'immutable runtime erases Wayland/input development and pkg-config payloads after resolving libraries'
else
  bad 'runtime rootfs retains Wayland/input development payloads'
fi

runtime_splash="$(sed -n '/# The ukify transaction\./,/# The install-config drop-in/p' "$repo/os/Containerfile")"
crypto_packages='systemd-ukify|openssl|sbsigntools|efitools|mokutil|pesign|nss-tools|gnutls-utils|git|git-core|openvpn|NetworkManager-openvpn|openssh-server'
if inventory_cleanup "$runtime_splash" "^($crypto_packages)\$" \
    "xargs -r dnf --disablerepo='*' remove -y" "^(python3-pillow|$crypto_packages)\$"; then
  ok 'early rootfs cleanup removes inherited crypto CLI packages before later feature dependencies'
else
  bad 'final image does not purge inherited OpenSSL consumers/legacy EFI command-line tools'
fi
if grep -Fq "dnf --disablerepo='*' remove -y systemd-ukify python3-pillow" <<<"$runtime_splash"; then
  ok 'splash rendering removes its one-shot Pillow dependency and does not install runtime ukify'
else
  bad 'runtime rootfs retains build-only Pillow or systemd-ukify tooling'
fi

if sed -n '/# System utilities\./,/xdg-desktop-portal/p' "$repo/os/Containerfile" \
    | grep -Eq '^[[:space:]]+(git|NetworkManager-openvpn)[[:space:]\\]'; then
  bad 'immutable host explicitly installs Git or OpenVPN, both avoidable OpenSSL consumers'
else
  ok 'Git and OpenVPN are excluded from the immutable host crypto surface'
fi

music_packages="$(sed -n '/# GStreamer/,/# Display manager/p' "$repo/os/Containerfile")"
if grep -Eq '^[[:space:]]+(yt-dlp|python3-dbus-next|python3-gobject)[[:space:]\\]' \
    <<<"$music_packages"; then
  bad 'Music retains the external downloader or unused Python bindings'
elif grep -Fq 'youtubei/v1/player' "$repo/apps/music/src/backend/android_vr.rs" \
   && grep -Fq 'rustls-no-provider' "$repo/apps/music/Cargo.toml" \
   && ! rg -n 'backend::ytdlp|Command::new\("yt-dlp"\)' "$repo/apps/music/src" >/dev/null; then
  ok 'Music full-track resolution is Rust-native and removes yt-dlp/Python bindings'
else
  bad 'Music does not carry a Rust-native full-track resolver'
fi

host_runtime="$(sed -n '/# Minimal runtime/,/# Copy binaries/p' "$repo/os/Containerfile.host")"
if sed -e "/rpm -q --qf.*openssl sbsigntools/d" \
       -e '/^[[:space:]]*openvpn NetworkManager-openvpn openssh/d' \
       -e '/^[[:space:]]*ModemManager fwupd .*xargs/d' <<<"$host_runtime" \
    | grep -Eq '^[[:space:]]+(NetworkManager-openvpn|ModemManager|fwupd)[[:space:]\\]'; then
  bad 'headless relay image installs VPN/modem/firmware stacks outside its appliance role'
elif ! grep -Fq "rpm -q --qf '%{NAME}\\n' openssl sbsigntools efitools mokutil pesign nss-tools gnutls-utils git git-core" \
      <<<"$host_runtime"; then
  bad 'headless relay does not purge avoidable OpenSSL consumers inherited from its base'
else
  ok 'headless relay excludes the OpenSSL-linked VPN plugin and unused hardware daemons'
fi

runtime_source_roots=("$repo/os/usr" "$repo/apps" "$repo/crates")
missing_runtime_source_root=0
for root in "${runtime_source_roots[@]}"; do
  if [ ! -d "$root" ]; then
    bad "runtime source scan root does not exist: ${root#"$repo/"}"
    missing_runtime_source_root=1
  fi
done
if [ "$missing_runtime_source_root" -eq 0 ]; then
  if rg -n 'Command::new\("ssh"\)|^[[:space:]]*(exec[[:space:]]+)?ssh[[:space:]]' \
      "${runtime_source_roots[@]}" \
      -g '*.rs' -g '*.sh' -g '!**/tests/**' -g '!**/crates/dev/**' >/dev/null; then
    bad 'Tilo-owned runtime source invokes OpenSSH outside the reviewed distro dependency exception'
  else
    ok 'Tilo-owned Remote transport does not invoke the distro OpenSSH CLI'
  fi
fi

runtime_network="$(sed -n '/# System utilities\./,/xdg-desktop-portal/p' "$repo/os/Containerfile")"
if inventory_cleanup "$runtime_network" '^systemd-(coredump|networkd|resolved)$' \
    "xargs -r dnf --disablerepo='*' remove -y" '^systemd-(coredump|networkd|resolved)$' \
  && grep -Fq 'test "$(readlink /etc/resolv.conf)" = ../run/NetworkManager/resolv.conf' <<<"$runtime_network" \
  && awk '/ln -sfn .*NetworkManager/ { switched=1 } /xargs -r dnf/ { if (!switched) exit 1; removed=1 } END { if (!removed) exit 1 }' <<<"$runtime_network" \
  && grep -Fq 'installed systemd-networkd' "$repo/os/usr/libexec/tilo-security-audit"; then
  ok 'redundant systemd network daemons are removed and audited'
else
  bad 'systemd network daemons remain in the NetworkManager-owned runtime'
fi
grep -Fq 'installed systemd-coredump' "$repo/os/usr/libexec/tilo-security-audit" \
  && grep -Fq 'systemd-coredump.socket' "$repo/os/Containerfile" \
  || bad 'runtime can retain process memory through inherited systemd-coredump'

native_roots="$repo/crates/system/tilo-native-certs/src/lib.rs"
if grep -Fq 'const TILO_CA_BUNDLE: &str = "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem"' "$native_roots" \
   && grep -Fq 'Path::new(TILO_CA_BUNDLE)' "$native_roots" \
   && ! grep -Fq 'var_os("SSL_CERT_FILE")' "$native_roots"; then
  ok 'Rustls trust is fixed to the immutable OS bundle and ignores environment overrides'
else
  bad 'Rustls platform trust can be redirected or fall back to path probing'
fi

runtime_hits="$(rg -n 'Command::new\("(openssl|sbsign|sbverify)"\)|run(_capture|_with_input)?\(\s*"(openssl|sbsign|sbverify)"|(^|[;&|[:space:]])(openssl|sbsign|sbverify)[[:space:]]' \
  "$repo/os/usr" "$repo/crates" "$repo/apps" \
  -g '*.rs' -g '*.sh' \
  -g '!**/tests/**' -g '!**/target/**' -g '!**/data/**' \
  -g '!**/apps/imager/packaging/package-host.sh' \
  -g '!Cargo.lock' || true)"
if [ -n "$runtime_hits" ]; then
  printf '%s\n' "$runtime_hits" >&2
  bad 'shipped runtime source still invokes OpenSSL/sbsigntools'
else
  ok 'shipped runtime source has no OpenSSL/sbsigntools path'
fi

if sed '/^[[:space:]]*#/d' "$repo/os/dev/test-secureboot.sh" \
    | grep -Eq '(^|[;&|[:space:]])mokutil([;&|[:space:]]|$)'; then
  bad 'Secure Boot VM validation still depends on removed mokutil'
else
  ok 'Secure Boot VM validation uses the shipped Rust-native/direct-firmware status path'
fi

for manifest in \
  "$repo/os/Containerfile" \
  "$repo/os/kernel/Containerfile" \
  "$repo/os/containers/tilo-relay/Containerfile" \
  "$repo/dist/arch/PKGBUILD" \
  "$repo/dist/nix/package.nix"; do
  if sed -e '/^[[:space:]]*#/d' \
         -e "/rpm -q --qf.*openssl sbsigntools efitools mokutil/d" \
         -e '/microdnf remove.*openssl-libs/d' \
         -e '/name.*libssl.*libcrypto.*openssl/d' "$manifest" \
      | grep -Eqi '(^|[[:space:]\\,])(openssl(-devel)?|sbsigntools)([[:space:]\\,]|$)'; then
    bad "runtime/package manifest still requests OpenSSL/sbsigntools: ${manifest#"$repo/"}"
  fi
done

relay_container="$repo/os/containers/tilo-relay/Containerfile"
relay_entrypoint="$repo/os/containers/tilo-relay/tilo-home-peer"
if grep -Fq 'openssl-libs coreutils dnf5' "$relay_container" \
  && grep -Fq -- "-name 'libssl.so*' -o -name 'libcrypto.so*' -o -name openssl" "$relay_container" \
  && grep -Fq '/build/os/scripts/verify-elf-hardening.sh /out' "$relay_container" \
  && grep -Fq '#!/usr/bin/busybox sh' "$relay_entrypoint"; then
  ok 'standalone relay verifies its ELFs, strips OpenSSL/package-manager, and uses static BusyBox'
else
  bad 'standalone relay runtime does not enforce its OpenSSL-free minimal surface'
fi

grep -Fq -- "-o -name 'libshout.so*'" "$repo/os/Containerfile" \
  && grep -Fq 'rpm -e --nodeps libshout' "$repo/os/Containerfile" \
  && grep -Fxq 'libshout' <(sed -n "/^forbidden_packages='/,/^'/p" \
    "$repo/os/scripts/verify-runtime-crypto-packages.sh") \
  && grep -Fq -- "-o -name 'libshout.so*'" "$repo/os/usr/libexec/tilo-security-audit" \
  && ok 'orphaned OpenSSL-linked Shoutcast package and payload are removed and audited' \
  || bad 'orphaned OpenSSL-linked Shoutcast package or payload is not removed and audited'

forbidden_list=$(sed -n "/^forbidden_packages='/,/^'/p" \
  "$repo/os/scripts/verify-runtime-crypto-packages.sh")
grep -Fq -- '--exclude=dosbox-staging' "$repo/os/Containerfile" \
  && grep -Fq "rpm -qa --qf '%{NAME}\n' perl-IO-Socket-SSL perl-Net-SSLeay qatlib-service qatlib" "$repo/os/Containerfile" \
  && grep -Fxq 'perl-Net-SSLeay' <<<"$forbidden_list" \
  && grep -Fxq 'perl-IO-Socket-SSL' <<<"$forbidden_list" \
  && grep -Fxq 'qatlib' <<<"$forbidden_list" \
  && grep -Fxq 'qatlib-service' <<<"$forbidden_list" \
  && ok "wine's DOS emulator and Perl's OpenSSL binding stay out of the rootfs" \
  || bad "wine's DOS emulator or Perl's OpenSSL binding can reach the rootfs"

if [ "$(grep -Fc -- "-o -name 'libevent_openssl.so*'" "$repo/os/Containerfile")" -eq 2 ] \
  && grep -Fq -- "-o -name 'libevent_openssl.so*'" \
       "$repo/os/usr/libexec/tilo-security-audit"; then
  ok 'unused libevent OpenSSL adapter is removed and audited'
else
  bad 'unused libevent OpenSSL adapter is not removed and audited'
fi

grep -Fq 'tilo-crypto sign-module' "$repo/os/kernel/build-support.sh" \
  && grep -Fq 'tilo-crypto verify-module' "$repo/os/kernel/build-support.sh" \
  || bad 'kernel modules are not signed and verified by tilo-crypto'
if grep -Fq "sed -i '/hostprogs-always-\$(CONFIG_MODULE_SIG_FORMAT).*sign-file/d' scripts/Makefile" \
    "$repo/os/kernel/build-support.sh" \
  && grep -Fq "sed -i '/^hostprogs := extract-cert\$/d' certs/Makefile" \
    "$repo/os/kernel/build-support.sh"; then
  ok 'kernel recipe disables upstream libcrypto host helpers'
else
  bad 'kernel recipe does not disable upstream libcrypto host helpers'
fi

secureboot="$repo/os/usr/libexec/tilo-secureboot"
deployment="$repo/os/usr/libexec/tilo-deployment"
grep -Fq 'tilo-crypto sign-efi' "$secureboot" \
  && grep -Fq 'tilo-crypto verify-efi' "$secureboot" \
  || bad 'Secure Boot runtime is not pinned to tilo-crypto EFI signing and verification'
grep -Fq 'tilo-crypto verify-efi' "$deployment" \
  || bad 'deployment repair path does not verify its newly signed EFI image'

elf_gate="$repo/os/scripts/verify-elf-hardening.sh"
grep -Fq "direct OpenSSL ABI dependency (DT_NEEDED libssl/libcrypto)" "$elf_gate" \
  || bad 'final Tilo ELF artifacts are not checked for OpenSSL ABI linkage'
grep -Fq '/build/os/scripts/verify-elf-hardening.sh /out' "$repo/os/Containerfile" \
  || bad 'final Tilo ELF linkage verifier is not part of the image build'

runtime_package_gate="$repo/os/scripts/verify-runtime-crypto-packages.sh"
grep -Fq 'unreviewed runtime packages directly requiring libssl' "$runtime_package_gate" \
  && grep -Fq 'unreviewed runtime packages directly requiring libcrypto' "$runtime_package_gate" \
  && grep -Fq 'bash /usr/local/bin/verify-runtime-crypto-packages.sh' "$repo/os/Containerfile" \
  && grep -Fq 'rm -f /usr/local/bin/verify-runtime-crypto-packages.sh' "$repo/os/Containerfile" \
  || bad 'finished rootfs does not enforce and retire its residual OpenSSL ABI allowlist gate'
grep -Fq -- "-path '*/ossl-modules/legacy.so'" "$repo/os/Containerfile" \
  && grep -Fq -- "-path '*/engines-3/*.so'" "$repo/os/Containerfile" \
  && grep -Fq -- "-path '*/gstreamer-1.0/libgstdtls.so'" "$repo/os/Containerfile" \
  && grep -Fq -- "-path '*/gstreamer-1.0/libgstcurl.so'" "$repo/os/Containerfile" \
  && grep -Fq -- "-path '*/gstreamer-1.0/libgstshout2.so'" "$repo/os/Containerfile" \
  && grep -Fq -- "-path '*/gstreamer-1.0/libgstwebrtc.so'" "$repo/os/Containerfile" \
  && grep -Fq -- "-path '*/gstreamer-1.0/libgstnice.so'" "$repo/os/Containerfile" \
  && grep -Fq -- "-path '*/gstreamer-1.0/libgstsrtp.so'" "$repo/os/Containerfile" \
  && grep -Fq 'optional OpenSSL modules and unused network media plugins absent' \
       "$repo/os/usr/libexec/tilo-security-audit" \
  || bad 'sealed rootfs does not remove and audit optional OpenSSL provider/engine/media modules'
rootfs_end_line="$(grep -n '^FROM rootfs AS split' "$repo/os/Containerfile" | cut -d: -f1)"
last_package_mutation_line="$(rg -n 'dnf .*\b(install|remove)\b|rpm -[ei]' "$repo/os/Containerfile" \
  | awk -F: -v end="$rootfs_end_line" '$1 < end { line=$1 } END { print line+0 }')"
runtime_gate_line="$(grep -n 'bash /usr/local/bin/verify-runtime-crypto-packages.sh' \
  "$repo/os/Containerfile" | cut -d: -f1)"
if [ "$runtime_gate_line" -gt "$last_package_mutation_line" ] \
   && [ "$runtime_gate_line" -lt "$rootfs_end_line" ]; then
  ok 'residual OpenSSL ABI gate runs after the final rootfs package mutation'
else
  bad 'residual OpenSSL ABI gate does not inspect the finished rootfs'
fi

if python3 "$repo/os/tests/test_runtime_crypto_packages.py" --gate "$runtime_package_gate"; then
  ok 'finished-rootfs RPM policy enforces conditional desktop/VM exceptions and rejects unknown consumers'
else
  bad 'finished-rootfs RPM policy fixtures failed'
fi

exit "$fail"
