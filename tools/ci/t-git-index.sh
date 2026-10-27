#!/usr/bin/env bash
# Run a command with a Git index of the current tree.
#
# t runs checks on an uploaded copy of the working tree, without its .git
# directory. Whole-tree checks that list files through `git ls-files` (the
# unused-dependency scan) get a throwaway repository whose index holds exactly
# the files present, honoring .gitignore. Nothing is committed, so checks that
# read history still need a real checkout; ones that read HEAD's tree take the
# index's instead (tools/policy/check_source_content.py). Every Git command the
# check starts inherits GIT_DIR and GIT_WORK_TREE, so steps that make
# repositories of their own drop them (tools/policy/run.sh).
#
# Indexing writes every file of the tree as a loose object, about 16,000 files.
# On a t node's disk under load that took minutes for each policy step that
# needs it, and a push runs about ten of them; in memory it takes seconds. So
# the repository goes to /dev/shm where there is one, at zlib's fastest level
# (objects and their ids are the same at any level), and is removed after.
#
# Usage: t-git-index.sh COMMAND [ARG...]
set -euo pipefail
if git rev-parse --git-dir >/dev/null 2>&1; then
    exec "$@"
fi
parent="${TMPDIR:-/tmp}"
if [ -d /dev/shm ] && [ -w /dev/shm ]; then
    parent=/dev/shm
fi
GIT_DIR="$(mktemp -d "$parent/t-git-index.XXXXXX")"
trap 'rm -rf "$GIT_DIR"' EXIT
export GIT_DIR GIT_WORK_TREE="$PWD"
git init -q
git config core.looseCompression 1
git add -A
status=0
"$@" || status=$?
exit "$status"
