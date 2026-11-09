#!/usr/bin/env bash
# Contract checks for dist/dev/build-provenance.sh: a run refuses a build another tree made,
# and a build this tree just made is taken as it is.
#
# The checks use a throwaway git repository in place of the tree: "another tree" is the same
# files at a changed commit, and "this tree" is the one the record was written for. They prove
# the refusals and the record without building anything or starting a session.
set -euo pipefail

tool=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)/build-provenance.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
tree=$work/tree
mkdir -p "$tree"
git -C "$tree" init -q
git -C "$tree" config user.email test@tilo.so
git -C "$tree" config user.name Test
printf 'fn main() {}\n' > "$tree/main.rs"
printf '/target/\n' > "$tree/.gitignore"
git -C "$tree" add .
git -C "$tree" commit -qm 'the tree'
commit=$(git -C "$tree" rev-parse HEAD)

# The helper reads its repository from its own location; the checks run a copy of it inside
# the throwaway tree, so `digest` and `write` see that tree as theirs.
dist=$tree/dist/dev
mkdir -p "$dist"
cp -p "$tool" "$dist/build-provenance.sh"
helper=$dist/build-provenance.sh

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
pass() { printf 'ok: %s\n' "$*"; }

build=$tree/target/nested-session
mkdir -p "$build"

# A build this tree made is taken as it is, and `show` names the commit.
"$helper" write "$build" 2>/dev/null
"$helper" verify "$build" || fail "a build just recorded is not this tree's"
shown=$("$helper" show "$build")
[[ $shown == *"$commit"* ]] || fail "show does not name the commit: $shown"
pass "a build this tree just made is verified, and show names its commit"

# The record is an untracked file beside the build, never part of the tree's sources: writing
# it again does not change the digest it records.
before=$("$helper" digest)
"$helper" write "$build" 2>/dev/null
[[ $("$helper" digest) == "$before" ]] || fail "recording a build changed the tree's digest"
pass "the record is outside the tree's sources"

# Another tree's build: a file of the tree changed after the record was written.
printf 'fn main() { println!("changed"); }\n' > "$tree/main.rs"
if "$helper" verify "$build" 2>"$work/why"; then
    fail "a build made before a source change is verified"
fi
grep -q "not this tree" "$work/why" || fail "the refusal does not say another tree made it: $(cat "$work/why")"
pass "a build another tree made is refused, by name"
# The build's own record is unchanged: the refusal is the tree's, not a rewrite.
grep -q "digest $before" "$build/.tilo-build-provenance" \
    || fail "verify rewrote the build's record"
git -C "$tree" checkout -q -- main.rs

# A build directory with no record is one whose tree is unknown: refused, saying so.
mkdir -p "$work/unknown"
if "$helper" verify "$work/unknown" 2>"$work/why"; then
    fail "a build with no record is verified"
fi
grep -q "records no tree" "$work/why" || fail "an unrecorded build is not named: $(cat "$work/why")"
pass "a build with no record is refused, saying its tree is unknown"

# A tree outside git records `none` for the commit and still verifies, so a copied build tree
# without `.git` is not refused for having no commit to name.
plain=$work/plain
mkdir -p "$plain/dist/dev" "$plain/target/nested-session"
cp -p "$tool" "$plain/dist/dev/build-provenance.sh"
printf 'fn main() {}\n' > "$plain/main.rs"
"$plain/dist/dev/build-provenance.sh" write "$plain/target/nested-session" 2>/dev/null
"$plain/dist/dev/build-provenance.sh" verify "$plain/target/nested-session" \
    || fail "a tree outside git does not verify"
shown=$("$plain/dist/dev/build-provenance.sh" show "$plain/target/nested-session")
[[ $shown == *"outside git"* ]] || fail "show does not say the tree is outside git: $shown"
printf 'fn main() { println!("changed"); }\n' > "$plain/main.rs"
if "$plain/dist/dev/build-provenance.sh" verify "$plain/target/nested-session" 2>/dev/null; then
    fail "a tree outside git verifies after a source change"
fi
pass "a tree outside git records and verifies by its sources"

# Deleting a tracked file is a change: the digest follows the working tree, not the index.
git -C "$tree" rm -q main.rs
if "$helper" verify "$build" 2>/dev/null; then
    fail "a deleted source file does not change the build's provenance"
fi
pass "a deleted source file fails the build's provenance"

printf 'all build-provenance checks passed\n'
