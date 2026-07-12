#!/usr/bin/env bash
# Thin-app proof — end to end, using only committed, non-breaking machinery.
#
# Ships a qOS app as a tiny binary that links the SDK + libstd as SHARED
# libraries out of a content-addressed runtime bundle, instead of baking
# the whole SDK into every binary. Prints before/after sizes and runs the
# relinked apps against the bundle.
#
# How it works (all committed — nothing is patched at build time):
#
#   * `crates/runtime/runtime-dylib` is a `crate-type = ["dylib"]` wrapper
#     over the `runtime` umbrella + `cli` helpers. It is a workspace member
#     but is kept OUT of `default-members`, so a normal `cargo build` never
#     compiles it. It is built only under `--profile thin-app`.
#
#   * `[profile.thin-app]` (root Cargo.toml) is the release profile with
#     the two settings dynamic linking demands: `panic = "unwind"` and
#     `lto = false`. Nothing builds under it unless `--profile thin-app`
#     is passed, so the default `panic = "abort"` release path is
#     untouched (verify: `cargo build --release -p hello` still works).
#
#   * The apps carry a `thin` feature that pulls `runtime-dylib` in and a
#     one-line `#[cfg(feature="thin")] extern crate runtime_dylib;` so the
#     linker keeps the dylib. Built with `-C prefer-dynamic`, rustc then
#     serves `runtime`/`cli` (and libstd) from the shared libs; the app
#     binary holds only its own code.
#
#   * `qos-runtime-bundle build --include <libstd> --include <libruntime>`
#     folds those two shared objects into the signed, content-addressed
#     bundle at `lib/`, alongside the host loader + libc set. The launcher
#     mounts the bundle and sets `LD_LIBRARY_PATH=<mount>/lib`, so a thin
#     app resolves everything by soname inside the sandbox. Here we point
#     `LD_LIBRARY_PATH` straight at the bundle to prove the apps run.
set -euo pipefail

WT=$(cd "$(dirname "$0")/../../.." && pwd)
SCCACHE_DIR=${SCCACHE_DIR:-/var/home/quinten/Development/quickdesktop/.claude/qos-sccache}
RUNTIME_VERSION=${RUNTIME_VERSION:-thin-1}
BUNDLE_ROOT="$WT/target/thin-bundle"
BUNDLE_DIR="$BUNDLE_ROOT/qos-runtime/$RUNTIME_VERSION"
IDENTITY_JSON="$BUNDLE_ROOT/identity.json"

run() { distrobox enter my-distrobox -- bash -c "cd '$WT' && $*"; }
hr() { printf '\n─── %s ───\n' "$*"; }
size() { stat -c%s "$1"; }
env_prefix="CARGO_TARGET_DIR='$WT/target' SCCACHE_DIR='$SCCACHE_DIR' RUSTC_WRAPPER=sccache"

hr "1. baseline: static release (whole SDK baked into each app)"
run "$env_prefix cargo build --release -p hello -p notes-cli >/dev/null 2>&1"
cp "$WT/target/release/qos-hello" "$WT/target/qos-hello.static-baseline"
cp "$WT/target/release/notes" "$WT/target/notes.static-baseline"
HELLO_STATIC=$(size "$WT/target/qos-hello.static-baseline")
NOTES_STATIC=$(size "$WT/target/notes.static-baseline")
printf '  qos-hello (static): %s bytes\n' "$HELLO_STATIC"
printf '  notes     (static): %s bytes\n' "$NOTES_STATIC"

hr "2. thin: runtime-dylib + prefer-dynamic (--profile thin-app, --features thin)"
run "$env_prefix RUSTFLAGS='-C prefer-dynamic' \
     cargo build --profile thin-app -p hello -p notes-cli --features thin 2>&1 | tail -2"
HELLO_THIN=$(size "$WT/target/thin-app/qos-hello")
NOTES_THIN=$(size "$WT/target/thin-app/notes")
LIBRT="$WT/target/thin-app/libruntime_dylib.so"
printf '  qos-hello (thin):        %s bytes\n' "$HELLO_THIN"
printf '  notes     (thin):        %s bytes\n' "$NOTES_THIN"
printf '  libruntime_dylib.so:     %s bytes (shared, ships once)\n' "$(size "$LIBRT")"

hr "3. build the signed {libstd, libruntime} runtime bundle"
run "$env_prefix cargo build --release -p runtime-bundle >/dev/null 2>&1"
BUNDLE_BIN="$WT/target/release/qos-runtime-bundle"
rm -rf "$BUNDLE_ROOT"
mkdir -p "$BUNDLE_ROOT"
run "'$BUNDLE_BIN' key generate --out '$IDENTITY_JSON' >/dev/null"
PUBKEY=$(run "'$BUNDLE_BIN' key pubkey --identity '$IDENTITY_JSON'")
# The exact libstd the thin apps linked (its hash is in their DT_NEEDED).
LIBSTD=$(run "ls \$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/lib/libstd-*.so | head -1")
run "'$BUNDLE_BIN' build --version '$RUNTIME_VERSION' --arch x86_64 --from / \
       --identity '$IDENTITY_JSON' \
       --include '$LIBSTD' --include '$LIBRT' \
       --out '$BUNDLE_DIR'"
run "'$BUNDLE_BIN' verify '$BUNDLE_DIR' --require-publisher '$PUBKEY' --require-signature"

hr "4. run the thin apps against the shared bundle"
run "LP='$BUNDLE_DIR/lib' && \
     echo '--- ldd notes (shared libs come from the bundle) ---' && \
     LD_LIBRARY_PATH=\$LP ldd '$WT/target/thin-app/notes' | grep -Ei 'runtime|libstd' && \
     echo '--- run qos-hello ---' && \
     LD_LIBRARY_PATH=\$LP '$WT/target/thin-app/qos-hello' | sed -n '1,2p' && \
     echo '--- run notes ---' && \
     LD_LIBRARY_PATH=\$LP '$WT/target/thin-app/notes' --version"

hr "result"
awk -v hs="$HELLO_STATIC" -v ht="$HELLO_THIN" -v ns="$NOTES_STATIC" -v nt="$NOTES_THIN" 'BEGIN{
  printf "  qos-hello: %d → %d bytes (%.1f%% smaller) — under 2 MB: %s\n", hs, ht, (1-ht/hs)*100, (ht < 2*1024*1024 ? "YES" : "NO");
  printf "  notes:     %d → %d bytes (%.1f%% smaller) — under 2 MB: %s\n", ns, nt, (1-nt/ns)*100, (nt < 2*1024*1024 ? "YES" : "NO");
}'
echo "  bundle: $BUNDLE_DIR (signed by ed25519:$PUBKEY)"
