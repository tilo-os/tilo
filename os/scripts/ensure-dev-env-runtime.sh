#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# Give a dev image an env-runtime, or fail its build.
#
#   ensure-dev-env-runtime.sh SOURCE INSTALL_BIN
#
#   SOURCE       a sealed env-runtime directory in install-env-runtime.sh's
#                layout, or an empty one: the runtime the same t run built
#                from this tree's os/Containerfile.env-runtime and sealed with
#                a throwaway key (tools/ci/publish/env-runtime.sh dev)
#   INSTALL_BIN  tilo-install
#
# A release image ships the published, stable-signed runtime of its
# definition, and none until stable has published one. A dev image boots the
# tree under test, whose definition stable may not have published yet; without
# a runtime no environment, and so no Terminal shell, can start in it. When the
# image under it ships no runtime, this installs SOURCE's into the system store
# the way install-env-runtime.sh installs a published one. Its throwaway
# publisher is recorded only in this dev image's store: release images and
# their trust are untouched. A dev image that ends with no env-runtime fails
# the build here.
# ---------------------------------------------------------------------------
set -euo pipefail

msg() { printf '[ensure-dev-env-runtime] %s\n' "$*" >&2; }
die() { msg "ERROR: $*"; exit 1; }

[ "$#" -eq 2 ] || die "usage: $0 SOURCE INSTALL_BIN"
source_dir=$1
install_bin=$2
store=/usr/share/tilo-apps
here=$(dirname -- "${BASH_SOURCE[0]}")

# The env-runtimes the system store holds, as `tilo-install image list` names them.
runtimes() {
    local home
    home=$(mktemp -d)
    HOME=$home XDG_DATA_HOME=$home/data "$install_bin" image list | awk '$2 == "env-runtime"'
    rm -rf "$home"
}

held=$(runtimes)
if [ -n "$held" ]; then
    msg "the image ships an env-runtime: $held"
    exit 0
fi
[ -f "$source_dir/env-runtime-$(uname -m).tapp" ] \
    || die "the image ships no env-runtime and no runtime of this run was given ($source_dir): \
a dev image without one starts no environment"
TILO_REQUIRE_ENV_RUNTIME=1 bash "$here/install-env-runtime.sh" \
    "$source_dir" "$store" "$store" "$install_bin"
held=$(runtimes)
[ -n "$held" ] || die "installed the run's env-runtime, but the system store lists none"
msg "the dev image ships this run's env-runtime: $held"
