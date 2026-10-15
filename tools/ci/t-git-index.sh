#!/usr/bin/env bash
# Run a command with a Git index of the current tree.
#
# t runs checks on an uploaded copy of the working tree, without its .git
# directory. Whole-tree checks that list files through `git ls-files` (the
# unused-dependency scan) get a throwaway repository in TMPDIR whose index
# holds exactly the files present, honoring .gitignore. Nothing is committed,
# so checks that read history or HEAD still need a real checkout.
#
# Usage: t-git-index.sh COMMAND [ARG...]
set -euo pipefail
if ! git rev-parse --git-dir >/dev/null 2>&1; then
    GIT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/t-git-index.XXXXXX")"
    export GIT_DIR GIT_WORK_TREE="$PWD"
    git init -q
    git add -A
fi
exec "$@"
