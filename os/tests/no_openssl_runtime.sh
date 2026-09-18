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

crypto_source="$repo/crates/system/tilo-crypto/src/lib.rs"
development_signer="$repo/os/dev/development-signer.py"
development_signer_setup="$repo/os/dev/setup-development-signer.py"
development_compiler="$repo/os/dev/compile-development-os.py"
development_trust="$repo/os/dev/development-trust.py"
development_compile_container="$repo/os/Containerfile.development-compile"
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
if grep -Eq "run\('openssl', '(genpkey|pkey)'|subprocess\.run\(\['openssl', 'pkey'" \
    "$development_compiler"; then
  bad 'development registry-owner key generation still invokes OpenSSL'
else
  ok 'development registry-owner key generation uses Rust-native Ed25519'
fi
if grep -Eq "(^|[^[:alnum:]_])openssl([^[:alnum:]_]|$)" "$development_trust"; then
  bad 'development public trust parsing still invokes OpenSSL'
else
  ok 'development Cosign, PCR, and certificate identities use Rust-native parsing'
fi
if rg -n -i '(/usr/bin/(openssl|sbsign|sbverify)|\b(openssl|sbsign|sbverify)\b)' \
    "$development_signer" "$development_signer_setup" "$development_compiler" \
    "$development_trust" "$repo/os/dev/verify-development-loader.py" \
    "$repo/os/dev/development_boot.py" "$repo/os/dev/development_prediction.py" \
    "$repo/os/dev/retain-development-rootfs.py" >/dev/null; then
  bad 'development trust tooling still invokes OpenSSL or sbsigntools'
elif grep -Eq 'dnf .*install.*(openssl|sbsigntools)' "$development_compile_container"; then
  bad 'retained development compiler still installs OpenSSL or sbsigntools'
else
  ok 'retained development trust tooling uses only Rust-native crypto'
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

runtime_wayland="$(sed -n '/# The frozen snapshot already resolves these development specs/,/FINALIZE_RPMDB/p' \
  "$repo/os/Containerfile")"
if grep -Fq 'rpm -e --nodeps wayland-devel libxkbcommon-devel libinput-devel' \
    <<<"$runtime_wayland" \
  && grep -Fq '! rpm -q wayland-devel libxkbcommon-devel libinput-devel' \
    <<<"$runtime_wayland" \
  && grep -Fq '! rpm -q pkgconf-pkg-config pkgconf pkgconf-m4 libpkgconf' \
    <<<"$runtime_wayland"; then
  ok 'immutable runtime erases Wayland/input development and pkg-config payloads after resolving libraries'
else
  bad 'runtime rootfs retains Wayland/input development payloads'
fi

if grep -Fq "rpm -q --qf '%{NAME}\\n' systemd-ukify openssl sbsigntools efitools mokutil pesign nss-tools gnutls-utils git git-core openvpn NetworkManager-openvpn openssh openssh-clients openssh-server" \
    "$repo/os/Containerfile"; then
  ok 'final image purges runtime ukify, avoidable OpenSSL consumers, and legacy EFI CLIs inherited from its base'
else
  bad 'final image does not purge inherited OpenSSL consumers/legacy EFI command-line tools'
fi
if sed -n '/# Splash rendering needs Pillow/,/# The install-config drop-in/p' \
    "$repo/os/Containerfile" | grep -Fq 'systemd-ukify' \
   && sed -n '/# Splash rendering needs Pillow/,/# The install-config drop-in/p' \
      "$repo/os/Containerfile" | grep -Fq 'dnf remove -y systemd-ukify python3-pillow'; then
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

if rg -n 'Command::new\("ssh"\)|^[[:space:]]*(exec[[:space:]]+)?ssh[[:space:]]' \
    "$repo/os/usr" "$repo/apps" "$repo/crates" "$repo/shell" \
    -g '*.rs' -g '*.sh' -g '!**/tests/**' >/dev/null; then
  bad 'OpenSSH purge is unsafe because a shipped runtime still invokes its CLI'
else
  ok 'Rust Remote transport permits inherited OpenSSH packages to be purged'
fi

if grep -Fq "rpm -q --qf '%{NAME}\\n' systemd-coredump systemd-networkd systemd-resolved" "$repo/os/Containerfile" \
  && grep -Fq '../run/NetworkManager/resolv.conf' "$repo/os/Containerfile" \
  && grep -Fq 'rpm -q systemd-networkd' "$repo/os/usr/libexec/tilo-security-audit"; then
  ok 'redundant systemd network daemons are removed and audited'
else
  bad 'systemd network daemons remain in the NetworkManager-owned runtime'
fi
grep -Fq 'rpm -q systemd-coredump' "$repo/os/usr/libexec/tilo-security-audit" \
  && grep -Fq 'systemd-coredump.socket' "$repo/os/Containerfile" \
  || bad 'runtime can retain process memory through inherited systemd-coredump'

native_roots="$repo/crates/system/tilo-native-certs/src/lib.rs"
if grep -Fq 'const TILO_CA_BUNDLE: &str = "/etc/pki/tls/certs/ca-bundle.crt"' "$native_roots" \
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

grep -Fq 'tilo-crypto sign-module' "$repo/os/kernel/Containerfile" \
  && grep -Fq 'tilo-crypto verify-module' "$repo/os/kernel/Containerfile" \
  || bad 'kernel modules are not signed and verified by tilo-crypto'
if grep -Fq "sed -i '/hostprogs-always-\$(CONFIG_MODULE_SIG_FORMAT).*sign-file/d' scripts/Makefile" \
    "$repo/os/kernel/Containerfile" \
  && grep -Fq "sed -i '/^hostprogs := extract-cert\$/d' certs/Makefile" \
    "$repo/os/kernel/Containerfile"; then
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
rootfs_end_line="$(grep -n '^FROM rootfs AS split' "$repo/os/Containerfile" | cut -d: -f1)"
last_package_mutation_line="$(rg -n 'dnf (install|remove)|rpm -[ei]' "$repo/os/Containerfile" \
  | awk -F: -v end="$rootfs_end_line" '$1 < end { line=$1 } END { print line+0 }')"
runtime_gate_line="$(grep -n 'bash /usr/local/bin/verify-runtime-crypto-packages.sh' \
  "$repo/os/Containerfile" | cut -d: -f1)"
if [ "$runtime_gate_line" -gt "$last_package_mutation_line" ] \
   && [ "$runtime_gate_line" -lt "$rootfs_end_line" ]; then
  ok 'residual OpenSSL ABI gate runs after the final rootfs package mutation'
else
  bad 'residual OpenSSL ABI gate does not inspect the finished rootfs'
fi

cat >"$work/rpm" <<'EOF'
#!/usr/bin/env bash
[[ $1 == -qa && $2 == --qf ]] || exit 2
printf '%b' "${TILO_TEST_RPM_ROWS:-}"
EOF
chmod 0755 "$work/rpm"
if TILO_RPM_BIN="$work/rpm" \
   TILO_TEST_RPM_ROWS=$'cryptsetup-libs\tlibcrypto.so.3()(64bit)\npython3-libs\tlibssl.so.3()(64bit)\n' \
   bash "$runtime_package_gate" >/dev/null; then
  ok 'reviewed residual OpenSSL ABI packages pass the finished-rootfs gate'
else
  bad 'finished-rootfs gate rejects its reviewed residual OpenSSL ABI boundary'
fi
if TILO_RPM_BIN="$work/rpm" \
   TILO_TEST_RPM_ROWS=$'attacker-tls\tlibssl.so.3()(64bit)\n' \
   bash "$runtime_package_gate" >/dev/null 2>&1; then
  bad 'finished-rootfs gate accepts an unreviewed libssl consumer'
else
  ok 'finished-rootfs gate rejects an unreviewed libssl consumer'
fi
if TILO_RPM_BIN="$work/rpm" \
   TILO_TEST_RPM_ROWS=$'attacker-crypto\tlibcrypto.so.3()(64bit)\n' \
   bash "$runtime_package_gate" >/dev/null 2>&1; then
  bad 'finished-rootfs gate accepts an unreviewed libcrypto consumer'
else
  ok 'finished-rootfs gate rejects an unreviewed libcrypto consumer'
fi
if TILO_RPM_BIN="$work/rpm" \
   TILO_TEST_RPM_ROWS=$'openssl\topenssl-libs = 3.0\n' \
   bash "$runtime_package_gate" >/dev/null 2>&1; then
  bad 'finished-rootfs gate accepts a forbidden package without a direct libssl requirement'
else
  ok 'finished-rootfs gate rejects forbidden packages reintroduced indirectly'
fi

exit "$fail"
