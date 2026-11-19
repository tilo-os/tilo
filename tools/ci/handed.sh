#!/usr/bin/env bash
# The artifact a test job was handed, if any. t builds a program or library a package's tests
# need once in the family's build (t.toml's `starts`) and names it to every shard, so a shard
# builds nothing: a bin as NEXTEST_BIN_EXE_<bin>, nextest's own name for a package's binaries;
# a cdylib as T_STARTED_CDYLIB_<lib>, which t sets and nextest never does. A job handed none
# (a developer's) builds it, saying where.
#
# A hand that cannot be used is the handoff's fault, said as such: building would hide it and
# compile the artifact in every shard again (as tools/ci/capability_filter.py answers for the
# probe).
#
# Read into the shell of the wrapper (tools/ci/nextest.sh) and of the setup steps that hand a
# program to their tests (tools/test-env-setup.sh).
handed() { # VARIABLE, the test its file must pass, and what a file of its kind is
    local path
    path=$(printenv "$1" || true)
    if [[ -n $path ]] && ! test "$2" "$path"; then
        echo "$0: $1 names $path, which is not $3" >&2
        exit 1
    fi
    [[ -z $path ]] || printf '%s' "$path"
}
handed_bin() { handed "NEXTEST_BIN_EXE_$1" -x an executable; }
