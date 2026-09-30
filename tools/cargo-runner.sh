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
# tools/test-env.sh, scripts/test-local.sh) keeps it.
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
        exec bash "$(dirname -- "${BASH_SOURCE[0]}")/with-test-environment.sh" "$@"
        ;;
    *)
        exec "$@"
        ;;
esac
