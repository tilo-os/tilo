#!/usr/bin/env bash
# Build the binaries and libraries of other packages that a package's tests
# start or load, before the tests start: the session's nextest setup scripts
# (.config/nextest.toml, `session-siblings` and `pam-module`, in every
# profile).
#
#   tools/build-sibling-binaries.sh PACKAGE:BIN...  or  PACKAGE:LIB
#
# A test finds such a binary through tilo_test_env::sibling_binary, which
# answers only with a binary the run built; it never starts Cargo itself, so
# the run's target directory is written by one Cargo at a time and nothing is
# compiled twice. So each binary's path is handed to the run's tests as
# TILO_SIBLING_BIN_<bin, - as _> (through nextest's NEXTEST_ENV), which
# sibling_binary answers with.
#
# A name with an underscore is a library, not a binary: pam-tilo's tests load
# `pam_tilo` as libpam does, so it is built as the package's library (`--lib`,
# a cdylib Cargo leaves beside the binaries) and handed as the library's own
# variable, TILO_PAM_TILO_MODULE, which the tests read. A binary's target name
# is Cargo's own `-` spelling, so one never collides with a library's.
#
# A binary the run was handed already is not built: t builds these once in its
# test build (t.toml's `starts`), keeps them in the archive its shards run
# from, and names each to the tests as NEXTEST_BIN_EXE_<bin>, a library as
# T_STARTED_CDYLIB_<lib>. Built here, every shard compiled the three apps
# again (~850 units, minutes a shard). With every one handed, Cargo is not
# started at all.
#
# A test run that is not a nextest run (`scripts/two-netns-share.sh`, a test
# binary started by hand) runs this once before its tests, and the binaries it
# leaves beside them are found there.
set -euo pipefail

if [[ $# -eq 0 ]]; then
    echo "usage: $0 PACKAGE:BIN... | PACKAGE:LIB..." >&2
    exit 2
fi
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

# Hand BIN's path at PATH to the tests, a library's as its own variable.
hand() {
    local variable=TILO_SIBLING_BIN_${1//-/_}
    [[ "$1" != *_* ]] || variable=TILO_${1^^}_MODULE
    if [[ -n "${NEXTEST_ENV:-}" ]]; then
        echo "$variable=$2" >>"$NEXTEST_ENV"
    fi
}

arguments=(build --locked --message-format=json-render-diagnostics)
bins=()
libraries=()
for binary; do
    # PACKAGE:BIN, or PACKAGE:BIN=FEATURE,FEATURE for a binary a package only
    # builds with those features on (the workspace host's subcommand is one).
    features="${binary#*=}"
    [[ "$features" == "$binary" ]] && features=
    binary="${binary%%=*}"
    package=${binary%%:*}
    target=${binary#*:}
    if [[ "$target" == *_* ]]; then
        # A library the tests load. t hands one as a file whose existence it was told
        # (T_STARTED_CDYLIB_<lib>); a hand that is no file fails here rather than
        # building again, which would hide a broken handoff and compile the library in
        # every shard.
        handed=$(printenv "T_STARTED_CDYLIB_${target}" || true)
        if [[ -n "$handed" ]]; then
            if [[ ! -f "$handed" ]]; then
                echo "$0: T_STARTED_CDYLIB_${target} names no file: $handed" >&2
                exit 1
            fi
            hand "$target" "$handed"
            continue
        fi
        arguments+=(--package "$package" --lib)
        [[ -z "$features" ]] || arguments+=(--features "$features")
        libraries+=("$target")
        continue
    fi
    handed=$(printenv "NEXTEST_BIN_EXE_${target}" || true)
    if [[ -n "$handed" && -x "$handed" ]]; then
        # A featured entry's hand is that build: t builds each `starts` artifact with the
        # features it names and refuses one target declared with two feature sets in a
        # family, and tools/tests/test_nextest_policy.py holds every family to declaring
        # the featured build alone, so the file a hand names is the one these tests start.
        hand "$target" "$handed"
        continue
    fi
    arguments+=(--package "$package" --bin "$target")
    [[ -z "$features" ]] || arguments+=(--features "$features")
    bins+=("$target")
done
if [[ ${#bins[@]} -eq 0 && ${#libraries[@]} -eq 0 ]]; then
    exit 0
fi
cd "$repo"
messages=$("${CARGO:-cargo}" "${arguments[@]}")
for bin in "${bins[@]}"; do
    # The artifact message of each binary names the executable it wrote.
    path=$(grep -o "\"executable\":\"[^\"]*/${bin}\"" <<<"$messages" | tail -n 1 |
        sed -e 's/^"executable":"//' -e 's/"$//')
    if [[ -z "$path" || ! -x "$path" ]]; then
        echo "$0: cargo named no executable for $bin" >&2
        exit 1
    fi
    hand "$bin" "$path"
done
for library in "${libraries[@]}"; do
    # A library's artifact message is a compiler artifact whose filenames name the
    # file Cargo uplifted beside the binaries: `<target>/<profile>/lib<lib>.so`.
    path=$(grep -o "\"[^\"]*/lib${library}\.so\"" <<<"$messages" | tail -n 1 |
        sed -e 's/^"//' -e 's/"$//')
    if [[ -z "$path" ]]; then
        echo "$0: cargo named no library for $library" >&2
        exit 1
    fi
    hand "$library" "$path"
done
