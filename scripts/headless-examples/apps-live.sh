#!/usr/bin/env bash
# Assemble target/tilo-apps-live — an installed, signed word-processor
# payload that the install and installed-app headless proofs serve.
#
#   scripts/headless-examples/apps-live.sh            # optimised (profile: demo)
#   TILO_APPS_LIVE_PROFILE=debug scripts/headless-examples/apps-live.sh
#
# The size of one binary determines the fetch cost of "search, Enter,
# and it opens". The default profile is optimised: a `debug` word processor
# is ~449 MB, and moving and BLAKE3-hashing it accounts for most of the
# 16-21 s measured for that step.
#
# `[profile.demo]` inherits `release` and overrides exactly one thing —
# `debug-assertions` for the `shell` package, which this script does not
# build — so for every package built here it is the release configuration.
#
# This tree is a build artefact under `target/`, so it is not in git and
# a clean checkout has none of it.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PROFILE="${TILO_APPS_LIVE_PROFILE:-demo}"
BIN="$REPO/target/$PROFILE"
OUT="${TILO_APPS_LIVE_DIR:-$REPO/target/tilo-apps-live}"
IDENTITY="${TILO_APPS_LIVE_IDENTITY:-$OUT/publisher-identity.json}"
PKG_ID="so.tilo.word-processor"
STAGE="$OUT/.stage"

case "$PROFILE" in
    debug)   PROFILE_FLAG="" ;;
    release) PROFILE_FLAG="--release" ;;
    *)       PROFILE_FLAG="--profile $PROFILE" ;;
esac

say() { printf '\033[1m[apps-live]\033[0m %s\n' "$*"; }
die() { echo "FATAL: $*" >&2; exit 1; }

say "building word-processor, tilo-app-package, tilo-install (profile: $PROFILE)"
(cd "$REPO" && cargo build $PROFILE_FLAG -p word-processor -p tilo-app-package-cli -p install)
for b in word-processor tilo-app-package tilo-install; do
    [ -x "$BIN/$b" ] || die "$BIN/$b missing after build"
done

# Reject binaries older than their sources so measurements cannot use a
# stale, unreproducible payload.
newer="$(find "$REPO/apps/word-processor/src" "$REPO/crates/sdk/ui-core/src" -name '*.rs' \
         -newer "$BIN/word-processor" -print -quit 2>/dev/null || true)"
[ -z "$newer" ] || die "$BIN/word-processor is older than $newer — the build did not take"

mkdir -p "$OUT"
if [ ! -f "$IDENTITY" ]; then
    say "minting publisher identity at $IDENTITY"
    "$BIN/tilo-app-package" keygen --out "$IDENTITY" >/dev/null
fi

# Copy the binary so a later `cargo build` cannot change an existing payload.
say "staging the payload"
rm -rf "$STAGE"
mkdir -p "$STAGE/word-processor/bin"
cp "$BIN/word-processor" "$STAGE/word-processor/bin/word-processor"

say "packing $PKG_ID"
"$BIN/tilo-app-package" pack \
    --manifest "$REPO/apps/word-processor/tilo-app.toml" \
    --payload "$STAGE/word-processor" \
    --identity "$IDENTITY" \
    --output "$STAGE/word-processor.tapp"

# Install fresh rather than editing in place. `tilo-install verify` checks
# every payload file against the `package_id_hex` recorded in `registry.toml`.
say "installing into $OUT"
rm -rf "$OUT/payloads" "$OUT/registry.toml"
TILO_APPS_DATA_DIR="$OUT" "$BIN/tilo-install" install "$STAGE/word-processor.tapp" --force
TILO_APPS_DATA_DIR="$OUT" "$BIN/tilo-install" verify "$PKG_ID" \
    || die "the freshly installed package does not verify"

rm -rf "$STAGE"
payload="$OUT/payloads/$PKG_ID/bin/word-processor"
tapp_note=""
say "payload: $payload ($(numfmt --to=iec-i --suffix=B "$(stat -c%s "$payload")"))$tapp_note"
