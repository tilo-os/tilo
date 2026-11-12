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
#                                             to the test user, once: a build
#                                             tree root built and tests write in
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
    # A tree the build already handed over is left alone, and the tests of one
    # run start at once: the first walks it while the others wait on the lock,
    # then find it theirs. Cargo builds into the tree as root on every run, so a
    # walk per test process would cost it over and over.
    [ -z "$user" ] || for path; do
        [ "$(stat -c %U -- "$path")" = "$user" ] && continue
        if command -v flock >/dev/null; then
            exec {lock}>"$path/.handed-to-$user"
            flock "$lock"
            [ "$(stat -c %U -- "$path")" = "$user" ] || chown -R -h -- "$user:" "$path"
            exec {lock}>&-
        else
            chown -R -h -- "$user:" "$path"
        fi
    done
    exit 0
fi
[ -n "$user" ] || exec "$@"
exec setpriv --reuid="$user" --regid="$user" --init-groups --inh-caps=-all -- "$@"
