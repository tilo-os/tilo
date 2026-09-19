#!/usr/bin/env bash
# Verify the exact native builder's opt-in linker, runtime, and ELF hardening.
set -euo pipefail

arch=$(uname -m)
case "$arch" in
    x86_64)
        cpu=x86-64-v3
        architecture_flags=(-C link-arg=-Wl,-z,shstk)
        ;;
    aarch64)
        cpu=generic
        architecture_flags=()
        ;;
    *)
        printf 'unsupported native architecture: %s\n' "$arch" >&2
        exit 2
        ;;
esac

linker=${TILO_CI_MOLD_LINKER:-/usr/local/bin/tilo-ci-mold}
test -x "$linker"
export TILO_CI_MOLD_TARGET="$arch-unknown-linux-gnu"
export TILO_CI_MOLD_THREADS=2

rustc -Vv
rpm -q --qf '%{NEVRA}\n' mold
mold --version

work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
cat >"$work/smoke.rs" <<'RS'
fn main() {
    let n: usize = std::env::args().nth(1).unwrap().parse().unwrap();
    println!("{}", (0..n).map(|x| x.wrapping_mul(3)).sum::<usize>());
}
RS

for profile in dev thin; do
    profile_flags=(-C debuginfo=2)
    if [[ $profile == thin ]]; then
        profile_flags=(-C opt-level=2 -C lto=thin -C codegen-units=4)
    fi
    /usr/bin/time -f "mold_${profile}_wall_s=%e mold_${profile}_peak_rss_kib=%M" \
        rustc "$work/smoke.rs" --edition 2024 -o "$work/$profile" \
        -C "linker=$linker" -C "target-cpu=$cpu" \
        -C force-frame-pointers=yes -C relro-level=full \
        -C link-arg=-Wl,-z,noexecstack \
        "${architecture_flags[@]}" "${profile_flags[@]}"
    [[ $("$work/$profile" 1000) == 1498500 ]]
    readelf -p .comment "$work/$profile" | grep -F 'mold '
    readelf -lW "$work/$profile" | grep -F GNU_RELRO
    readelf -lW "$work/$profile" | grep -E 'GNU_STACK.* RW +0x'
    readelf -dW "$work/$profile" | grep -F BIND_NOW
    readelf -dW "$work/$profile" | grep -E 'Flags:.*PIE'
    if [[ $arch == x86_64 ]]; then
        readelf -n "$work/$profile" | grep -F SHSTK
    fi
done

printf 'native mold smoke passed on %s\n' "$arch"
