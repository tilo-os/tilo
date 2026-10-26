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

# The aarch64 kernel cross-compiles on x86_64: every make of the compile runs with Kbuild's ARCH
# and CROSS_COMPILE, the compiler cache wraps the cross compiler (and the builder's own for host
# programs), and a builder without the cross toolchain, or another pairing, fails before make.
mkdir -p "$tmp/cross/bin" "$tmp/cross/tree" "$tmp/cross/extmod" "$tmp/cross/cache"
export CROSS_LOG="$tmp/cross/make.log" CCACHE_LOG_FILE="$tmp/cross/ccache.log"
cat > "$tmp/cross/bin/uname" <<'EOF'
#!/bin/sh
[ "$1" = -m ] && { echo "${FAKE_HOST:-x86_64}"; exit 0; }
exec /usr/bin/uname "$@"
EOF
cat > "$tmp/cross/bin/make" <<'EOF'
#!/bin/sh
printf 'ARCH=%s CROSS_COMPILE=%s %s\n' "${ARCH:-}" "${CROSS_COMPILE:-}" "$*" >> "$CROSS_LOG"
case "$*" in *kernelrelease*) echo 6.test ;; esac
EOF
cat > "$tmp/cross/bin/ccache" <<'EOF'
#!/bin/sh
printf '%s CCACHE_DIR=%s CCACHE_MAXSIZE=%s\n' "$*" "${CCACHE_DIR:-}" "${CCACHE_MAXSIZE:-}" >> "$CCACHE_LOG_FILE"
EOF
printf '#!/bin/sh\n' > "$tmp/cross/bin/aarch64-linux-gnu-gcc"
chmod +x "$tmp/cross/bin/"*
cross_path="$tmp/cross/bin:$PATH"
PATH="$cross_path" sh "$helper" compile aarch64 "$tmp/cross/tree" 2 "$tmp/cross/extmod" > "$tmp/cross/out"
[ "$(tail -1 "$tmp/cross/out")" = 6.test ] || fail 'cross compile did not print the kernel release'
[ "$(grep -c . "$CROSS_LOG")" -eq 3 ] || fail "cross compile ran an unexpected make: $(cat "$CROSS_LOG")"
if grep -Fv 'ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu- ' "$CROSS_LOG" | grep -q .; then
    fail "a cross make ran without Kbuild's cross settings: $(cat "$CROSS_LOG")"
fi
: > "$CROSS_LOG"
PATH="$cross_path" TILO_KERNEL_COMPILER_CACHE="$tmp/cross/cache" \
    sh "$helper" compile aarch64 "$tmp/cross/tree" 2 "$tmp/cross/extmod" > /dev/null
grep -Fq 'CC=sh /work/kernel-cc-wrapper.sh aarch64-linux-gnu-gcc HOSTCC=sh /work/kernel-cc-wrapper.sh gcc all' "$CROSS_LOG" \
    || fail "the compiler cache does not wrap the cross compiler: $(cat "$CROSS_LOG")"
grep -Fxq -- "--zero-stats CCACHE_DIR=$tmp/cross/cache CCACHE_MAXSIZE=0" "$CCACHE_LOG_FILE" \
    || fail 'ccache was not pointed at the cache directory, unbounded'
grep -Fxq -- "--show-stats --verbose CCACHE_DIR=$tmp/cross/cache CCACHE_MAXSIZE=0" "$CCACHE_LOG_FILE" \
    || fail 'the build did not report what the cache served'
reject env PATH="$cross_path" TILO_KERNEL_COMPILER_CACHE="$tmp/cross/absent" \
    sh "$helper" compile aarch64 "$tmp/cross/tree" 2 "$tmp/cross/extmod"
grep -Fq 'no writable compiler cache directory' "$tmp/rejected.log" || fail 'a missing cache directory did not fail'
# A buildroot from before ccache joined it compiles without the cache. Only the fakes and sh
# are on PATH, so no ccache of the machine running this stands in.
mv "$tmp/cross/bin/ccache" "$tmp/cross/ccache.away"
mkdir -p "$tmp/cross/shell"
ln -s "$(command -v sh)" "$tmp/cross/shell/sh"
: > "$CROSS_LOG"
PATH="$tmp/cross/bin:$tmp/cross/shell" TILO_KERNEL_COMPILER_CACHE="$tmp/cross/cache" \
    "$tmp/cross/shell/sh" "$helper" compile aarch64 "$tmp/cross/tree" 2 "$tmp/cross/extmod" > /dev/null 2> "$tmp/cross/err"
grep -Fq 'has no ccache; compiling without the compiler cache' "$tmp/cross/err" || fail 'no ccache was not reported'
if grep -Fq 'kernel-cc-wrapper' "$CROSS_LOG"; then fail 'a build without ccache wrapped the compiler'; fi
mv "$tmp/cross/ccache.away" "$tmp/cross/bin/ccache"
: > "$CROSS_LOG"
PATH="$cross_path" FAKE_HOST=aarch64 sh "$helper" compile aarch64 "$tmp/cross/tree" 2 "$tmp/cross/extmod" > /dev/null
if grep -Fv 'ARCH= CROSS_COMPILE= ' "$CROSS_LOG" | grep -q .; then fail 'a native build set cross settings'; fi
rm "$tmp/cross/bin/aarch64-linux-gnu-gcc"
: > "$CROSS_LOG"
reject env PATH="$cross_path" sh "$helper" compile aarch64 "$tmp/cross/tree" 2 "$tmp/cross/extmod"
grep -Fq 'carries no aarch64 cross toolchain' "$tmp/rejected.log" || fail 'a missing cross toolchain did not fail'
reject env PATH="$cross_path" FAKE_HOST=aarch64 sh "$helper" compile x86_64 "$tmp/cross/tree" 2 "$tmp/cross/extmod"
grep -Fq 'the x86_64 kernel builds natively or on x86_64, not on aarch64' "$tmp/rejected.log" \
    || fail 'an x86_64 kernel built on aarch64'
[ ! -s "$CROSS_LOG" ] || fail "a refused cross build ran make: $(cat "$CROSS_LOG")"
printf 'ok    the aarch64 kernel cross-compiles on x86_64 with Kbuild cross settings and its cache\n'

# The cache wrapper compiles every source that pulls a file in with .incbin without the cache,
# since the cache keys the source text, not the file it includes; the rest go through it.
wrapper="$repo/os/kernel/cc-wrapper.sh"
mkdir -p "$tmp/ccw"
printf '#!/bin/sh\necho "cached $*"\n' > "$tmp/ccw/launcher"
printf '#!/bin/sh\necho "direct $*"\n' > "$tmp/ccw/cc"
chmod +x "$tmp/ccw/launcher" "$tmp/ccw/cc"
printf 'int f(void) { return 0; }\n' > "$tmp/ccw/plain.c"
printf 'asm(".incbin \\"kernel/config_data.gz\\"");\n' > "$tmp/ccw/configs.c"
printf '\t.incbin "arch/arm64/kernel/vdso/vdso.so"\n' > "$tmp/ccw/vdso-wrap.S"
ccw() { (cd "$tmp/ccw" && TILO_KERNEL_CC_LAUNCHER="$tmp/ccw/launcher" sh "$wrapper" "$tmp/ccw/cc" "$@"); }
[ "$(ccw -c -o plain.o plain.c)" = "cached $tmp/ccw/cc -c -o plain.o plain.c" ] || fail 'a plain source bypassed the cache'
[ "$(ccw -c -o configs.o configs.c)" = 'direct -c -o configs.o configs.c' ] || fail 'an inline .incbin went through the cache'
[ "$(ccw -c -o vdso-wrap.o vdso-wrap.S)" = 'direct -c -o vdso-wrap.o vdso-wrap.S' ] || fail 'an assembler .incbin went through the cache'
[ "$(ccw -E plain.c)" = 'direct -E plain.c' ] || fail 'preprocessing went through the cache'
printf 'ok    sources naming .incbin compile without the compiler cache\n'

# Verify the Containerfile actually uses the executable path under test, that
# native RPM metadata is not forced to x86, and that the DTB tree is packaged.
tr -d '\r' < "$repo/os/kernel/Containerfile" > "$tmp/Containerfile"
for contract in \
    'COPY os/kernel/build-support.sh /work/kernel-build-support.sh' \
    'COPY os/kernel/config-fragments/ /work/kernel-config/' \
    'sh /work/kernel-build-support.sh validate-arch "$ARCH"' \
    'sh /work/kernel-build-support.sh prepare-source "$ARCH" "$KSRC"' \
    'TARGET_ARCH="${KERNEL_ARCH:-$(uname -m)}"' \
    'sh /work/kernel-build-support.sh compile "$TARGET_ARCH" "$KSRC" "$JOBS"' \
    'sh /work/kernel-build-support.sh stage-modules "$TARGET_ARCH" "$KSRC" "$STAGE" "$KVER"' \
    'rpmbuild -bb --target "$TARGET_ARCH"'; do
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
