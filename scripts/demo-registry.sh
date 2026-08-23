#!/usr/bin/env bash
# A local app registry for the "search → the app is not here → it opens"
# palette flow (plan/yc-demo.md, beat 3; shell/src/shell/catalogue.rs).
#
#   scripts/demo-registry.sh up      # build, pack, sign, configure, serve (foreground)
#   scripts/demo-registry.sh down    # restore registries.toml, stop a background server
#   scripts/demo-registry.sh reset   # forget the installed word processor so the beat repeats
#
# What `up` does, all in userspace, nothing on the host:
#   1. builds qos-pkg, qos-install, qos-launcher, qos-catalogue and the word processor;
#   2. mints a publisher identity (once) with `qos-pkg keygen`;
#   3. installs the signed runtime bundle the manifest pins, from the host
#      root, once — the launcher refuses to start an app without it;
#   4. packs apps/word-processor into a .qpkg and signs a registry entry
#      for it with `qos-pkg entry`;
#   5. points the Store's registries.toml (which the shell's palette also
#      reads) at http://127.0.0.1:$PORT, keeping a backup;
#   6. makes sure the word processor is NOT installed, so the palette's
#      "Open Word Processor" row is the catalogue's, not the registry's;
#   7. serves the directory with python's http.server: GET /entries is the
#      signed catalogue, GET /word-processor.qpkg is the package.
#
# Then start the shell from the same tree (it finds qos-install and
# qos-launcher beside its own binary):
#
#   cargo run --profile demo -p shell -- --enable-peer-sync
#
# from the same profile as the packages above — the shell finds qos-install
# and qos-launcher beside its own binary, so a `debug` shell will not see a
# `demo` toolchain.
#
# Super+Space, type "word", Enter — the row reads "Open Word Processor" like
# any installed app; the fetch happens behind it.
set -euo pipefail

REPO="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
# The profile decides what a viewer waits through, so the default is an
# optimised one. A `debug` word processor is 449 MB against 24 MB built
# here, and the packaging then spends its time moving and BLAKE3-hashing
# eighteen times the bytes it needs to: measured keystroke-to-first-frame
# is ~1.9 s optimised and 6.7-21 s debug, for the same mechanism.
#
# `demo` rather than `release` because the shell has to run from the same
# profile — it resolves qos-install and qos-launcher beside its own binary
# — and `target/demo/shell` is the one that keeps the QOS_TEST_CONTROL
# socket a `--release` build deliberately omits. For every package built
# here `demo` IS the release configuration; it overrides one thing,
# `debug-assertions` for `shell`.
PROFILE="${QOS_DEMO_PROFILE:-demo}"
BIN="$REPO/target/$PROFILE"
case "$PROFILE" in
    debug)   CARGO_PROFILE_FLAG="" ;;
    release) CARGO_PROFILE_FLAG="--release" ;;
    *)       CARGO_PROFILE_FLAG="--profile $PROFILE" ;;
esac
OUT="${QOS_DEMO_REGISTRY_DIR:-$REPO/target/demo-registry}"
PORT="${QOS_DEMO_REGISTRY_PORT:-8765}"
BASE_URL="http://127.0.0.1:$PORT"
STORE_CFG="${QOS_STORE_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/qos-store}"
REGISTRIES="$STORE_CFG/registries.toml"
BACKUP="$REGISTRIES.pre-demo"
IDENTITY="$OUT/publisher-identity.json"
RUNTIME_VERSION="$(sed -n 's/^runtime *= *"qos-runtime\/\(.*\)"/\1/p' "$REPO/apps/word-processor/qos-app.toml")"
PKG_ID="dev.qos.word-processor"
PIDFILE="$OUT/http.pid"

say() { printf '\033[1m[demo-registry]\033[0m %s\n' "$*"; }
die() { echo "FATAL: $*" >&2; exit 1; }

build() {
    say "building qos-pkg, qos-install, qos-launcher, qos-catalogue, word-processor"
    (cd "$REPO" && cargo build $CARGO_PROFILE_FLAG -p pkg-cli -p install -p launcher -p app-catalogue -p word-processor)
    for b in qos-pkg qos-install qos-launcher qos-catalogue word-processor; do
        [ -x "$BIN/$b" ] || die "$BIN/$b missing after build"
    done
}

identity() {
    mkdir -p "$OUT"
    if [ ! -f "$IDENTITY" ]; then
        say "minting publisher identity at $IDENTITY"
        "$BIN/qos-pkg" keygen --out "$IDENTITY" >/dev/null
    fi
}

runtime() {
    [ -n "$RUNTIME_VERSION" ] || die "could not read the runtime version from the manifest"
    if "$BIN/qos-install" runtime list 2>/dev/null | grep -q "$RUNTIME_VERSION"; then
        say "runtime bundle qos-runtime/$RUNTIME_VERSION already installed"
        return
    fi
    say "installing runtime bundle qos-runtime/$RUNTIME_VERSION from the host root (one-time)"
    "$BIN/qos-install" runtime install --version "$RUNTIME_VERSION" --from-host --identity "$IDENTITY"
}

pack() {
    say "packing $PKG_ID"
    local payload="$OUT/payload/word-processor"
    rm -rf "$payload"; mkdir -p "$payload/bin"
    cp "$BIN/word-processor" "$payload/bin/word-processor"
    "$BIN/qos-pkg" pack \
        --manifest "$REPO/apps/word-processor/qos-app.toml" \
        --payload "$payload" \
        --identity "$IDENTITY" \
        --output "$OUT/word-processor.qpkg"
    say "signing the registry entry"
    "$BIN/qos-pkg" entry \
        --url "$BASE_URL/word-processor.qpkg" \
        --identity "$IDENTITY" \
        "$OUT/word-processor.qpkg" > "$OUT/entry.json"
    # `/entries` is a bare array — one of the two shapes every client accepts.
    python3 - "$OUT/entry.json" "$OUT/entries" <<'PY'
import json, sys
entry = json.load(open(sys.argv[1]))
json.dump([entry], open(sys.argv[2], "w"), indent=2)
PY
    say "catalogue: $OUT/entries"
}

configure() {
    mkdir -p "$STORE_CFG"
    if [ -f "$REGISTRIES" ] && [ ! -f "$BACKUP" ]; then
        cp "$REGISTRIES" "$BACKUP"
        say "backed up $REGISTRIES -> $BACKUP"
    fi
    cat > "$REGISTRIES" <<EOF
# Written by scripts/demo-registry.sh — \`demo-registry.sh down\` restores the backup.
[[registry]]
name = "demo"
base_url = "$BASE_URL"
owner_pubkey_hex = "$(printf '00%.0s' $(seq 32))"
enabled = true
EOF
    say "registries.toml -> $BASE_URL"
}

forget_app() {
    if "$BIN/qos-install" list 2>/dev/null | grep -q "$PKG_ID"; then
        say "removing installed $PKG_ID so the palette offers to install it"
        "$BIN/qos-install" remove "$PKG_ID" || true
    fi
}

serve() {
    say "serving $OUT on $BASE_URL (Ctrl-C to stop)"
    say "  GET $BASE_URL/entries"
    say "  GET $BASE_URL/word-processor.qpkg"
    cd "$OUT"
    if [ "${QOS_DEMO_REGISTRY_BACKGROUND:-0}" = "1" ]; then
        python3 -m http.server "$PORT" --bind 127.0.0.1 >"$OUT/http.log" 2>&1 &
        echo $! > "$PIDFILE"
        say "background server pid $(cat "$PIDFILE")"
    else
        exec python3 -m http.server "$PORT" --bind 127.0.0.1
    fi
}

down() {
    if [ -f "$PIDFILE" ]; then
        kill "$(cat "$PIDFILE")" 2>/dev/null || true
        rm -f "$PIDFILE"
        say "stopped background server"
    fi
    if [ -f "$BACKUP" ]; then
        mv "$BACKUP" "$REGISTRIES"
        say "restored $REGISTRIES"
    elif [ -f "$REGISTRIES" ] && grep -q 'demo-registry.sh' "$REGISTRIES"; then
        rm -f "$REGISTRIES"
        say "removed the demo registries.toml (there was none before)"
    fi
}

case "${1:-}" in
    up)    build; identity; runtime; pack; configure; forget_app; serve ;;
    down)  down ;;
    reset) forget_app ;;
    *)     echo "Usage: $0 {up|down|reset}" >&2; exit 2 ;;
esac
