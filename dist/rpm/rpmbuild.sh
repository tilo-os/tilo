#!/usr/bin/env bash
# Run rpmbuild on this machine, or, on an image-based host without rpm-build,
# in the development container named by `.build-container` (DEVELOPMENT.md,
# "If you build in a container"). Only packaging runs there: the binaries it
# packages are compiled before, by whatever `cargo` this host has.
#
#   dist/rpm/rpmbuild.sh --check     say where rpmbuild runs, or fail saying how to fix it
#   dist/rpm/rpmbuild.sh ARGS...     run `rpmbuild ARGS...` there
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

die() { echo "rpmbuild: $*" >&2; exit 1; }

# The checkout a worktree belongs to, wherever the worktree lives.
main_checkout() {
    local common
    common=$(git -C "$repo_root" rev-parse --path-format=absolute --git-common-dir 2>/dev/null) \
        || return 0
    dirname "$common"
}

# The marker sits at the project root and covers every worktree below it, so
# look upwards from this checkout as the cargo shim does, stopping at $HOME;
# then from the main checkout, for a worktree that lives elsewhere.
find_marker() {
    local dir
    for dir in "$repo_root" "$(main_checkout)"; do
        while [[ -n $dir && $dir != / ]]; do
            if [[ -r $dir/.build-container ]]; then
                printf '%s\n' "$dir/.build-container"
                return 0
            fi
            [[ $dir != "$HOME" ]] || break
            dir=$(dirname "$dir")
        done
    done
    return 1
}

container=
if ! command -v rpmbuild >/dev/null 2>&1; then
    marker=$(find_marker) || {
        main=$(main_checkout)
        die "rpmbuild is not installed here, and no build container is named.
Install rpm-build, or name the distrobox that has it (the one your cargo builds in):
    printf '%s\\n' <distrobox> > ${main:-$repo_root}/.build-container
(see DEVELOPMENT.md, \"If you build in a container\")"
    }
    container=$(head -n1 "$marker" | tr -d '[:space:]')
    [[ -n $container ]] || die "$marker is empty; it must name a distrobox"
    command -v distrobox >/dev/null 2>&1 \
        || die "rpmbuild is not installed here, and distrobox, which $marker asks for, is not either"
fi

if [[ ${1:-} == --check ]]; then
    if [[ -z $container ]]; then
        echo "rpmbuild: on this machine"
        exit 0
    fi
    distrobox enter "$container" -- sh -c 'command -v rpmbuild' >/dev/null 2>&1 \
        || die "the build container $container has no rpmbuild; install it there:
    distrobox enter $container -- sudo dnf install -y rpm-build"
    echo "rpmbuild: in the build container $container ($marker)"
    exit 0
fi

if [[ -z $container ]]; then
    exec rpmbuild "$@"
fi
printf -v quoted '%q ' rpmbuild "$@"
exec distrobox enter "$container" -- bash -lc "$quoted"
