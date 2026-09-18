#!/usr/bin/env bash
# Tiny actual native link/run probe; no Cargo workspace build or shared target.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
[[ $(uname -s) == Linux ]]
arch=$(uname -m)
case "$arch" in
    x86_64) cpu=x86-64-v3; extra=(-C link-arg=-Wl,-z,shstk) ;;
    aarch64) cpu=generic; extra=() ;;
    *) echo 'unsupported native architecture' >&2; exit 2 ;;
esac
export TILO_CI_MOLD_TARGET="$arch-unknown-linux-gnu"
export TILO_CI_MOLD_THREADS=2
rustc +1.97.1 -Vv
gcc --version | head -n 1
rpm -q --qf '%{NEVRA}\n' mold
mold --version
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
printf '%s\n' 'fn main() { let n: usize = std::env::args().nth(1).unwrap().parse().unwrap(); println!("{}", (0..n).map(|x| x.wrapping_mul(3)).sum::<usize>()); }' > "$work/smoke.rs"
for profile in dev thin; do
    options=(-C debuginfo=2)
    [[ $profile != thin ]] || options=(-C opt-level=2 -C lto=thin -C codegen-units=4)
    echo "MOLD_NATIVE_PROFILE=$profile"
    /usr/bin/time -f 'whole_compile_link_wall_s=%e peak_rss_kib=%M' \
        rustc +1.97.1 "$work/smoke.rs" --edition 2024 -o "$work/$profile" \
        -C "linker=$repo/tools/ci-native-mold.sh" -C "target-cpu=$cpu" \
        -C force-frame-pointers=yes -C relro-level=full \
        -C link-arg=-Wl,-z,noexecstack "${extra[@]}" "${options[@]}"
    [[ $("$work/$profile" 1000) == 1498500 ]]
    readelf -p .comment "$work/$profile" | grep 'mold '
    readelf -lW "$work/$profile" | grep GNU_RELRO
    readelf -lW "$work/$profile" | grep GNU_STACK | grep -E ' RW +0x'
    readelf -dW "$work/$profile" | grep BIND_NOW
    readelf -dW "$work/$profile" | grep 'Flags:.*PIE'
    readelf -dW "$work/$profile" | grep NEEDED
    if [[ $arch == x86_64 ]]; then
        readelf -n "$work/$profile" | grep SHSTK
    fi
done
echo MOLD_NATIVE_RUNTIME_SMOKE_PASS
