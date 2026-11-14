#!/usr/bin/env bash
# Tests run as an ordinary user. Root passes every permission check, so a test
# of a sandbox, a Landlock rule or a refusal to run as root proves nothing
# there. A job that starts as root in an image naming its test user
# (TILO_TEST_USER, os/Containerfile.builder) runs each test binary as that
# user; anywhere else (a developer's own account) this changes nothing.
#
#   tools/as-test-user.sh COMMAND [ARG ...]   run COMMAND as the test user
#   tools/as-test-user.sh --own PATH ...      give PATHs (not their contents)
#                                             to the test user
#   tools/as-test-user.sh --own-tree DIR ...  give DIRs and everything in them
#                                             to the test user, once per job: a
#                                             build tree root built and tests
#                                             write in
#   tools/as-test-user.sh --home              the home of whoever tests run as
#
# A test that needs root says so with TILO_TEST_AS_ROOT=1 (a nextest setup
# script, .config/nextest.toml).
#
# Every test start asks these questions, so asking costs it no process:
# tools/cargo-runner.sh and tools/with-test-environment.sh read this file into
# their own shell (`source`) and call the functions below instead of starting
# it, and who the job runs as is the shell's own word, not a program's.
set -euo pipefail

# The test user, or nothing where tests run as whoever started them. Only an
# image that names a test user asks who started the job. bash sets EUID itself
# and never exports it: a value the environment brought instead is exported,
# and is not believed; nor is the EUID of a bash too old to tell the two apart.
tilo_test_user=
if [ -n "${TILO_TEST_USER:-}" ] && [ "${TILO_TEST_AS_ROOT:-0}" != 1 ]; then
    if [ "${BASH_VERSINFO[0]}" -ge 5 ] && [[ ${EUID@a} != *x* ]]; then
        tilo_test_euid=$EUID
    else
        tilo_test_euid=$(id -u)
    fi
    [ "$tilo_test_euid" != 0 ] || tilo_test_user=$TILO_TEST_USER
fi

# PATH ...: the paths, not their contents, are the test user's.
tilo_test_user_own() {
    [ -z "$tilo_test_user" ] || chown -- "$tilo_test_user:" "$@"
}

# DIR ...: the directories and everything in them are the test user's.
tilo_test_user_own_tree() {
    local path mark lock
    [ -n "$tilo_test_user" ] || return 0
    # One walk per job, and the tests of a job start at once: the first walks the
    # tree while the others wait on the lock and then find the job's mark. Every
    # job walks, because root builds and steps write into the tree after an
    # earlier job handed it over, and a directory root left there refuses a test
    # that writes inside it. A mark is per job so a walk per test process is not
    # what a warm tree pays: a start that finds the mark starts nothing.
    for path; do
        mark="$path/.handed-to-$tilo_test_user${GITHUB_RUN_ID:+-$GITHUB_RUN_ID-${GITHUB_JOB:-job}}"
        [ ! -e "$mark" ] || continue
        if command -v flock >/dev/null; then
            exec {lock}>>"$path/.handover-$tilo_test_user.lock"
            flock "$lock"
            if [ ! -e "$mark" ]; then
                chown -R -h -- "$tilo_test_user:" "$path" && : >"$mark"
            fi
            exec {lock}>&-
        else
            chown -R -h -- "$tilo_test_user:" "$path" && : >"$mark"
        fi
    done
    return 0
}

# The home of whoever tests run as: the test user's, else this shell's own.
tilo_test_user_home() {
    if [ -n "$tilo_test_user" ]; then
        getent passwd "$tilo_test_user" | cut -d: -f6
    else
        printf '%s\n' "$HOME"
    fi
}

# COMMAND [ARG ...]: this shell becomes COMMAND, running as the test user.
tilo_test_user_exec() {
    [ -n "$tilo_test_user" ] || exec "$@"
    exec setpriv --reuid="$tilo_test_user" --regid="$tilo_test_user" --init-groups --inh-caps=-all -- "$@"
}

# Read into another script's shell, this file is the functions above.
if [ "${BASH_SOURCE[0]}" != "$0" ]; then
    return 0
fi

if [ "$#" -eq 0 ]; then
    echo "usage: $0 COMMAND [ARG ...] | --own PATH ... | --own-tree DIR ... | --home" >&2
    exit 2
fi
if [ "$1" = --home ]; then
    tilo_test_user_home
    exit 0
fi
if [ "$1" = --own ]; then
    shift
    tilo_test_user_own "$@"
    exit 0
fi
if [ "$1" = --own-tree ]; then
    shift
    tilo_test_user_own_tree "$@"
    exit 0
fi
tilo_test_user_exec "$@"
