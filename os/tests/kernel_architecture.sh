#!/usr/bin/env bash
# Exercise the production kernel build helper with resolved-config and build
# output fixtures. No kernel compilation, privileges, or container daemon needed.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
helper="$repo/os/kernel/build-support.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

fail() { printf 'FAIL  %s\n' "$*" >&2; exit 1; }
reject() {
    if "$@" > "$tmp/rejected.log" 2>&1; then
        fail "unexpected success: $*"
    fi
}

# A resolved ARM64 config has no x86 CET/IBT symbols. This fixture previously
# failed the Containerfile's unconditional x86 assertions.
cat > "$tmp/shared.config" <<'EOF'
CONFIG_MODULE_SIG=y
# CONFIG_MODULE_SIG_ALL is not set
CONFIG_MODULE_SIG_FORCE=y
CONFIG_USER_NS=y
CONFIG_SECCOMP_FILTER=y
CONFIG_SECURITY_LANDLOCK=y
CONFIG_SECURITY_YAMA=y
CONFIG_SECURITY_LOCKDOWN_LSM=y
CONFIG_STRICT_KERNEL_RWX=y
CONFIG_STACKPROTECTOR_STRONG=y
CONFIG_FORTIFY_SOURCE=y
CONFIG_HARDENED_USERCOPY=y
CONFIG_VMAP_STACK=y
CONFIG_SLAB_FREELIST_HARDENED=y
CONFIG_RANDOMIZE_KSTACK_OFFSET=y
CONFIG_LIST_HARDENED=y
EOF
for arch in x86_64 aarch64; do
    cp "$tmp/shared.config" "$tmp/$arch.config"
    if [ "$arch" = x86_64 ]; then
        printf '%s\n' CONFIG_X86_64=y CONFIG_X86_USER_SHADOW_STACK=y CONFIG_X86_KERNEL_IBT=y >> "$tmp/$arch.config"
    else
        printf '%s\n' CONFIG_ARM64=y >> "$tmp/$arch.config"
    fi
    sh "$helper" check-config "$arch" "$tmp/$arch.config" > "$tmp/check.log"
    # Removing any shared protection must still fail on each architecture.
    while IFS= read -r option; do
        grep -Fvx "$option" "$tmp/$arch.config" > "$tmp/missing.config"
        reject sh "$helper" check-config "$arch" "$tmp/missing.config"
        grep -Fq "MISSING $option" "$tmp/rejected.log" || fail "missing-option diagnostic: $option"
    done < "$tmp/shared.config"
done
for option in CONFIG_X86_USER_SHADOW_STACK=y CONFIG_X86_KERNEL_IBT=y; do
    grep -Fvx "$option" "$tmp/x86_64.config" > "$tmp/missing.config"
    reject sh "$helper" check-config x86_64 "$tmp/missing.config"
    grep -Fqx "$option" "$repo/os/kernel/config-fragments/x86_64.config" || fail "x86 fragment lost $option"
done
reject sh "$helper" check-config x86_64 "$tmp/aarch64.config"
reject sh "$helper" check-config aarch64 "$tmp/x86_64.config"
reject sh "$helper" check-config aarch64 "$tmp/absent.config"
if grep -Eq '^CONFIG_(X86_|LEGACY_VSYSCALL_)' "$repo/os/kernel/config-fragments/tilo.config"; then
    fail 'x86-only options leaked into the shared fragment'
fi
printf 'ok    architecture-specific config checks retain every shared protection\n'

mkdir -p "$tmp/source/arch/x86/boot" "$tmp/source/arch/arm64/boot" "$tmp/bin"
printf 'x86 kernel\n' > "$tmp/source/arch/x86/boot/bzImage"
printf 'ARM64 kernel\n' > "$tmp/source/arch/arm64/boot/Image"
printf 'symbols\n' > "$tmp/source/System.map"
cp "$tmp/aarch64.config" "$tmp/source/.config"
export MAKE_LOG="$tmp/make.log"
cat > "$tmp/bin/make" <<'EOF'
#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$MAKE_LOG"
if [ "${FAIL_DTBS:-0}" = 1 ]; then exit 1; fi
if [ "$1" = dtbs ]; then exit 0; fi
case "$1" in INSTALL_DTBS_PATH=*) output=${1#*=} ;; *) exit 2 ;; esac
[ "$2" = dtbs_install ]
mkdir -p "$output/qcom"
if [ "${EMPTY_DTBS:-0}" != 1 ]; then
    printf 'board device tree\n' > "$output/qcom/x1e80100-lenovo-yoga-slim7x.dtb"
fi
EOF
chmod +x "$tmp/bin/make"
export PATH="$tmp/bin:$PATH"

sh "$helper" stage-boot x86_64 "$tmp/source" "$tmp/x86-stage" 6.test
cmp "$tmp/source/arch/x86/boot/bzImage" "$tmp/x86-stage/lib/modules/6.test/vmlinuz"
[ ! -e "$MAKE_LOG" ] || fail 'x86 staging invoked ARM DTB build'
sh "$helper" stage-boot aarch64 "$tmp/source" "$tmp/arm-stage" 6.test
boot="$tmp/arm-stage/lib/modules/6.test"
cmp "$tmp/source/arch/arm64/boot/Image" "$boot/vmlinuz"
cmp "$tmp/source/System.map" "$boot/System.map"
cmp "$tmp/source/.config" "$boot/config"
grep -Fxq dtbs "$MAKE_LOG" || fail 'ARM DTBs were not built'
grep -Fxq "INSTALL_DTBS_PATH=$boot/dtb dtbs_install" "$MAKE_LOG" || fail 'ARM DTB install path changed'
grep -Fxq 'board device tree' "$boot/dtb/qcom/x1e80100-lenovo-yoga-slim7x.dtb" || fail 'board DTB missing'
reject env EMPTY_DTBS=1 sh "$helper" stage-boot aarch64 "$tmp/source" "$tmp/empty-stage" 6.test
reject env FAIL_DTBS=1 sh "$helper" stage-boot aarch64 "$tmp/source" "$tmp/failed-stage" 6.test
rm "$tmp/source/arch/arm64/boot/Image"
reject sh "$helper" stage-boot aarch64 "$tmp/source" "$tmp/missing-image-stage" 6.test
reject sh "$helper" stage-boot aarch64 "$tmp/source" relative-stage 6.test
for command in validate-arch check-config stage-boot; do
    reject sh "$helper" "$command" riscv64 "$tmp/source" "$tmp/unknown-stage" 6.test
    grep -Fq 'unsupported kernel architecture: riscv64' "$tmp/rejected.log" || fail 'unsupported architecture did not fail closed'
done
[ ! -e "$tmp/unknown-stage" ] || fail 'unsupported architecture created a payload'
printf 'ok    kernel images and DTBs staged by architecture; missing artifacts fail\n'

# Verify the Containerfile actually uses the executable path under test, that
# native RPM metadata is not forced to x86, and that the DTB tree is packaged.
tr -d '\r' < "$repo/os/kernel/Containerfile" > "$tmp/Containerfile"
for contract in \
    'COPY os/kernel/build-support.sh /work/kernel-build-support.sh' \
    'COPY os/kernel/config-fragments/ /work/kernel-config/' \
    'sh /work/kernel-build-support.sh validate-arch "$ARCH"' \
    'sh /work/kernel-build-support.sh prepare-source "$ARCH" "$KSRC"' \
    'sh /work/kernel-build-support.sh compile "$(uname -m)" "$KSRC" "$JOBS"' \
    'sh /work/kernel-build-support.sh stage-modules "$(uname -m)" "$KSRC" "$STAGE" "$KVER"'; do
    grep -Fq "$contract" "$tmp/Containerfile" || fail "kernel build bypassed tested helper: $contract"
done
# The helper's own steps: the native fragment, the resolved-config checks and
# the boot staging all sit behind the commands above.
for contract in \
    '"/work/kernel-config/$arch.config"' \
    'sh "$self" check-config "$arch" .config' \
    'sh "$self" stage-boot "$arch" "$ksrc" "$stage" "$kver"'; do
    grep -Fq "$contract" "$helper" || fail "kernel build helper lost a step: $contract"
done
spec="$repo/os/kernel/tilo-kernel-payload.spec"
if grep -Eq '^BuildArch:' "$spec"; then fail 'RPM overrides the native target architecture'; fi
grep -Eq '^ExclusiveArch:[[:space:]]+x86_64 aarch64[[:space:]]*$' "$spec" || fail 'RPM supported architectures changed'
grep -Eq '^/lib/modules/%\{kver\}[[:space:]]*$' "$spec" || fail 'RPM does not package the complete boot artifact tree'
printf 'ok    Containerfile and native RPM payload use the tested architecture contracts\n'
