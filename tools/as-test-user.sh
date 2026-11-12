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
#
# A test that needs root says so with TILO_TEST_AS_ROOT=1 (a nextest setup
# script, .config/nextest.toml).
set -euo pipefail
if [ "$#" -eq 0 ]; then
    echo "usage: $0 COMMAND [ARG ...] | --own PATH ..." >&2
    exit 2
fi
user=
if [ "$(id -u)" = 0 ] && [ -n "${TILO_TEST_USER:-}" ] && [ "${TILO_TEST_AS_ROOT:-0}" != 1 ]; then
    user=$TILO_TEST_USER
fi
if [ "$1" = --own ]; then
    shift
    [ -z "$user" ] || chown -- "$user:" "$@"
    exit 0
fi
[ -n "$user" ] || exec "$@"
exec setpriv --reuid="$user" --regid="$user" --init-groups --inh-caps=-all -- "$@"
