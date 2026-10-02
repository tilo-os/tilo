#!/usr/bin/env bash
# Exercise format-independent installed-layout verifiers without compiling Tilo.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
work=$(mktemp -d)
cleanup() { rm -rf -- "$work"; }
trap cleanup EXIT
true_bin=$(type -P true)
native_arch=$(uname -m)
case "$native_arch" in
    x86_64) package_arch=amd64; other_arch=arm64 ;;
    aarch64) package_arch=arm64; other_arch=amd64 ;;
    *) echo "unsupported fixture architecture: $native_arch" >&2; exit 1 ;;
esac

# Minimal distro images need not install Git merely to make the reusable OS
# payload reproducible. The outer runner resolves both source values and passes
# them through the packaging boundary.
package_job="$repo_root/dist/ci/run-package-job.sh"
python3 "$repo_root/dist/ci/tests/test_package_source_transfer.py"
grep -Fq -- '--env SOURCE_DATE_EPOCH="$source_epoch"' "$package_job"
grep -Fq -- '--env GITHUB_SHA="$source_revision"' "$package_job"
test "$(grep -Fc 'source tools/ci/sccache.sh prepare-shell' "$package_job")" -eq 3
if grep -Fq 'export RUSTC_WRAPPER=/usr/local/bin/sccache' "$package_job"; then
    echo "package lanes bypass the shared fail-open compiler-cache probe" >&2
    exit 1
fi
if sed -n '/^fedora-44)/,/^    ;;/p' "$package_job" | grep -Eq '\bgit[[:space:]]'; then
    echo "Fedora payload assembly still requires Git inside the target image" >&2
    exit 1
fi

"$repo_root/dist/session/tests/host-session-lifecycle.sh"
python3 "$repo_root/dist/session/tests/flight-recorder.py"
python3 "$repo_root/dist/session/tests/session-report.py"
"$repo_root/dist/session/tests/private-dbus-handoff.sh"
"$repo_root/dist/session/tests/private-user-manager-handoff.sh"
"$repo_root/dist/rpm/test-signing-contract.sh"
"$repo_root/dist/rpm/test-app-store-stage.sh"
"$repo_root/dist/rpm/test-build-target-dir.sh"
"$repo_root/dist/rpm/test-host-update.sh"

# A production import entry must name an installed native executable. This
# fails before the import payload is added, and must remain in every builder.
for builder in dist/build-binaries.sh dist/rpm/build-rpm.sh dist/arch/PKGBUILD; do
    grep -Eq 'cargo build .*-p imager.*--bin imager' "$repo_root/$builder" || {
        echo "$builder does not build the delivered folder import UI" >&2
        exit 1
    }
done

# Host packages ship only the compositor from the shell crate. Keep every FHS
# builder explicit so adding another developer binary to that crate cannot
# multiply release build time and disk use again.
for builder in dist/build-binaries.sh dist/rpm/build-rpm.sh dist/arch/PKGBUILD; do
    grep -Eq 'cargo build .*(-p shell.*--bin shell|--bin shell.*-p shell)' \
        "$repo_root/$builder" || {
        echo "$builder builds every shell binary instead of only the packaged compositor" >&2
        exit 1
    }
done

# A local Atomic trial must install only the runtime RPM unless the developer
# entry is explicitly requested; a broad tilo-session*.rpm glob regresses that
# contract by silently layering tilo-session-devel too.
overlay_helper="$repo_root/dist/rpm/usroverlay-install.sh"
grep -Fq -- "-name 'tilo-session-[0-9]*.rpm'" "$overlay_helper"
grep -Fq -- 'include_devel=1' "$overlay_helper"
grep -Fq -- '--arm-devel' "$overlay_helper"
grep -Fq -- '--disarm-devel' "$overlay_helper"
if grep -Fq -- "-name 'tilo-session*.rpm'" "$overlay_helper"; then
    echo "usroverlay helper still selects the optional developer RPM by default" >&2
    exit 1
fi

fhs=$work/fhs
mkdir -p "$fhs"/{usr/bin,usr/libexec,usr/lib/systemd/user,etc/pam.d,usr/share/wayland-sessions,usr/share/applications,usr/share/xdg-desktop-portal/portals,usr/share/pipewire,usr/share/licenses/tilo-session}
for target in "$fhs/usr/bin/tilo-shell" "$fhs/usr/bin/tilo-launcher" "$fhs/usr/bin/tilo-import-folders" \
              "$fhs/usr/libexec/tilo-unlock-helper"; do
    install -m755 "$true_bin" "$target"
done
install -m644 "$repo_root/dist/session/tilo-import-folders.desktop" "$fhs/usr/share/applications/tilo-import-folders.desktop"
install -m755 "$repo_root/dist/session/tilo-session" "$fhs/usr/libexec/tilo-session"
install -m755 "$repo_root/dist/session/tilo-session-runner" "$fhs/usr/libexec/tilo-session-runner"
install -m755 "$repo_root/dist/session/tilo-flight-recorder.py" "$fhs/usr/libexec/tilo-flight-recorder"
install -m755 "$repo_root/dist/session/tilo-session-report.py" "$fhs/usr/libexec/tilo-session-report"
install -m755 "$repo_root/dist/session/tilo-session-dev" "$fhs/usr/libexec/tilo-session-dev"
install -m644 "$repo_root/dist/session/tilo.desktop" "$fhs/usr/share/wayland-sessions/tilo.desktop"
install -m644 "$repo_root/dist/session/tilo-app.desktop" "$fhs/usr/share/wayland-sessions/tilo-app.desktop"
install -m644 "$repo_root/os/etc/pam.d/tilo-unlock" "$fhs/etc/pam.d/tilo-unlock"
install -m644 "$repo_root/os/usr/share/xdg-desktop-portal/portals/tilo.portal" \
    "$fhs/usr/share/xdg-desktop-portal/portals/tilo.portal"
install -m644 "$repo_root/os/usr/share/xdg-desktop-portal/tilo-portals.conf" \
    "$fhs/usr/share/xdg-desktop-portal/tilo-portals.conf"
install -m644 "$repo_root/os/usr/share/pipewire/tilo-echo-cancel.conf" \
    "$fhs/usr/share/pipewire/tilo-echo-cancel.conf"
install -m644 "$repo_root/os/usr/lib/systemd/user/tilo-graphical-session.target" \
    "$fhs/usr/lib/systemd/user/tilo-graphical-session.target"
install -m644 "$repo_root/LICENSE" "$fhs/usr/share/licenses/tilo-session/LICENSE"
TILO_EXPECT_ARCH="$package_arch" "$repo_root/dist/verify-payload.sh" "$fhs"
if TILO_EXPECT_ARCH="$other_arch" "$repo_root/dist/verify-payload.sh" "$fhs" >/dev/null 2>&1; then
    echo "payload verifier accepted the other architecture's native binaries" >&2
    exit 1
fi
# Keep dependency inspection fail-closed: an unavailable/failing inspector is
# not evidence of a fully resolved payload. Exercise the production verifier
# against real child processes, while retaining real ELF architecture checks.
mkdir "$work/verifier-bin"
cat > "$work/verifier-bin/ldd" <<'EOF'
#!/usr/bin/env bash
case "$TILO_TEST_LDD_MODE" in
    error) echo 'fixture inspector failed' >&2; exit 7 ;;
    unavailable) exec /nonexistent/tilo-test-dependency-inspector ;;
    empty) exit 0 ;;
    missing) echo 'libfixture.so => not found'; exit 0 ;;
    missing-stderr) echo 'libfixture.so => not found' >&2; exit 0 ;;
    *) exit 99 ;;
esac
EOF
chmod +x "$work/verifier-bin/ldd"
for mode in error unavailable empty missing missing-stderr; do
    if PATH="$work/verifier-bin:$PATH" TILO_TEST_LDD_MODE="$mode" \
        TILO_EXPECT_ARCH="$package_arch" "$repo_root/dist/verify-payload.sh" "$fhs" \
        >"$work/verifier-$mode.log" 2>&1; then
        echo "payload verifier accepted $mode dependency inspection" >&2
        exit 1
    fi
    case "$mode" in
        error) grep -Fq 'fixture inspector failed' "$work/verifier-$mode.log" ;;
        unavailable) grep -Fq 'dependency inspection failed' "$work/verifier-$mode.log" ;;
        empty) grep -Fq 'empty dependency inspection' "$work/verifier-$mode.log" ;;
        missing*) grep -Fq 'unresolved shared library' "$work/verifier-$mode.log" ;;
    esac
done
# Both legitimate static forms bypass ldd only after real ELF inspection.
# Build minimal native executables; a missing compiler/inspector fails this
# contract rather than silently skipping an architecture's coverage.
# The import executable is also smoke-tested, so static fixtures must exit
# successfully without depending on libc or a dynamic loader.
cat > "$work/static.c" <<'EOF'
void _start(void) {
#if defined(__x86_64__)
    __asm__ volatile("xor %%edi, %%edi\n\tmov $60, %%eax\n\tsyscall"
                     ::: "rax", "rdi", "rcx", "r11", "memory");
#elif defined(__aarch64__)
    __asm__ volatile("mov x0, #0\n\tmov x8, #93\n\tsvc #0"
                     ::: "x0", "x8", "memory");
#else
#error unsupported fixture architecture
#endif
    __builtin_unreachable();
}
EOF
for link_mode in -static -static-pie; do
    cc -nostdlib "$link_mode" -Wl,-e,_start "$work/static.c" -o "$work/static"
    for target in "$fhs/usr/bin/tilo-shell" "$fhs/usr/bin/tilo-launcher" "$fhs/usr/bin/tilo-import-folders" \
                  "$fhs/usr/libexec/tilo-unlock-helper"; do
        install -m755 "$work/static" "$target"
    done
    PATH="$work/verifier-bin:$PATH" TILO_TEST_LDD_MODE=error \
        TILO_EXPECT_ARCH="$package_arch" "$repo_root/dist/verify-payload.sh" "$fhs"
    if TILO_EXPECT_ARCH="$other_arch" "$repo_root/dist/verify-payload.sh" "$fhs" >/dev/null 2>&1; then
        echo "payload verifier accepted the other architecture's static binaries" >&2
        exit 1
    fi
done
cat > "$work/verifier-bin/readelf" <<'EOF'
#!/usr/bin/env bash
echo 'fixture ELF inspector failed' >&2
exit 4
EOF
chmod +x "$work/verifier-bin/readelf"
if PATH="$work/verifier-bin:$PATH" TILO_TEST_LDD_MODE=error \
    TILO_EXPECT_ARCH="$package_arch" "$repo_root/dist/verify-payload.sh" "$fhs" \
    >"$work/verifier-readelf.log" 2>&1; then
    echo "payload verifier accepted unobservable static ELF headers" >&2
    exit 1
fi
grep -Fq 'ELF dependency inspection failed' "$work/verifier-readelf.log"
# A shared object or relocatable object is not a static executable merely
# because it has neither an interpreter nor shared-library dependencies.
for object_mode in -shared -c; do
    cc -nostdlib -fPIC "$object_mode" "$work/static.c" -o "$work/not-executable"
    install -m755 "$work/not-executable" "$fhs/usr/bin/tilo-shell"
    if TILO_EXPECT_ARCH="$package_arch" "$repo_root/dist/verify-payload.sh" "$fhs" >/dev/null 2>&1; then
        echo "payload verifier accepted $object_mode output as a static executable" >&2
        exit 1
    fi
done
for target in "$fhs/usr/bin/tilo-shell" "$fhs/usr/bin/tilo-launcher" "$fhs/usr/bin/tilo-import-folders" \
              "$fhs/usr/libexec/tilo-unlock-helper"; do
    install -m755 "$true_bin" "$target"
done
"$repo_root/dist/verify-devel-payload.sh" "$fhs"
# A tool failure cannot masquerade as a complete runtime dependency closure.
mkdir "$work/fake-tools"
printf '#!/bin/sh\nexit 42\n' > "$work/fake-tools/ldd"
chmod 755 "$work/fake-tools/ldd"
if PATH="$work/fake-tools:$PATH" TILO_EXPECT_ARCH="$package_arch" "$repo_root/dist/verify-payload.sh" "$fhs" >/dev/null 2>&1; then
    echo "payload verifier accepted a failed ldd invocation" >&2; exit 1
fi
printf '#!/bin/sh\necho "libmissing.so => not found"\n' > "$work/fake-tools/ldd"
if PATH="$work/fake-tools:$PATH" TILO_EXPECT_ARCH="$package_arch" "$repo_root/dist/verify-payload.sh" "$fhs" >/dev/null 2>&1; then
    echo "payload verifier accepted a missing shared library" >&2; exit 1
fi
mv "$fhs/usr/bin/tilo-import-folders" "$work/import-held"
if TILO_EXPECT_ARCH="$package_arch" "$repo_root/dist/verify-payload.sh" "$fhs" >/dev/null 2>&1; then
    echo "payload verifier accepted missing import executable" >&2; exit 1
fi
mv "$work/import-held" "$fhs/usr/bin/tilo-import-folders"
sed -i 's#^Exec=.*#Exec=/missing --import-folders#' "$fhs/usr/share/applications/tilo-import-folders.desktop"
if TILO_EXPECT_ARCH="$package_arch" "$repo_root/dist/verify-payload.sh" "$fhs" >/dev/null 2>&1; then
    echo "payload verifier accepted wrong import entry" >&2; exit 1
fi
install -m644 "$repo_root/dist/session/tilo-import-folders.desktop" "$fhs/usr/share/applications/tilo-import-folders.desktop"

sed -i 's#^TryExec=.*#TryExec=/wrong#' "$fhs/usr/share/wayland-sessions/tilo-app.desktop"
if "$repo_root/dist/verify-devel-payload.sh" "$fhs" >/dev/null 2>&1; then
    echo "developer payload verifier accepted a wrong TryExec" >&2
    exit 1
fi

nix=$work/nix-result
mkdir -p "$nix"/{bin,libexec,etc/pam.d,share/systemd/user,share/wayland-sessions,share/applications,share/xdg-desktop-portal/portals,share/pipewire,share/licenses/tilo-session}
for target in "$nix/bin/tilo-shell" "$nix/bin/tilo-launcher" "$nix/bin/tilo-import-folders" \
              "$nix/libexec/tilo-unlock-helper"; do
    install -m755 "$true_bin" "$target"
done
sed "s#/usr/bin/tilo-import-folders#$nix/bin/tilo-import-folders#g" "$repo_root/dist/session/tilo-import-folders.desktop" > "$nix/share/applications/tilo-import-folders.desktop"
install -m755 "$repo_root/dist/session/tilo-session" "$nix/libexec/tilo-session"
install -m755 "$repo_root/dist/session/tilo-session-runner" "$nix/libexec/tilo-session-runner"
install -m755 "$repo_root/dist/session/tilo-flight-recorder.py" "$nix/libexec/tilo-flight-recorder"
install -m755 "$repo_root/dist/session/tilo-session-report.py" "$nix/libexec/tilo-session-report"
install -m644 "$repo_root/dist/session/tilo.desktop" "$nix/share/wayland-sessions/tilo.desktop"
install -m644 "$repo_root/os/etc/pam.d/tilo-unlock" "$nix/etc/pam.d/tilo-unlock"
install -m644 "$repo_root/os/usr/share/xdg-desktop-portal/portals/tilo.portal" \
    "$nix/share/xdg-desktop-portal/portals/tilo.portal"
install -m644 "$repo_root/os/usr/share/xdg-desktop-portal/tilo-portals.conf" \
    "$nix/share/xdg-desktop-portal/tilo-portals.conf"
install -m644 "$repo_root/os/usr/share/pipewire/tilo-echo-cancel.conf" \
    "$nix/share/pipewire/tilo-echo-cancel.conf"
install -m644 "$repo_root/os/usr/lib/systemd/user/tilo-graphical-session.target" \
    "$nix/share/systemd/user/tilo-graphical-session.target"
install -m644 "$repo_root/LICENSE" "$nix/share/licenses/tilo-session/LICENSE"
sed -i \
    -e "s#TILO_SHELL=/usr/bin/tilo-shell#TILO_SHELL=$nix/bin/tilo-shell#" \
    -e "s#SESSION_RUNNER=/usr/libexec/tilo-session-runner#SESSION_RUNNER=$nix/libexec/tilo-session-runner#" \
    "$nix/libexec/tilo-session"
sed -i \
    -e 's#SYSTEMCTL=/usr/bin/systemctl#SYSTEMCTL=/nix/store/fixture-systemd/bin/systemctl#' \
    -e 's#SYSTEMD_CAT=/usr/bin/systemd-cat#SYSTEMD_CAT=/nix/store/fixture-systemd/bin/systemd-cat#' \
    -e 's#DBUS_UPDATE=/usr/bin/dbus-update-activation-environment#DBUS_UPDATE=/nix/store/fixture-dbus/bin/dbus-update-activation-environment#' \
    -e "s#FLIGHT_RECORDER=/usr/libexec/tilo-flight-recorder#FLIGHT_RECORDER=$nix/libexec/tilo-flight-recorder#" \
    "$nix/libexec/tilo-session-runner"
sed -i \
    -e "s#Exec=/usr/libexec/tilo-session#Exec=$nix/libexec/tilo-session#" \
    -e "s#TryExec=/usr/bin/tilo-shell#TryExec=$nix/bin/tilo-shell#" \
    "$nix/share/wayland-sessions/tilo.desktop"
"$repo_root/dist/ci/verify-nix-result.sh" "$nix" "$package_arch"
if "$repo_root/dist/ci/verify-nix-result.sh" "$nix" "$other_arch" >/dev/null 2>&1; then
    echo "Nix verifier accepted the other architecture's native binaries" >&2
    exit 1
fi

# The OS hand-off is deterministic and contains only the native desktop binaries
# produced by the distro package lane.
payload_bin=$work/payload-bin
mkdir -p "$payload_bin" "$work/payload-a" "$work/payload-b"
for binary in shell tilo-launcher tilo-runtime-mcp tilo-unlock-helper; do
    install -m755 "$true_bin" "$payload_bin/$binary"
done
SOURCE_DATE_EPOCH=123 GITHUB_SHA="$(printf 'a%.0s' {1..40})" \
    "$repo_root/dist/desktop/package-os-payload.sh" \
    "$payload_bin" "$native_arch" "$work/payload-a"
SOURCE_DATE_EPOCH=123 GITHUB_SHA="$(printf 'a%.0s' {1..40})" \
    "$repo_root/dist/desktop/package-os-payload.sh" \
    "$payload_bin" "$native_arch" "$work/payload-b"
cmp "$work/payload-a/tilo-desktop-os-$native_arch.tar.gz" \
    "$work/payload-b/tilo-desktop-os-$native_arch.tar.gz"
mkdir "$work/payload-unpacked"
tar -xzf "$work/payload-a/tilo-desktop-os-$native_arch.tar.gz" \
    -C "$work/payload-unpacked"
(cd "$work/payload-unpacked" && sha256sum --check --strict SHA256SUMS)

echo "payload contract fixtures passed"
