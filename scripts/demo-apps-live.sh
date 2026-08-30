#!/usr/bin/env bash
# Assemble target/qos-apps-live — the app payload the YC demo serves.
#
#   scripts/demo-apps-live.sh            # optimised (profile: demo)
#   QOS_DEMO_PROFILE=debug scripts/demo-apps-live.sh
#
# `scripts/yc-demo.py` reads this tree twice, and both readings are the
# app a viewer waits for:
#
#   * bob and the agent get it as an already-installed app, straight
#     out of `payloads/<id>/`;
#   * ada does NOT have it, and fetches it in beat 5 — that beat is
#     a `.qpkg` packed out of this same payload and served over
#     loopback by `serve-test-registry`.
#
# So the size of one binary here is the whole of what "search, Enter,
# and it opens" costs on camera. That is why the default profile is an
# optimised one and not `debug`: a `debug` word processor is ~449 MB,
# and moving and BLAKE3-hashing 449 MB is most of the 16-21 s that beat
# used to take. Nothing about the architecture required that wait —
# only the profile did.
#
# `demo` rather than `release` because that is what the compositor is
# built as and what `yc-demo.py` resolves first; for every package
# built here it IS the release configuration (opt-level 3, fat LTO,
# one codegen unit, `strip = "symbols"`). `[profile.demo]` inherits
# `release` and overrides exactly one thing — `debug-assertions` for
# the `shell` package, which this script does not build.
#
# This tree is a build artefact under `target/`, so it is not in git and
# a clean checkout has none of it. Before this script it was assembled
# by hand, which is how it came to hold a `debug` binary that nobody
# had a reason to look at again.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="${QOS_DEMO_PROFILE:-demo}"
BIN="$REPO/target/$PROFILE"
OUT="${QOS_APPS_LIVE_DIR:-$REPO/target/qos-apps-live}"
IDENTITY="${QOS_DEMO_IDENTITY:-$OUT/publisher-identity.json}"
PKG_ID="dev.qos.word-processor"
STAGE="$OUT/.stage"

case "$PROFILE" in
    debug)   PROFILE_FLAG="" ;;
    release) PROFILE_FLAG="--release" ;;
    *)       PROFILE_FLAG="--profile $PROFILE" ;;
esac

say() { printf '\033[1m[demo-apps-live]\033[0m %s\n' "$*"; }
die() { echo "FATAL: $*" >&2; exit 1; }

say "building word-processor, qos-pkg, qos-install (profile: $PROFILE)"
(cd "$REPO" && cargo build $PROFILE_FLAG -p word-processor -p pkg-cli -p install)
for b in word-processor qos-pkg qos-install; do
    [ -x "$BIN/$b" ] || die "$BIN/$b missing after build"
done

# A binary older than its own sources is the one failure mode this
# script exists to make loud: a measurement taken against a stale
# payload is a measurement of a build nobody can reproduce, and it
# looks exactly like a good one.
newer="$(find "$REPO/apps/word-processor/src" "$REPO/ui/src" -name '*.rs' \
         -newer "$BIN/word-processor" -print -quit 2>/dev/null || true)"
[ -z "$newer" ] || die "$BIN/word-processor is older than $newer — the build did not take"

mkdir -p "$OUT"
if [ ! -f "$IDENTITY" ]; then
    say "minting publisher identity at $IDENTITY"
    "$BIN/qos-pkg" keygen --out "$IDENTITY" >/dev/null
fi

# `cp`, never a hardlink: the payload has to survive the next
# `cargo build` unchanged, or a rebuild silently redefines what an
# already-taken measurement measured.
say "staging the payload"
rm -rf "$STAGE"
mkdir -p "$STAGE/word-processor/bin"
cp "$BIN/word-processor" "$STAGE/word-processor/bin/word-processor"

say "packing $PKG_ID"
"$BIN/qos-pkg" pack \
    --manifest "$REPO/apps/word-processor/qos-app.toml" \
    --payload "$STAGE/word-processor" \
    --identity "$IDENTITY" \
    --output "$STAGE/word-processor.qpkg"

# Installed fresh rather than edited in place. `registry.toml` names a
# `package_id_hex`, and `qos-install verify` replays that hash down to
# every payload file, so a binary swapped in underneath a kept entry is
# not a faster demo — it is a package that fails its own integrity
# check at the least convenient moment.
say "installing into $OUT"
rm -rf "$OUT/payloads" "$OUT/registry.toml"
QOS_APPS_DATA_DIR="$OUT" "$BIN/qos-install" install "$STAGE/word-processor.qpkg" --force
QOS_APPS_DATA_DIR="$OUT" "$BIN/qos-install" verify "$PKG_ID" \
    || die "the freshly installed package does not verify"

rm -rf "$STAGE"
payload="$OUT/payloads/$PKG_ID/bin/word-processor"
qpkg_note=""
say "payload: $payload ($(numfmt --to=iec-i --suffix=B "$(stat -c%s "$payload")"))$qpkg_note"
