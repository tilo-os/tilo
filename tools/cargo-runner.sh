#!/usr/bin/env bash
# Cargo's runner for Linux and macOS targets (.cargo/config.toml): `cargo test`,
# `cargo bench`, `cargo nextest` and `cargo run` start every binary through
# this script, locally and on a remote builder alike.
#
# Test and bench harnesses (target/<profile>/deps/*) and doctests always run
# inside tools/with-test-environment.sh: a fresh HOME and XDG tree and no
# desktop session. Whoever runs a test and however, it cannot reach their
# session bus, systemd user manager, logind, portals, media players, keyring
# or display. A run that is already inside that environment (CI,
# tools/test-env.sh, scripts/test-local.sh) keeps it. Started as root in the
# CI image, they run as its test user (tools/as-test-user.sh).
#
# `cargo run` targets (target/<profile>/<bin>, examples) start unchanged:
# that is how a developer tries an app against their own desktop.
set -euo pipefail
if [ "$#" -eq 0 ]; then
    echo "usage: $0 BINARY [ARG ...]" >&2
    exit 2
fi
case $1 in
    */deps/* | */rustdoctest*/*)
        tools=$(dirname -- "${BASH_SOURCE[0]}")
        dir=$(dirname -- "$1")
        while [ "$dir" != / ] && [ "$dir" != . ] && [ ! -e "$dir/.rustc_info.json" ]; do dir=$(dirname -- "$dir"); done
        # The build tree ($dir, which holds the profile directories) and the
        # build directory's tmp (CARGO_TARGET_TMPDIR) that tests write into are
        # the test user's too: cargo made them as whoever built. A test that
        # writes beside the build's output — review images, fixture caches,
        # prepared catalogs — lands in the tree, and root's mode 0755
        # directories there would refuse it.
        if [ -e "$dir/.rustc_info.json" ]; then
            bash "$tools/as-test-user.sh" --own-tree "$dir"
        fi
        [ ! -d "$dir/tmp" ] || bash "$tools/as-test-user.sh" --own "$dir/tmp"
        exec bash "$tools/with-test-environment.sh" bash "$tools/as-test-user.sh" "$@"
        ;;
    *)
        exec "$@"
        ;;
esac
