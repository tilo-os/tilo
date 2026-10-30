#!/usr/bin/env bash
# The lint job of tools/ci/local-affected.sh, run where cargo runs (on a builder:
# `rcargo exec`, so its builds share one builder slot and nothing is fetched).
#
#   lint-affected.sh [--portable-ui] [--headless-app] -- -p NAME...
#   lint-affected.sh --default-builds NAME...
#   lint-affected.sh --check-only -- -p NAME...
#
# Clippy over the packages (tools/ci/clippy-check.sh: every target, `-D warnings`,
# the shipped-default builds of packages tested with extra features), which also
# type-checks every target, then the library builds check.yml checks besides:
# tilo-ui without its default features (portable) and tilo-app without them
# (headless). The shipped-default builds (tools/ci/package_features.py
# --default-builds) are a run of their own, `--default-builds NAME...`, which
# local_affected.py starts beside the main one. `--check-only` type-checks every
# target instead, for an architecture that is not linted.
#
# Each builds in a target directory of its own ($CARGO_TARGET_DIR/lint, and
# lint-defaults for the default builds): cargo locks a profile directory for a
# whole build, so a lint sharing the test build's would wait for the tests'
# compile, and they for it; the default builds resolve other features than the
# main lint, so they share few units with it anyway.
set -uo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root" || exit 2

portable_ui=0
headless_app=0
check_only=0
default_builds=0
while [ $# -gt 0 ]; do
    case "$1" in
        --default-builds) default_builds=1; shift; break ;;
        --portable-ui) portable_ui=1; shift ;;
        --headless-app) headless_app=1; shift ;;
        --check-only) check_only=1; shift ;;
        --) shift; break ;;
        *) echo "lint-affected.sh: unknown argument: $1" >&2; exit 2 ;;
    esac
done
if [ "$default_builds" = 1 ]; then
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}/lint-defaults"
    export CLIPPY_CHECK_DEFAULT_BUILDS=0
    status=0
    for package in "$@"; do
        echo "==> Clippy over $package's default build" >&2
        bash tools/ci/clippy-check.sh -p "$package" || status=1
    done
    exit "$status"
fi
[ $# -gt 0 ] || { echo "lint-affected.sh: no packages" >&2; exit 2; }
# Every member resolves its features as in a whole-workspace build, whatever packages are
# named (tools/ci/workspace_unify.py; the package adds no unit): a member a narrower selection
# reaches is otherwise a unit of other features, checked again instead of found fresh.
case " $* " in
    *" workspace-unify "*) ;;
    *) set -- "$@" -p workspace-unify ;;
esac

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}/lint"
export CLIPPY_CHECK_DEFAULT_BUILDS=0
status=0
if [ "$check_only" = 1 ]; then
    cargo check --locked --all-targets --keep-going "$@" || status=1
    exit "$status"
fi
bash tools/ci/clippy-check.sh "$@" || status=1
if [ "$portable_ui" = 1 ]; then
    echo "==> check portable tilo-ui" >&2
    cargo check --locked -p tilo-ui --no-default-features --lib || status=1
fi
if [ "$headless_app" = 1 ]; then
    echo "==> check headless tilo-app" >&2
    cargo check --locked -p tilo-app --no-default-features --lib || status=1
fi
exit "$status"
