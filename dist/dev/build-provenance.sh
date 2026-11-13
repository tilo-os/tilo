#!/usr/bin/env bash
# Which tree a development build or install came from, so a session started from a build
# that is not this tree's fails loudly instead of running another tree's binaries.
#
# A dev session is a set of builds and installs that are easy to point at the wrong tree:
#
#   * `dist/dev/nested-session-build.sh` builds host programs and thin apps into
#     `${CARGO_TARGET_DIR:-target}/{nested,thin-dev,nested-session}`,
#   * `dist/dev/tilolang-runtime-build.sh` builds the tilolang binary into those same two,
#   * `dist/dev/shell-run.sh` builds into `target/debug` and bakes the runtime bundles from
#     `target/thin-dev`, and
#   * `dist/dev/nested-session.sh` reuses the apps a state directory already holds.
#
# A shared or stale target directory, a build handed over through `TILO_NESTED_SESSION_BUILD`,
# a checkout someone copied a `target/` into, or a state directory from another tree all run
# code this tree's sources did not build. Which commit it was is then invisible: the
# artifacts are binary, and the failure reads as a bug in today's code. That is exactly how a
# session at another tree's commit was read as two fixes failing.
#
# So the script that makes a build or an install records the tree it came from beside what it
# made: the source digest (a hash of every file git tracks or leaves untracked-and-not-ignored,
# which is what the build sees, so a dirty tree is never mistaken for its commit) and the
# tree's commit, when it is in one. The script that runs the build checks it before anything
# starts.
#
#   dist/dev/build-provenance.sh digest                  this tree's source digest
#   dist/dev/build-provenance.sh write DIR...            record this tree beside each build
#   dist/dev/build-provenance.sh verify DIR...           fail unless every build is this tree's
#   dist/dev/build-provenance.sh show DIR...             a line each: which tree made it
#
# DIR is a build directory (`DIR/.tilo-build-provenance` holds the record). The record is an
# untracked file beside the artifacts (`target/` is ignored, so it never joins the tree's
# sources), and a directory without one is a build this script never made — a tree unknown, so
# `verify` refuses it.
#
# `verify` compares the recorded digest with the tree's now: any source change between the
# build and the run fails it, which is the point. A run that wants a build of another state
# of the tree builds that state, rather than reusing this one; `show` says which tree a build
# came from when a developer needs to know.
#
# The record is not a signature. It says which tree an artifact *was built from*, not that
# nobody edited the artifact since; the runtime bundle's own signature and the app store's
# publisher key already say what a session runs.
set -euo pipefail

repo_root=$(cd "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)
record_name=.tilo-build-provenance

die() { printf 'build-provenance: %s\n' "$*" >&2; exit 1; }

# The record file inside build directory `dir`.
record_of() { printf '%s/%s\n' "$1" "$record_name"; }

# Paths that are not inputs to what a build produces: documentation, and the developer scripts
# that orchestrate a build without compiling into it. A build does not change when one of these
# does, and a run that reused a build across such an edit is still this tree's build — so they
# are out of the digest, and `--no-build` stays usable while a developer works on them. What
# *does* change the artifacts (Cargo files, sources, `vendor/`, build scripts, app manifests and
# app surfaces, `os/scripts/build-thin-apps.sh`, `tools/apps/`) is what is left in.
not_an_input() {
    case $1 in
        ./docs/* | ./.claude/* | ./dist/dev/* | *.md | ./target/* | ./os/output/*) return 0 ;;
        *) return 1 ;;
    esac
}

digest_of() {
    local root=$1 listing present sums
    listing=$(mktemp)
    present=$(mktemp)
    sums=$(mktemp)
    # shellcheck disable=SC2064 # the temp files are this function's, freed when it returns
    trap "rm -f $(printf '%q' "$listing") $(printf '%q' "$present") $(printf '%q' "$sums")" RETURN
    # Every file a build reads: what git tracks, and what it leaves untracked and not ignored.
    # A tree outside git has no ignore rules, so it hashes its files as `cargo-freshness.sh`
    # does: everything but the build output, the caches and git's own directory.
    if git -C "$root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        git -C "$root" ls-files -z --cached --others --exclude-standard -- :/ > "$listing"
    else
        (cd "$root" && find . \
            \( -path ./.git -o -path ./target -o -path ./.cargo-home -o -path ./.sccache \) -prune \
            -o -type f -print0) > "$listing"
    fi
    # A tracked file deleted in the working tree is a change, not a file to hash; the record
    # itself is never a source, so writing one cannot change the tree it records; and a file
    # that is not a build input is left out.
    (
        cd "$root"
        while IFS= read -r -d '' file; do
            [[ -f $file ]] || continue
            [[ $(basename -- "$file") == *"$record_name" ]] && continue
            # Both listings name files from the tree's root: git without a `./` and find with
            # one, so the patterns read one form.
            not_an_input "./${file#./}" && continue
            printf '%s\0' "$file"
        done < "$listing"
    ) > "$present"
    # Paths and contents, sorted: a rename is a change, and a changed file is a different
    # digest.
    (cd "$root" && xargs -0 -r sha256sum < "$present" | LC_ALL=C sort) > "$sums"
    sha256sum < "$sums" | cut -d' ' -f1
}

# The commit this tree is at, when git can say; `none` for a tree outside a repository. A
# dirty tree is still its commit here — the digest is what tells a dirty tree from its
# commit's build.
commit_of() {
    local root=$1
    git -C "$root" rev-parse --verify --quiet HEAD 2>/dev/null || printf 'none\n'
}

# Why `path` is not a build of this tree, as one sentence; empty when it is. `digest` and
# `commit` are this tree's, computed once by the caller.
mismatch_of() {
    local path=$1 digest=$2 commit=$3 record recorded_digest recorded_commit
    record=$(record_of "$path")
    if [[ ! -e $path ]]; then
        printf '%s is not there' "$path"
        return 0
    fi
    if [[ ! -e $record ]]; then
        printf '%s records no tree: another tree, a shared build or a copied target/ made it, or it predates this check' "$path"
        return 0
    fi
    recorded_digest=$(awk '$1 == "digest" { print $2 }' "$record")
    recorded_commit=$(awk '$1 == "commit" { print $2 }' "$record")
    if [[ -z $recorded_digest ]]; then
        printf '%s records no source digest' "$path"
        return 0
    fi
    if [[ $recorded_digest != "$digest" ]]; then
        local from
        if [[ -n $recorded_commit && $recorded_commit != none ]]; then
            from="commit $recorded_commit"
        else
            from="a tree outside git"
        fi
        printf '%s was built from %s, not this tree%s' \
            "$path" "$from" \
            "$([[ $commit != none ]] && printf ' (%s)' "$commit")"
    fi
}

write() {
    local path digest commit
    digest=$(digest_of "$repo_root")
    commit=$(commit_of "$repo_root")
    for path in "$@"; do
        [[ -d $path ]] || die "$path is no build directory to record"
        printf 'digest %s\ncommit %s\n' "$digest" "$commit" > "$(record_of "$path")"
        printf 'build-provenance: recorded %s as %s\n' \
            "$path" "$([[ $commit == none ]] && printf 'a tree outside git' || printf 'commit %s' "$commit")" >&2
    done
}

verify() {
    local digest commit path why failed=0
    digest=$(digest_of "$repo_root")
    commit=$(commit_of "$repo_root")
    for path in "$@"; do
        if why=$(mismatch_of "$path" "$digest" "$commit") && [[ -z $why ]]; then
            continue
        fi
        printf 'build-provenance: %s\n' "$why" >&2
        failed=1
    done
    ((failed == 0)) || die "build this tree (run without --no-build), or point the run at a build of this tree"
}

show() {
    local path record
    for path in "$@"; do
        record=$(record_of "$path")
        if [[ ! -e $record ]]; then
            printf '%s: no tree recorded (unknown, or made before this check)\n' "$path"
            continue
        fi
        awk -v where="$path" '
            $1 == "commit" && $2 != "none" { c = $2 }
            $1 == "digest" { d = $2 }
            END { printf "%s: %s, sources %s\n", where, (c == "" ? "a tree outside git" : "commit " c), d }
        ' "$record"
    done
}

case ${1:-} in
    digest) shift; digest_of "$repo_root" ;;
    write) shift; (($#)) || die "write takes the builds it records"; write "$@" ;;
    verify) shift; (($#)) || die "verify takes the builds it checks"; verify "$@" ;;
    show) shift; (($#)) || die "show takes the builds it names"; show "$@" ;;
    *) sed -n '2,/^set -euo/p' "$0" | sed '$d;s/^# \{0,1\}//' >&2; exit 2 ;;
esac
