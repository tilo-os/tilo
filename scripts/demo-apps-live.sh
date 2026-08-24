#!/usr/bin/env bash
# Assemble target/qos-apps-live — the app payload the YC demo serves.
#
#   scripts/demo-apps-live.sh            # optimised (profile: demo)
#   QOS_DEMO_PROFILE=debug scripts/demo-apps-live.sh
#
# `scripts/yc-demo.py` reads this tree twice, and both readings are the
# app a viewer waits for:
#
#   * xaver and the agent get it as an already-installed app, straight
#     out of `payloads/<id>/`;
#   * quinten does NOT have the word processor, and fetches it in beat 5
#     — that beat is a `.qpkg` packed out of this same payload and
#     served over loopback by `serve-test-registry`.
#
# So the size of one binary here is the whole of what "search, Enter,
# and it opens" costs on camera. That is why the default profile is an
# optimised one and not `debug`: a `debug` word processor is ~449 MB,
# and moving and BLAKE3-hashing 449 MB is most of the 16-21 s that beat
# used to take. Nothing about the architecture required that wait —
# only the profile did.
#
# `demo` rather than `release` because that is what the compositor is
# built as and what `yc-demo.py` resolves first. It is the release
# configuration in everything that reaches the screen — opt-level 3,
# `strip = "symbols"` — and differs from it in how it links: thin LTO
# across 16 codegen units rather than fat LTO through one. That is a
# recording profile, relinked after almost every change, where fat LTO
# cost over seven minutes of a single core with the rest of the machine
# idle. It also sets `debug-assertions` for the `shell` package, which
# this script does not build.
#
# This tree is a build artefact under `target/`, so it is not in git and
# a clean checkout has none of it. Before this script it was assembled
# by hand, which is how it came to hold a `debug` binary that nobody
# had a reason to look at again.
#
# ## Which apps belong here, and which belong to the machine
#
# The machine's own store, `~/.local/share/qos-apps`, holds the rest of
# the suite, and `yc-demo.py` composes the two: an id present in *this*
# registry wins, and the machine supplies everything else. So an app is
# listed below when the film depends on what it does rather than merely
# on it being installed — the machine's payloads are static binaries
# from whenever somebody last ran an installer, and a run against one of
# those films a build that predates everything under test. Anything
# under active development belongs here.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="${QOS_DEMO_PROFILE:-demo}"
BIN="$REPO/target/$PROFILE"
OUT="${QOS_APPS_LIVE_DIR:-$REPO/target/qos-apps-live}"
IDENTITY="${QOS_DEMO_IDENTITY:-$OUT/publisher-identity.json}"
STAGE="$OUT/.stage"

# `<package id>|<crate, binary and apps/ directory>`. The three names
# coincide for everything here; a package where they did not would need
# its own fields rather than a special case in the loop.
APPS=(
    "dev.qos.word-processor|word-processor"
    "dev.qos.photos|photos"
)

case "$PROFILE" in
    debug)   PROFILE_FLAG="" ;;
    release) PROFILE_FLAG="--release" ;;
    *)       PROFILE_FLAG="--profile $PROFILE" ;;
esac

say() { printf '\033[1m[demo-apps-live]\033[0m %s\n' "$*"; }
die() { echo "FATAL: $*" >&2; exit 1; }

crates=()
for app in "${APPS[@]}"; do crates+=(-p "${app#*|}"); done

say "building ${#APPS[@]} app(s), qos-pkg, qos-install (profile: $PROFILE)"
(cd "$REPO" && cargo build $PROFILE_FLAG "${crates[@]}" -p pkg-cli -p install)
for b in qos-pkg qos-install; do
    [ -x "$BIN/$b" ] || die "$BIN/$b missing after build"
done

# A binary older than its own sources is the one failure mode this
# script exists to make loud: a measurement taken against a stale
# payload is a measurement of a build nobody can reproduce, and it
# looks exactly like a good one. `ui/src` counts as every app's own
# source, because each of these is a client of the toolkit and a
# toolkit change that never reached a payload is the same lie.
for app in "${APPS[@]}"; do
    name="${app#*|}"
    [ -x "$BIN/$name" ] || die "$BIN/$name missing after build"
    newer="$(find "$REPO/apps/$name/src" "$REPO/ui/src" -name '*.rs' \
             -newer "$BIN/$name" -print -quit 2>/dev/null || true)"
    [ -z "$newer" ] || die "$BIN/$name is older than $newer — the build did not take"
done

mkdir -p "$OUT"
if [ ! -f "$IDENTITY" ]; then
    say "minting publisher identity at $IDENTITY"
    "$BIN/qos-pkg" keygen --out "$IDENTITY" >/dev/null
fi

# Installed fresh rather than edited in place. `registry.toml` names a
# `package_id_hex`, and `qos-install verify` replays that hash down to
# every payload file, so a binary swapped in underneath a kept entry is
# not a faster demo — it is a package that fails its own integrity
# check at the least convenient moment. Emptied once, before the loop:
# inside it, each app would delete the one installed on the pass before.
rm -rf "$STAGE" "$OUT/payloads" "$OUT/registry.toml"

for app in "${APPS[@]}"; do
    id="${app%%|*}"
    name="${app#*|}"

    # `cp`, never a hardlink: the payload has to survive the next
    # `cargo build` unchanged, or a rebuild silently redefines what an
    # already-taken measurement measured.
    say "staging $id"
    mkdir -p "$STAGE/$name/bin"
    cp "$BIN/$name" "$STAGE/$name/bin/$name"

    say "packing $id"
    "$BIN/qos-pkg" pack \
        --manifest "$REPO/apps/$name/qos-app.toml" \
        --payload "$STAGE/$name" \
        --identity "$IDENTITY" \
        --output "$STAGE/$name.qpkg"

    say "installing $id into $OUT"
    QOS_APPS_DATA_DIR="$OUT" "$BIN/qos-install" install "$STAGE/$name.qpkg" --force
    QOS_APPS_DATA_DIR="$OUT" "$BIN/qos-install" verify "$id" \
        || die "the freshly installed $id does not verify"
done

# Every id asked for, present in the registry the demo will read. A
# package that packed and then did not land is otherwise a launch
# failure attributed to the app, hours later and somewhere else.
for app in "${APPS[@]}"; do
    id="${app%%|*}"
    grep -q "^id = \"$id\"\$" "$OUT/registry.toml" \
        || die "$id is missing from $OUT/registry.toml after install"
done

rm -rf "$STAGE"
for app in "${APPS[@]}"; do
    id="${app%%|*}"
    name="${app#*|}"
    payload="$OUT/payloads/$id/bin/$name"
    say "payload: $payload ($(numfmt --to=iec-i --suffix=B "$(stat -c%s "$payload")"))"
done
