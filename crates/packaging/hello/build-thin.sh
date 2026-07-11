#!/usr/bin/env bash
# Thin-app proof: build `qos-hello` linking the shared qOS runtime as a
# dynamic library instead of statically baking the whole SDK into the
# binary. Prints the before/after sizes and runs the relinked app.
#
# How it works:
#   * The shared SDK crate `crates/runtime/runtime` is built
#     `crate-type = ["lib", "dylib"]`, so it produces `libruntime.so`
#     (which statically absorbs the runtime sub-crates + their deps).
#   * `hello` is built with `-C prefer-dynamic`, so it links libstd and
#     libruntime dynamically. Its own artifact then holds only its code
#     + dynamic references — the shared SDK ships once, not per app.
#   * A Rust `dylib` cannot be linked into an app under `panic = "abort"`
#     (dylibs unwind) nor under fat LTO (`prefer-dynamic` + LTO is
#     rejected), so the *thin* build overrides `panic = "unwind"` +
#     `lto = false`. All three (dylib crate-type + those two profile
#     flags) are required together and are incompatible with the
#     default release profile — so this script applies them ONLY for
#     the thin build and restores the tree afterwards. It does NOT
#     commit any of them: a normal `cargo build` of the workspace is
#     unchanged.
#
# In the OS image the launcher would bind-mount a content-addressed
# bundle of { libstd, libruntime } into the sandbox; here we just point
# LD_LIBRARY_PATH at the build outputs to prove the app runs.
set -euo pipefail

WT=$(cd "$(dirname "$0")/../../.." && pwd)
RT_TOML="$WT/crates/runtime/runtime/Cargo.toml"
SCCACHE_DIR=${SCCACHE_DIR:-/var/home/quinten/Development/quickdesktop/.claude/qos-sccache}
COMMON_FLAGS='-C target-cpu=x86-64-v3 -C force-frame-pointers=yes'

# Always leave the tree exactly as we found it.
cleanup() { git -C "$WT" checkout -- "$RT_TOML" 2>/dev/null || true; }
trap cleanup EXIT

run() { distrobox enter my-distrobox -- bash -c "cd '$WT' && $*"; }
hr() { printf '\n─── %s ───\n' "$*"; }

hr "1. baseline: static release (whole SDK baked into the app)"
run "CARGO_TARGET_DIR='$WT/target' SCCACHE_DIR='$SCCACHE_DIR' RUSTC_WRAPPER=sccache \
     cargo build -p hello --release >/dev/null 2>&1"
cp "$WT/target/release/qos-hello" "$WT/target/qos-hello.static-baseline"
BASELINE=$(stat -c%s "$WT/target/qos-hello.static-baseline")
echo "  qos-hello (static):        $BASELINE bytes"

hr "2. thin: runtime as dylib + prefer-dynamic (panic=unwind, lto=off)"
# Add the dylib crate-type to the runtime SDK for this build only.
if ! grep -q 'crate-type' "$RT_TOML"; then
  sed -i 's|^path = "src/lib.rs"|path = "src/lib.rs"\ncrate-type = ["lib", "dylib"]|' "$RT_TOML"
fi
# A stale static hello/libruntime must not be reused.
rm -f "$WT/target/release/qos-hello" "$WT/target/release/libruntime.so"
run "CARGO_TARGET_DIR='$WT/target' SCCACHE_DIR='$SCCACHE_DIR' RUSTC_WRAPPER=sccache \
     RUSTFLAGS='$COMMON_FLAGS -C prefer-dynamic' \
     cargo build -p hello --release \
       --config 'profile.release.panic=\"unwind\"' \
       --config 'profile.release.lto=false' 2>&1 | tail -3"
THIN=$(stat -c%s "$WT/target/release/qos-hello")
LIBRT=$(find "$WT/target/release" -maxdepth 2 -name 'libruntime.so' | head -1)
LIBRT_SIZE=$(stat -c%s "$LIBRT")
echo "  qos-hello (thin):          $THIN bytes"
echo "  libruntime.so (shared):    $LIBRT_SIZE bytes"

hr "3. prove the thin app still runs against the shared dylibs"
run "SR=\$(rustc --print sysroot) && \
     LP=\"$WT/target/release:$WT/target/release/deps:\$SR/lib/rustlib/x86_64-unknown-linux-gnu/lib\" && \
     echo '--- ldd ---' && LD_LIBRARY_PATH=\$LP ldd '$WT/target/release/qos-hello' | grep -Ei 'runtime|libstd' && \
     echo '--- run ---' && LD_LIBRARY_PATH=\$LP '$WT/target/release/qos-hello' | sed -n '1,3p'"

hr "result"
awk -v b="$BASELINE" -v t="$THIN" 'BEGIN{
  printf "  app binary: %d → %d bytes (%.1f%% smaller)\n", b, t, (1-t/b)*100;
  printf "  under 2 MB: %s\n", (t < 2*1024*1024 ? "YES" : "NO");
}'
