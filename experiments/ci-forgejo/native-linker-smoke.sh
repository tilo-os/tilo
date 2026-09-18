#!/usr/bin/env bash
set -euo pipefail
[[ $(uname -s) == Linux && $(uname -m) == aarch64 ]] || {
  echo 'This recorded smoke is for native ARM64 Linux only.' >&2; exit 2;
}
scratch=$(mktemp -d /tmp/qos-link-probe.XXXXXX)
trap 'rm -rf -- "$scratch"' EXIT
echo "scratch=$scratch"
cd "$scratch"
printf '%s\n' 'fn main() { let n: usize = std::env::args().nth(1).unwrap().parse().unwrap(); println!("{}", (0..n).map(|x| x.wrapping_mul(3)).sum::<usize>()); }' > smoke.rs
sysroot=$(rustc +1.97.1 --print sysroot)
for mode in bfd mold lld; do
  flags=()
  case "$mode" in
    bfd) flags=(-C linker=gcc -C link-arg=-fuse-ld=bfd);;
    mold) flags=(-C linker=gcc -C link-arg=-fuse-ld=mold -C link-arg=-Wl,--threads=2);;
    lld) flags=(-C linker=clang -C "link-arg=--ld-path=$sysroot/lib/rustlib/aarch64-unknown-linux-gnu/bin/gcc-ld/ld.lld" -C link-arg=-Wl,--threads=2);;
  esac
  for opt in dev thin; do
    profile=(-C debuginfo=2)
    if [ "$opt" = thin ]; then profile=(-C opt-level=2 -C lto=thin -C codegen-units=4); fi
    echo "=== $mode $opt ==="
    /usr/bin/time -f 'wall_s=%e peak_rss_kib=%M' rustc +1.97.1 smoke.rs --edition 2024 -o "$mode-$opt" -C target-cpu=generic -C force-frame-pointers=yes -C relro-level=full -C link-arg=-Wl,-z,noexecstack "${flags[@]}" "${profile[@]}"
    test "$(./"$mode-$opt" 1000)" = 1498500
    readelf -lW "$mode-$opt" | grep -E 'GNU_STACK|GNU_RELRO'
    readelf -dW "$mode-$opt" | grep -E 'BIND_NOW|FLAGS_1|NEEDED'
    readelf -p .comment "$mode-$opt" | tail -5
  done
done
