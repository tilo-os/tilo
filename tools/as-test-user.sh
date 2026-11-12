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
set -euo pipefail
if [ "$#" -eq 0 ]; then
    echo "usage: $0 COMMAND [ARG ...] | --own PATH ... | --own-tree DIR ... | --home" >&2
    exit 2
fi
user=
if [ "$(id -u)" = 0 ] && [ -n "${TILO_TEST_USER:-}" ] && [ "${TILO_TEST_AS_ROOT:-0}" != 1 ]; then
    user=$TILO_TEST_USER
fi
if [ "$1" = --home ]; then
    if [ -n "$user" ]; then getent passwd "$user" | cut -d: -f6; else printf '%s\n' "$HOME"; fi
    exit 0
fi
if [ "$1" = --own ]; then
    shift
    [ -z "$user" ] || chown -- "$user:" "$@"
    exit 0
fi
if [ "$1" = --own-tree ]; then
    shift
    # One walk per job, and the tests of a job start at once: the first walks the
    # tree while the others wait on the lock and then find the job's mark. Every
    # job walks, because root builds and steps write into the tree after an
    # earlier job handed it over, and a directory root left there refuses a test
    # that writes inside it. A mark is per job so a walk per test process is not
    # what a warm tree pays.
    [ -z "$user" ] || for path; do
        mark="$path/.handed-to-$user${GITHUB_RUN_ID:+-$GITHUB_RUN_ID-${GITHUB_JOB:-job}}"
        [ ! -e "$mark" ] || continue
        if command -v flock >/dev/null; then
            exec {lock}>>"$path/.handover-$user.lock"
            flock "$lock"
            if [ ! -e "$mark" ]; then
                chown -R -h -- "$user:" "$path" && : >"$mark"
            fi
            exec {lock}>&-
        else
            chown -R -h -- "$user:" "$path" && : >"$mark"
        fi
    done
    exit 0
fi
[ -n "$user" ] || exec "$@"
exec setpriv --reuid="$user" --regid="$user" --init-groups --inh-caps=-all -- "$@"
